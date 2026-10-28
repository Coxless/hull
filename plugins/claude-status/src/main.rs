//! 標準プラグイン: Claude Code の状態。
//!
//! 出力を流しながら、Claude Code が「実行中」か「入力待ち」かを追い続け、窓の題名の印と
//! デスクトップの通知に出す。ある時点の文字を一度だけ外のコマンドに渡す拡張では作れないものの例。
//!
//! 手がかりは次の 3 つ。tmux の中でも届くベルを主にする。
//!
//! - ベル、OSC 9 / 777 の通知: 入力待ち (Claude Code は通知をベルで出すように設定できる)
//! - 題名の先頭が回る記号 (⠋ や ✶ など): 実行中 (Claude Code は作業中に題名の記号を回す)
//! - 実行中のあとで、題名の先頭が ✳ に戻った: 終わって入力待ち
//!
//! 窓が前にないときに入力待ちになったら、通知を出して窓に注意を向けさせる。窓が前に来たら印を外す。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hull_plugin::{Action, Client, HostEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Running,
    Waiting,
}

/// 通知を出す間隔の下限 (ベルが続けて鳴っても、通知は 1 回にまとめる)。
const NOTIFY_INTERVAL: Duration = Duration::from_secs(10);

/// 作業中に題名の先頭で回る記号。
fn is_spinner(c: char) -> bool {
    matches!(c as u32, 0x2800..=0x28ff) || matches!(c, '·' | '✢' | '✶' | '✻' | '✽' | '✺' | '*')
}

/// 状態が変わったときに取る行動。
#[derive(Debug, PartialEq)]
enum Effect {
    Mark(Option<&'static str>),
    Notify(String),
    Attention,
}

struct Tracker {
    state: State,
    focused: bool,
    last_notify: Option<Instant>,
}

impl Tracker {
    fn new() -> Tracker {
        Tracker { state: State::Idle, focused: true, last_notify: None }
    }

    fn mark(&self) -> Option<&'static str> {
        match self.state {
            State::Idle => None,
            State::Running => Some("⏳"),
            State::Waiting if self.focused => None,
            State::Waiting => Some("🔔"),
        }
    }

    fn set(&mut self, state: State, body: Option<String>, now: Instant) -> Vec<Effect> {
        let before = self.mark();
        let was = self.state;
        self.state = state;
        let mut effects = Vec::new();
        if self.mark() != before {
            effects.push(Effect::Mark(self.mark()));
        }
        let quiet = self.last_notify.is_some_and(|t| now - t < NOTIFY_INTERVAL);
        if state == State::Waiting
            && !self.focused
            && (was != State::Waiting || body.is_some())
            && !quiet
        {
            self.last_notify = Some(now);
            effects.push(Effect::Notify(body.unwrap_or_else(|| "入力を待っています".into())));
            effects.push(Effect::Attention);
        }
        effects
    }

    fn on_event(&mut self, event: &HostEvent, now: Instant) -> Vec<Effect> {
        match event {
            HostEvent::Bell => self.set(State::Waiting, None, now),
            HostEvent::Notify { title, body } => {
                let text = match title {
                    Some(t) if !t.is_empty() => format!("{t}: {body}"),
                    _ => body.clone(),
                };
                self.set(State::Waiting, Some(text), now)
            }
            HostEvent::Title { title } => match title.chars().next() {
                Some(c) if is_spinner(c) => self.set(State::Running, None, now),
                Some('✳') if self.state == State::Running => self.set(State::Waiting, None, now),
                _ => Vec::new(),
            },
            HostEvent::Focus { focused } => {
                let before = self.mark();
                self.focused = *focused;
                if *focused && self.state == State::Waiting {
                    // 前に来たら、見たものとして印を外す。
                    self.state = State::Idle;
                }
                if self.mark() != before { vec![Effect::Mark(self.mark())] } else { Vec::new() }
            }
            _ => Vec::new(),
        }
    }
}

fn notify(body: &str) {
    let spawned = Command::new("notify-send")
        .args(["--app-name=hull", "--icon=utilities-terminal", "Claude Code", body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = spawned {
        std::thread::spawn(move || child.wait());
    }
}

fn main() -> std::io::Result<()> {
    let mut client = Client::connect()?;
    let mut tracker = Tracker::new();
    while let Some(event) = client.next_event()? {
        for effect in tracker.on_event(&event, Instant::now()) {
            match effect {
                Effect::Mark(mark) => {
                    client.send(&Action::TitleMark { mark: mark.map(str::to_string) })?
                }
                Effect::Notify(body) => notify(&body),
                Effect::Attention => client.send(&Action::Attention)?,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(t: &str) -> HostEvent {
        HostEvent::Title { title: t.into() }
    }

    #[test]
    fn 回る記号で実行中_星に戻ると入力待ち() {
        let mut t = Tracker::new();
        let now = Instant::now();
        assert_eq!(t.on_event(&title("⠋ Fix the bug"), now), [Effect::Mark(Some("⏳"))]);
        assert_eq!(t.on_event(&title("⠙ Fix the bug"), now), []);
        // 前にあるので、通知は出さず印も外す。
        assert_eq!(t.on_event(&title("✳ Fix the bug"), now), [Effect::Mark(None)]);
        assert_eq!(t.state, State::Waiting);
    }

    #[test]
    fn 後ろにあるときのベルで通知する() {
        let mut t = Tracker::new();
        let now = Instant::now();
        t.on_event(&HostEvent::Focus { focused: false }, now);
        let effects = t.on_event(&HostEvent::Bell, now);
        assert_eq!(
            effects,
            [
                Effect::Mark(Some("🔔")),
                Effect::Notify("入力を待っています".into()),
                Effect::Attention
            ]
        );
        // 続けて鳴っても、通知は 1 回にまとめる。
        assert_eq!(t.on_event(&HostEvent::Bell, now + Duration::from_secs(1)), []);
        // 前に来たら印を外す。
        assert_eq!(t.on_event(&HostEvent::Focus { focused: true }, now), [Effect::Mark(None)]);
    }

    #[test]
    fn 通知の本文を使う() {
        let mut t = Tracker::new();
        let now = Instant::now();
        t.on_event(&HostEvent::Focus { focused: false }, now);
        let effects = t.on_event(
            &HostEvent::Notify { title: None, body: "Claude needs your permission".into() },
            now,
        );
        assert!(effects.contains(&Effect::Notify("Claude needs your permission".into())));
    }
}

//! プラグインの起動と、やりとり。
//!
//! - 設定に書いたプラグインだけを起動する。落ちたら間をあけて起動し直す
//! - プラグインには解析済みのイベントだけを渡し、本体はプラグインを待たない
//! - プラグインごとに上限つきの列を持ち、あふれたら古いものから捨てて、捨てた数を知らせる

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::calloop::{
    Interest, LoopHandle, Mode, PostAction, RegistrationToken,
    generic::Generic,
    timer::{TimeoutAction, Timer},
};

pub use hull_plugin::{Action, HostEvent, TextResult, TextSource};
use hull_term::input::Mods;
use hull_term::{Event, PromptMark, Terminal};

use crate::app::{App, Clipboard};
use crate::config::{PluginConfig, parse_rgb};
use crate::render::{Highlight, Overlay};

/// プラグインごとの列の上限 (行の数)。
const QUEUE_LIMIT: usize = 1024;
/// 1 行の上限。これより長い行を送ってくるプラグインは切る。
const LINE_LIMIT: usize = 16 << 20;
/// 起動し直すのを諦めるまでの回数 (1 分あたり)。
const MAX_RESTARTS: usize = 5;
const MAX_PROMPTS: usize = 2000;

const DEFAULT_OVERLAY_FG: [u8; 3] = [0x00, 0x00, 0x00];
const DEFAULT_OVERLAY_BG: [u8; 3] = [0xf6, 0xd3, 0x2d];

/// プラグインへの上限つきの列。
#[derive(Default)]
struct Queue {
    out: VecDeque<Vec<u8>>,
    /// 書きかけの行の残り。
    partial: Vec<u8>,
    /// あふれて捨てた数。次に書くときに知らせる。
    dropped: u64,
}

impl Queue {
    fn push(&mut self, event: &HostEvent) {
        let Ok(mut line) = serde_json::to_vec(event) else { return };
        line.push(b'\n');
        if self.out.len() >= QUEUE_LIMIT {
            self.out.pop_front();
            self.dropped += 1;
        }
        self.out.push_back(line);
    }

    fn is_empty(&self) -> bool {
        self.out.is_empty() && self.partial.is_empty()
    }

    /// 書けるだけ書く。書ききったら `true`。
    fn flush(&mut self, w: &mut impl Write) -> bool {
        loop {
            if self.partial.is_empty() {
                if self.dropped > 0 {
                    let notice = HostEvent::Dropped { count: self.dropped };
                    self.dropped = 0;
                    if let Ok(mut line) = serde_json::to_vec(&notice) {
                        line.push(b'\n');
                        self.out.push_front(line);
                    }
                }
                match self.out.pop_front() {
                    Some(line) => self.partial = line,
                    None => return true,
                }
            }
            match w.write(&self.partial) {
                Ok(0) => return false,
                Ok(n) => {
                    self.partial.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return false,
                // 相手が閉じた。読み出し側で気づいて片づける。
                Err(_) => {
                    self.partial.clear();
                    self.out.clear();
                    return true;
                }
            }
        }
    }
}

struct Running {
    child: Child,
    stream: UnixStream,
    read_token: RegistrationToken,
    write_token: Option<RegistrationToken>,
    read_buf: Vec<u8>,
    queue: Queue,
}

struct Plugin {
    config: PluginConfig,
    running: Option<Running>,
    restarts: VecDeque<Instant>,
    restart_timer: Option<RegistrationToken>,
    highlights: HashMap<String, (Vec<hull_plugin::Range>, [u8; 3])>,
    overlays: HashMap<String, Vec<hull_plugin::OverlayItem>>,
    title_mark: Option<String>,
}

#[derive(Default)]
pub struct PluginHost {
    plugins: Vec<Plugin>,
    /// キー入力を受け取っているプラグイン。
    grabber: Option<String>,
    /// プロンプトの始まりの行 (OSC 133;A)。
    prompts: VecDeque<u64>,
    size: (usize, usize),
}

/// 端末のイベントを、プラグインに渡す形にする。
pub fn event_from_term(event: &Event) -> Option<HostEvent> {
    Some(match event {
        Event::Bell => HostEvent::Bell,
        Event::Title(title) => HostEvent::Title { title: title.clone() },
        Event::Cwd(path) => HostEvent::Cwd { path: path.clone() },
        Event::Notify { title, body } => {
            HostEvent::Notify { title: title.clone(), body: body.clone() }
        }
        Event::Progress { state, value } => HostEvent::Progress { state: *state, value: *value },
        Event::Prompt(mark) => {
            let (mark, exit_code) = match mark {
                PromptMark::PromptStart => (hull_plugin::PromptMark::PromptStart, None),
                PromptMark::CommandStart => (hull_plugin::PromptMark::CommandStart, None),
                PromptMark::OutputStart => (hull_plugin::PromptMark::OutputStart, None),
                PromptMark::CommandEnd(code) => (hull_plugin::PromptMark::CommandEnd, *code),
            };
            HostEvent::Prompt { mark, exit_code }
        }
        Event::Clipboard { .. } => return None,
    })
}

pub fn mods_list(mods: Mods) -> Vec<String> {
    let mut out = Vec::new();
    for (on, name) in
        [(mods.ctrl, "ctrl"), (mods.shift, "shift"), (mods.alt, "alt"), (mods.logo, "super")]
    {
        if on {
            out.push(name.to_string());
        }
    }
    out
}

fn spawn(config: &PluginConfig) -> io::Result<(Child, UnixStream)> {
    let (ours, theirs) = UnixStream::pair()?;
    ours.set_nonblocking(true)?;
    let their_fd = theirs.as_raw_fd();
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args)
        .env("HULL_PLUGIN_FD", "3")
        .env("HULL_PLUGIN_NAME", &config.name)
        .env("HULL_VERSION", env!("CARGO_PKG_VERSION"))
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    // SAFETY: fork と exec のあいだで、async-signal-safe な呼び出しだけを使う。
    unsafe {
        cmd.pre_exec(move || {
            // ソケットを fd 3 に置く。dup2 は close-on-exec を引き継がない。
            if their_fd == 3 {
                libc::fcntl(3, libc::F_SETFD, 0);
            } else if libc::dup2(their_fd, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            // hull が落ちたら、プラグインも終わらせる。
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    drop(theirs);
    Ok((child, ours))
}

impl PluginHost {
    pub fn grabbing(&self) -> bool {
        self.grabber.is_some()
    }

    fn find(&mut self, name: &str) -> Option<&mut Plugin> {
        self.plugins.iter_mut().find(|p| p.config.name == name)
    }

    fn enqueue(plugin: &mut Plugin, event: &HostEvent) {
        if let Some(run) = &mut plugin.running {
            run.queue.push(event);
        }
    }

    /// すべてのプラグインに送る。
    pub fn broadcast(&mut self, event: HostEvent) {
        for plugin in &mut self.plugins {
            Self::enqueue(plugin, &event);
        }
        self.flush_all();
    }

    pub fn send_to(&mut self, name: &str, event: HostEvent) {
        if let Some(plugin) = self.find(name) {
            Self::enqueue(plugin, &event);
        }
        self.flush_all();
    }

    /// キー入力を、受け取っているプラグインに送る。
    pub fn send_key(&mut self, event: HostEvent) {
        if let Some(name) = self.grabber.clone() {
            self.send_to(&name, event);
        }
    }

    fn flush_all(&mut self) {
        for plugin in &mut self.plugins {
            if let Some(run) = &mut plugin.running {
                run.queue.flush(&mut run.stream);
            }
        }
    }

    pub fn title_mark(&self) -> Option<String> {
        let marks: Vec<&str> =
            self.plugins.iter().filter_map(|p| p.title_mark.as_deref()).collect();
        (!marks.is_empty()).then(|| marks.join(" "))
    }

    pub fn note_prompt(&mut self, line: u64) {
        if self.prompts.back() == Some(&line) {
            return;
        }
        self.prompts.push_back(line);
        if self.prompts.len() > MAX_PROMPTS {
            self.prompts.pop_front();
        }
    }

    /// `from` より前 (`up`) か後のプロンプトの行。
    pub fn prompt_line(&self, from: u64, up: bool) -> Option<u64> {
        if up {
            self.prompts.iter().rev().find(|&&l| l < from).copied()
        } else {
            self.prompts.iter().find(|&&l| l > from).copied()
        }
    }

    /// 見えている範囲のハイライトとオーバーレイ。
    pub fn visible_decorations(&self, term: &Terminal) -> (Vec<Highlight>, Vec<Overlay>) {
        let rows = term.rows();
        let top = term.visible_abs(0);
        let row_of =
            |line: u64| (line >= top && line < top + rows as u64).then(|| (line - top) as usize);
        let mut highlights = Vec::new();
        let mut overlays = Vec::new();
        for plugin in &self.plugins {
            for (ranges, color) in plugin.highlights.values() {
                for r in ranges {
                    if let Some(row) = row_of(r.line) {
                        highlights.push(Highlight {
                            row,
                            start: r.start,
                            end: r.end,
                            color: *color,
                        });
                    }
                }
            }
            for items in plugin.overlays.values() {
                for item in items {
                    let row = match (item.row, item.line) {
                        (Some(r), _) if r < 0 => rows.checked_sub(r.unsigned_abs() as usize),
                        (Some(r), _) => Some(r as usize).filter(|&r| r < rows),
                        (None, Some(line)) => row_of(line),
                        (None, None) => None,
                    };
                    let Some(row) = row else { continue };
                    overlays.push(Overlay {
                        row,
                        col: item.col,
                        text: item.text.clone(),
                        fg: item.fg.as_deref().and_then(parse_rgb).unwrap_or(DEFAULT_OVERLAY_FG),
                        bg: item.bg.as_deref().and_then(parse_rgb).unwrap_or(DEFAULT_OVERLAY_BG),
                    });
                }
            }
        }
        // ハイライトは、あとから付けたものほど上に。並びを安定させる。
        highlights.sort_by_key(|h| (h.row, h.start));
        (highlights, overlays)
    }

    pub fn start_all(
        &mut self,
        configs: &[PluginConfig],
        handle: &LoopHandle<'static, App>,
        size: (usize, usize),
    ) {
        self.size = size;
        for config in configs {
            if self.plugins.iter().any(|p| p.config.name == config.name) {
                eprintln!(
                    "hull: プラグイン {} が 2 回書かれています。最初のものだけを使います",
                    config.name
                );
                continue;
            }
            self.plugins.push(Plugin {
                config: config.clone(),
                running: None,
                restarts: VecDeque::new(),
                restart_timer: None,
                highlights: HashMap::new(),
                overlays: HashMap::new(),
                title_mark: None,
            });
            self.start(&config.name, handle);
        }
    }

    /// 設定が変わったら、消えたプラグインを止め、増えたものと変わったものを起動する。
    pub fn reconcile(
        &mut self,
        configs: &[PluginConfig],
        handle: &LoopHandle<'static, App>,
        size: (usize, usize),
    ) {
        let mut keep = Vec::new();
        for mut plugin in std::mem::take(&mut self.plugins) {
            if configs.contains(&plugin.config) {
                keep.push(plugin);
            } else {
                stop(&mut plugin, handle);
                if self.grabber.as_deref() == Some(plugin.config.name.as_str()) {
                    self.grabber = None;
                }
            }
        }
        self.plugins = keep;
        let new: Vec<PluginConfig> = configs
            .iter()
            .filter(|c| !self.plugins.iter().any(|p| p.config == **c))
            .cloned()
            .collect();
        self.start_all(&new, handle, size);
    }

    fn start(&mut self, name: &str, handle: &LoopHandle<'static, App>) {
        let size = self.size;
        let Some(plugin) = self.find(name) else { return };
        let (child, stream) = match spawn(&plugin.config) {
            Ok(x) => x,
            Err(e) => {
                eprintln!(
                    "hull: プラグイン {} ({}) を起動できません: {e}",
                    plugin.config.name, plugin.config.command
                );
                return;
            }
        };
        let Ok(dup) = stream.try_clone() else { return };
        let owned = OwnedFd::from(dup);
        let source_name = name.to_string();
        let token = handle.insert_source(
            Generic::new(owned, Interest::READ, Mode::Level),
            move |_, _, app: &mut App| Ok(app.on_plugin_readable(&source_name)),
        );
        let Ok(read_token) = token else { return };
        plugin.running = Some(Running {
            child,
            stream,
            read_token,
            write_token: None,
            read_buf: Vec::new(),
            queue: Queue::default(),
        });
        Self::enqueue(
            plugin,
            &HostEvent::Hello {
                version: hull_plugin::PROTOCOL_VERSION,
                cols: size.0,
                rows: size.1,
            },
        );
        self.flush_all();
    }

    /// 書ききれなかったプラグインの書き込みを、書けるようになったら続ける。
    pub fn ensure_write_watch(&mut self, handle: &LoopHandle<'static, App>) {
        for plugin in &mut self.plugins {
            let name = plugin.config.name.clone();
            let Some(run) = &mut plugin.running else { continue };
            if run.queue.is_empty() || run.write_token.is_some() {
                continue;
            }
            let Ok(dup) = run.stream.try_clone() else { continue };
            let token = handle.insert_source(
                Generic::new(OwnedFd::from(dup), Interest::WRITE, Mode::Level),
                move |_, _, app: &mut App| {
                    let Some(plugin) = app.plugins.find(&name) else {
                        return Ok(PostAction::Remove);
                    };
                    let Some(run) = &mut plugin.running else { return Ok(PostAction::Remove) };
                    if run.queue.flush(&mut run.stream) {
                        run.write_token = None;
                        return Ok(PostAction::Remove);
                    }
                    Ok(PostAction::Continue)
                },
            );
            run.write_token = token.ok();
        }
    }
}

fn stop(plugin: &mut Plugin, handle: &LoopHandle<'static, App>) {
    teardown(plugin, handle, true);
    if let Some(t) = plugin.restart_timer.take() {
        handle.remove(t);
    }
}

/// プロセスを止め、プラグインが画面に付けたものを消す。読み出しの監視は、読み出しの中から呼ぶときは
/// 呼び出し元が外すので、`remove_read` が偽なら残す。
fn teardown(plugin: &mut Plugin, handle: &LoopHandle<'static, App>, remove_read: bool) {
    if let Some(mut run) = plugin.running.take() {
        if remove_read {
            handle.remove(run.read_token);
        }
        if let Some(t) = run.write_token {
            handle.remove(t);
        }
        let _ = run.child.kill();
        let _ = run.child.wait();
    }
    plugin.highlights.clear();
    plugin.overlays.clear();
    plugin.title_mark = None;
}

/// `id` のものを消す。`None` ならすべて消す。
fn clear_by_id<V>(map: &mut HashMap<String, V>, id: Option<String>) {
    match id {
        Some(id) => {
            map.remove(&id);
        }
        None => map.clear(),
    }
}

impl App {
    /// プラグインから届いた行を読み、操作を行う。
    fn on_plugin_readable(&mut self, name: &str) -> PostAction {
        let mut actions = Vec::new();
        let mut closed = false;
        {
            let Some(plugin) = self.plugins.find(name) else { return PostAction::Remove };
            let Some(run) = &mut plugin.running else { return PostAction::Remove };
            let mut buf = [0u8; 65536];
            loop {
                match run.stream.read(&mut buf) {
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => run.read_buf.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        closed = true;
                        break;
                    }
                }
            }
            let mut cursor = io::Cursor::new(std::mem::take(&mut run.read_buf));
            let mut line = Vec::new();
            loop {
                line.clear();
                match cursor.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.last() != Some(&b'\n') => {
                        // 行の途中までしか届いていない。
                        run.read_buf = std::mem::take(&mut line);
                        break;
                    }
                    Ok(_) => match serde_json::from_slice::<Action>(&line) {
                        Ok(action) => actions.push(action),
                        Err(e) => {
                            eprintln!("hull: プラグイン {name} から読めない操作が届きました: {e}")
                        }
                    },
                }
            }
            if run.read_buf.len() > LINE_LIMIT {
                eprintln!("hull: プラグイン {name} の行が長すぎるので切ります");
                closed = true;
            }
        }
        for action in actions {
            self.apply_plugin_action(name, action);
        }
        let handle = self.handle.clone();
        self.plugins.ensure_write_watch(&handle);
        if closed {
            self.plugin_exited(name);
            return PostAction::Remove;
        }
        PostAction::Continue
    }

    fn plugin_exited(&mut self, name: &str) {
        let handle = self.handle.clone();
        if self.plugins.grabber.as_deref() == Some(name) {
            self.plugins.grabber = None;
        }
        let Some(plugin) = self.plugins.find(name) else { return };
        teardown(plugin, &handle, false);

        // 間をあけて起動し直す。1 分に何度も落ちるなら諦める。
        let now = Instant::now();
        plugin.restarts.retain(|t| now - *t < Duration::from_secs(60));
        if plugin.restarts.len() >= MAX_RESTARTS {
            eprintln!("hull: プラグイン {name} が何度も終わるので、起動し直すのをやめます");
        } else {
            let delay = Duration::from_secs(1 << plugin.restarts.len().min(4));
            plugin.restarts.push_back(now);
            let restart_name = name.to_string();
            plugin.restart_timer = handle
                .insert_source(Timer::from_duration(delay), move |_, _, app: &mut App| {
                    let handle = app.handle.clone();
                    if let Some(p) = app.plugins.find(&restart_name) {
                        p.restart_timer = None;
                    }
                    app.plugins.start(&restart_name, &handle);
                    TimeoutAction::Drop
                })
                .ok();
        }
        self.refresh_title();
        self.request_draw();
    }

    fn apply_plugin_action(&mut self, name: &str, action: Action) {
        match action {
            Action::Write { data } => {
                self.write_pty(data.as_bytes());
            }
            Action::GrabKeys { grab } => {
                if grab {
                    self.plugins.grabber = Some(name.to_string());
                } else if self.plugins.grabber.as_deref() == Some(name) {
                    self.plugins.grabber = None;
                }
            }
            Action::ForwardKey { key, text, mods } => self.forward_key(&key, &text, &mods),
            Action::ReadText { id, source } => {
                let text = read_text(&self.term, source);
                self.plugins.send_to(name, HostEvent::Text { id, text });
            }
            Action::Highlight { id, ranges, color } => {
                let color = parse_rgb(&color).unwrap_or(DEFAULT_OVERLAY_BG);
                if let Some(p) = self.plugins.find(name) {
                    p.highlights.insert(id, (ranges, color));
                }
                self.request_draw();
            }
            Action::ClearHighlight { id } => {
                if let Some(p) = self.plugins.find(name) {
                    clear_by_id(&mut p.highlights, id);
                }
                self.request_draw();
            }
            Action::Overlay { id, items } => {
                if let Some(p) = self.plugins.find(name) {
                    p.overlays.insert(id, items);
                }
                self.request_draw();
            }
            Action::ClearOverlay { id } => {
                if let Some(p) = self.plugins.find(name) {
                    clear_by_id(&mut p.overlays, id);
                }
                self.request_draw();
            }
            Action::ScrollTo { line } => {
                match line {
                    Some(line) => self.term.scroll_to_line(line),
                    None => self.term.scroll_to_bottom(),
                }
                self.request_draw();
            }
            Action::Attention => self.request_attention(),
            Action::Clipboard { text, primary } => {
                let target = if primary { Clipboard::Primary } else { Clipboard::Clipboard };
                self.set_clipboard(target, text);
            }
            Action::OpenUrl { url } => crate::app::open_url(&url),
            Action::TitleMark { mark } => {
                if let Some(p) = self.plugins.find(name) {
                    p.title_mark = mark.filter(|m| !m.is_empty());
                }
                self.refresh_title();
            }
        }
    }
}

fn read_text(term: &Terminal, source: TextSource) -> TextResult {
    let grid = term.grid();
    let view_top = term.visible_abs(0);
    let (first, end) = match source {
        TextSource::Screen => (view_top, view_top + term.rows() as u64),
        TextSource::All => (grid.first_abs(), grid.end_abs()),
    };
    let lines = (first..end).map(|abs| term.line_text(abs).unwrap_or_default()).collect();
    TextResult {
        first_line: first,
        lines,
        view_top,
        cols: term.cols(),
        rows: term.rows(),
        alt_screen: term.is_alt_screen(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 画面の文字を読む() {
        let mut term = Terminal::new(10, 3, 100);
        term.advance(b"a\r\nb\r\nc\r\nd");
        let screen = read_text(&term, TextSource::Screen);
        assert_eq!(screen.lines, ["b", "c", "d"]);
        assert_eq!(screen.first_line, 1);
        let all = read_text(&term, TextSource::All);
        assert_eq!(all.lines, ["a", "b", "c", "d"]);
        assert_eq!(all.first_line, 0);
    }

    #[test]
    fn 列があふれたら古いものから捨てて数を知らせる() {
        let mut queue = Queue::default();
        for i in 0..QUEUE_LIMIT + 5 {
            queue.push(&HostEvent::Title { title: i.to_string() });
        }
        let mut out = Vec::new();
        assert!(queue.flush(&mut out));
        let lines: Vec<HostEvent> = out
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        assert_eq!(lines[0], HostEvent::Dropped { count: 5 });
        assert_eq!(lines[1], HostEvent::Title { title: "5".into() });
        assert_eq!(lines.len(), QUEUE_LIMIT + 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn 修飾キーの名前() {
        let mods = Mods { ctrl: true, shift: true, ..Mods::default() };
        assert_eq!(mods_list(mods), ["ctrl", "shift"]);
    }
}

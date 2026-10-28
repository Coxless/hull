//! 標準プラグイン: 検索。
//!
//! `plugin:search/search` のキーバインドで開く。画面の一番下に入力欄を出し、打つたびに
//! 一致した所をハイライトして、選んでいる所までスクロールする。
//! tmux の中ではスクロールバックを tmux が持つので、見えている画面とその上に残っている分だけを探す。
//!
//! - Enter / ↑ / Ctrl+P: 前 (上) の一致へ
//! - Shift+Enter / ↓ / Ctrl+N: 次 (下) の一致へ
//! - Esc: 閉じる (一番下に戻る)
//! - 小文字だけで打つと、大文字と小文字を区別しない

use hull_plugin::{Action, Client, HostEvent, OverlayItem, Range, TextResult, TextSource, width};

/// 送るハイライトの上限 (多すぎると重くなる)。
const MAX_MATCHES: usize = 10_000;
const MATCH_COLOR: &str = "#f6d32d";
const CURRENT_COLOR: &str = "#ff7800";

#[derive(Default)]
struct Search {
    open: bool,
    query: String,
    text: TextResult,
    /// `text` の行を小文字にしたもの (打つたびに作り直さないよう、開いたときに作る)。
    lower: Vec<String>,
    matches: Vec<Range>,
    current: Option<usize>,
    cols: usize,
}

/// 行を小文字にする。小文字にすると長さが変わる文字を含む行は、元のまま (バイトの位置がずれるため)。
fn lowercase_lines(text: &TextResult) -> Vec<String> {
    text.lines
        .iter()
        .map(|line| {
            let lower = line.to_lowercase();
            if lower.len() == line.len() { lower } else { line.clone() }
        })
        .collect()
}

fn find_matches(text: &TextResult, lower: &[String], query: &str) -> Vec<Range> {
    if query.is_empty() {
        return Vec::new();
    }
    // 小文字だけなら、大文字と小文字を区別しない。
    let fold = !query.chars().any(char::is_uppercase);
    let needle = if fold { query.to_lowercase() } else { query.to_string() };
    let mut out = Vec::new();
    for (i, line) in text.lines.iter().enumerate() {
        let hay = if fold { &lower[i] } else { line };
        let mut from = 0;
        while let Some(pos) = hay[from..].find(&needle) {
            let start = from + pos;
            let end = start + needle.len();
            out.push(text.range(i, start, end));
            if out.len() >= MAX_MATCHES {
                return out;
            }
            from = end;
        }
    }
    out
}

impl Search {
    fn open(&mut self, client: &mut Client) -> std::io::Result<()> {
        self.open = true;
        self.query.clear();
        self.matches.clear();
        self.current = None;
        client.send(&Action::GrabKeys { grab: true })?;
        self.text = client.read_text(TextSource::All)?;
        self.lower = lowercase_lines(&self.text);
        self.cols = self.text.cols;
        self.render(client)
    }

    fn close(&mut self, client: &mut Client) -> std::io::Result<()> {
        self.open = false;
        client.send(&Action::ClearOverlay { id: None })?;
        client.send(&Action::ClearHighlight { id: None })?;
        client.send(&Action::GrabKeys { grab: false })?;
        client.send(&Action::ScrollTo { line: None })
    }

    fn update(&mut self) {
        self.matches = find_matches(&self.text, &self.lower, &self.query);
        // 一番新しい (下の) 一致から始める。
        self.current = self.matches.len().checked_sub(1);
    }

    fn step(&mut self, up: bool) {
        let n = self.matches.len();
        if n == 0 {
            return;
        }
        self.current = Some(match self.current {
            Some(i) if up => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None => n - 1,
        });
    }

    fn render(&self, client: &mut Client) -> std::io::Result<()> {
        let status = match (self.current, self.matches.len()) {
            (_, 0) if self.query.is_empty() => String::new(),
            (_, 0) => "見つかりません".to_string(),
            (Some(i), n) => format!("{}/{}{}", i + 1, n, if n >= MAX_MATCHES { "+" } else { "" }),
            (None, n) => n.to_string(),
        };
        let help = "Enter:前 Shift+Enter:次 Esc:閉じる";
        let mut bar = format!(" 検索: {}▏ {status}", self.query);
        let room = self.cols.saturating_sub(width(&bar) + width(help) + 1);
        bar.push_str(&" ".repeat(room));
        bar.push_str(help);
        bar.push(' ');
        client.send(&Action::Overlay {
            id: "bar".into(),
            items: vec![OverlayItem {
                row: Some(-1),
                col: 0,
                text: bar,
                fg: Some("#000000".into()),
                bg: Some(MATCH_COLOR.into()),
                ..OverlayItem::default()
            }],
        })?;
        let (current, others): (Vec<_>, Vec<_>) =
            self.matches.iter().enumerate().partition(|(i, _)| Some(*i) == self.current);
        client.send(&Action::Highlight {
            id: "matches".into(),
            ranges: others.into_iter().map(|(_, r)| *r).collect(),
            color: MATCH_COLOR.into(),
        })?;
        client.send(&Action::Highlight {
            id: "current".into(),
            ranges: current.iter().map(|(_, r)| **r).collect(),
            color: CURRENT_COLOR.into(),
        })?;
        if let Some(r) = self.current.map(|i| self.matches[i]) {
            client.send(&Action::ScrollTo { line: Some(r.line) })?;
        }
        Ok(())
    }

    fn key(
        &mut self,
        client: &mut Client,
        key: &str,
        text: &str,
        mods: &[String],
    ) -> std::io::Result<()> {
        let ctrl = mods.iter().any(|m| m == "ctrl");
        let shift = mods.iter().any(|m| m == "shift");
        match (key, ctrl) {
            ("escape", _) | ("g", true) | ("c", true) => return self.close(client),
            ("enter", _) if shift => self.step(false),
            ("enter", _) | ("up", _) | ("p", true) => self.step(true),
            ("down", _) | ("n", true) => self.step(false),
            ("backspace", _) => {
                self.query.pop();
                self.update();
            }
            ("u", true) => {
                self.query.clear();
                self.update();
            }
            (_, false) if !text.is_empty() && !text.chars().any(char::is_control) => {
                self.query.push_str(text);
                self.update();
            }
            _ => return Ok(()),
        }
        self.render(client)
    }
}

fn main() -> std::io::Result<()> {
    let mut client = Client::connect()?;
    let mut search = Search::default();
    while let Some(event) = client.next_event()? {
        match event {
            HostEvent::Hello { cols, .. } | HostEvent::Resize { cols, .. } => {
                search.cols = cols;
                if search.open {
                    search.render(&mut client)?;
                }
            }
            HostEvent::Keybind { name } if name == "search" => {
                if search.open {
                    search.close(&mut client)?;
                } else {
                    search.open(&mut client)?;
                }
            }
            HostEvent::Key { key, text, mods } if search.open => {
                search.key(&mut client, &key, &text, &mods)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[&str]) -> TextResult {
        TextResult {
            first_line: 10,
            lines: lines.iter().map(|s| s.to_string()).collect(),
            ..TextResult::default()
        }
    }

    fn find(t: &TextResult, query: &str) -> Vec<Range> {
        find_matches(t, &lowercase_lines(t), query)
    }

    #[test]
    fn 小文字だけなら大文字と小文字を区別しない() {
        let t = text(&["Error: x", "no error", "ERROR"]);
        assert_eq!(find(&t, "error").len(), 3);
        assert_eq!(find(&t, "Error").len(), 1);
    }

    #[test]
    fn 全角を含む行の列() {
        let t = text(&["日本語のerror"]);
        let m = find(&t, "error");
        assert_eq!(m, [Range { line: 10, start: 8, end: 13 }]);
    }

    #[test]
    fn 前と次は端で回る() {
        let mut s = Search { matches: find(&text(&["a a a"]), "a"), ..Search::default() };
        s.current = Some(0);
        s.step(true);
        assert_eq!(s.current, Some(2));
        s.step(false);
        assert_eq!(s.current, Some(0));
    }
}

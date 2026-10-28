//! 標準プラグイン: URL ヒント。
//!
//! `plugin:url-hints/open` (または `/copy`) のキーバインドで、見えている画面の URL に
//! 短いラベルを付ける。ラベルを打つと、その URL を開く (`copy` ならクリップボードに写す)。
//! Esc でやめる。

use hull_plugin::{Action, Client, HostEvent, OverlayItem, Range, TextSource, col_of};

const SCHEMES: &[&str] =
    &["https://", "http://", "file://", "ftp://", "ssh://", "git://", "mailto:"];
/// ラベルに使う文字 (ホームポジションから)。
const LABEL_CHARS: &str = "asdfjklghqweruiopzxcvbnmty";

#[derive(Clone, Debug, PartialEq)]
struct Hint {
    url: String,
    range: Range,
    label: String,
}

/// 行の中の URL (バイトの位置と文字列)。
fn find_urls(line: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while from < line.len() {
        let next = SCHEMES.iter().filter_map(|s| line[from..].find(s).map(|p| from + p)).min();
        let Some(start) = next else { break };
        let rest = &line[start..];
        let mut end = rest
            .char_indices()
            .find(|&(_, c)| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`' | '\'' | '│'))
            .map_or(rest.len(), |(i, _)| i);
        // 文の終わりの句読点や、対になっていない閉じかっこは URL に含めない。
        loop {
            let url = &rest[..end];
            let Some(last) = url.chars().last() else { break };
            let unbalanced = |open: char, close: char| {
                last == close && url.matches(close).count() > url.matches(open).count()
            };
            if matches!(last, '.' | ',' | ';' | ':' | '!' | '?')
                || unbalanced('(', ')')
                || unbalanced('[', ']')
            {
                end -= last.len_utf8();
            } else {
                break;
            }
        }
        let url = &rest[..end];
        let is_scheme_only = SCHEMES.contains(&url);
        if !is_scheme_only {
            out.push((start, url.to_string()));
        }
        from = start + end.max(1);
    }
    out
}

/// `n` 個のラベル。26 個までは 1 文字、それより多ければ 2 文字。
fn labels(n: usize) -> Vec<String> {
    let chars: Vec<char> = LABEL_CHARS.chars().collect();
    if n <= chars.len() {
        return chars.iter().take(n).map(|c| c.to_string()).collect();
    }
    let mut out = Vec::new();
    'outer: for a in &chars {
        for b in &chars {
            if out.len() == n {
                break 'outer;
            }
            out.push(format!("{a}{b}"));
        }
    }
    out
}

#[derive(Default)]
struct Hints {
    active: bool,
    copy: bool,
    hints: Vec<Hint>,
    typed: String,
}

impl Hints {
    fn start(&mut self, client: &mut Client, copy: bool) -> std::io::Result<()> {
        let text = client.read_text(TextSource::Screen)?;
        let mut found = Vec::new();
        for (i, line) in text.lines.iter().enumerate() {
            for (pos, url) in find_urls(line) {
                let range = Range {
                    line: text.first_line + i as u64,
                    start: col_of(line, pos),
                    end: col_of(line, pos + url.len()),
                };
                found.push((url, range));
            }
        }
        if found.is_empty() {
            return Ok(());
        }
        let labels = labels(found.len());
        self.hints = found
            .into_iter()
            .zip(labels)
            .map(|((url, range), label)| Hint { url, range, label })
            .collect();
        self.active = true;
        self.copy = copy;
        self.typed.clear();
        client.send(&Action::GrabKeys { grab: true })?;
        self.render(client)
    }

    fn stop(&mut self, client: &mut Client) -> std::io::Result<()> {
        self.active = false;
        client.send(&Action::ClearOverlay { id: None })?;
        client.send(&Action::ClearHighlight { id: None })?;
        client.send(&Action::GrabKeys { grab: false })
    }

    fn render(&self, client: &mut Client) -> std::io::Result<()> {
        let visible: Vec<&Hint> =
            self.hints.iter().filter(|h| h.label.starts_with(&self.typed)).collect();
        client.send(&Action::Highlight {
            id: "urls".into(),
            ranges: visible.iter().map(|h| h.range).collect(),
            color: "#99c1f1".into(),
        })?;
        let items = visible
            .iter()
            .map(|h| OverlayItem {
                line: Some(h.range.line),
                col: h.range.start,
                text: h.label[self.typed.len()..].to_string(),
                fg: Some("#000000".into()),
                bg: Some("#ff7800".into()),
                ..OverlayItem::default()
            })
            .collect();
        client.send(&Action::Overlay { id: "labels".into(), items })
    }

    fn key(&mut self, client: &mut Client, key: &str, text: &str) -> std::io::Result<()> {
        match key {
            "escape" => return self.stop(client),
            "backspace" => {
                self.typed.pop();
            }
            _ if text.chars().count() == 1 && LABEL_CHARS.contains(text) => {
                self.typed.push_str(text)
            }
            _ => return Ok(()),
        }
        let matching: Vec<&Hint> =
            self.hints.iter().filter(|h| h.label.starts_with(&self.typed)).collect();
        match matching.as_slice() {
            [] => return self.stop(client),
            [hint] if hint.label == self.typed => {
                let url = hint.url.clone();
                self.stop(client)?;
                return client.send(&if self.copy {
                    Action::Clipboard { text: url, primary: false }
                } else {
                    Action::OpenUrl { url }
                });
            }
            _ => {}
        }
        self.render(client)
    }
}

fn main() -> std::io::Result<()> {
    let mut client = Client::connect()?;
    let mut hints = Hints::default();
    while let Some(event) = client.next_event()? {
        match event {
            HostEvent::Keybind { name } if !hints.active => {
                hints.start(&mut client, name == "copy")?;
            }
            HostEvent::Keybind { .. } => hints.stop(&mut client)?,
            HostEvent::Key { key, text, .. } if hints.active => {
                hints.key(&mut client, &key, &text)?
            }
            // 画面が動いたら位置がずれるので、やめる。
            HostEvent::Resize { .. } if hints.active => hints.stop(&mut client)?,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_を見つける() {
        let found = find_urls("see https://example.com/a(b) and (http://x.org/y). done");
        let urls: Vec<&str> = found.iter().map(|(_, u)| u.as_str()).collect();
        assert_eq!(urls, ["https://example.com/a(b)", "http://x.org/y"]);
        assert_eq!(found[0].0, 4);
    }

    #[test]
    fn 全角の後ろの_url_の列() {
        let line = "詳しくは https://a.jp を見る";
        let (pos, url) = &find_urls(line)[0];
        assert_eq!(url, "https://a.jp");
        assert_eq!(col_of(line, *pos), 9);
    }

    #[test]
    fn スキームだけは_url_にしない() {
        assert!(find_urls("https:// only").is_empty());
    }

    #[test]
    fn ラベルは足りなければ2文字() {
        assert_eq!(labels(3), ["a", "s", "d"]);
        let many = labels(30);
        assert_eq!(many.len(), 30);
        assert!(many.iter().all(|l| l.len() == 2));
        let set: std::collections::HashSet<_> = many.iter().collect();
        assert_eq!(set.len(), 30);
    }
}

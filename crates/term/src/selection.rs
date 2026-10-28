//! マウスで選ぶ範囲。行は絶対番号で持つので、出力が流れても選んだ文字からずれない。

use crate::cell::flags;
use crate::term::Terminal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: u64,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// 1 文字ずつ (ドラッグ)。
    Simple,
    /// 単語ごと (ダブルクリック)。
    Word,
    /// 行ごと (トリプルクリック)。
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
    pub kind: SelectionKind,
}

/// 単語の区切りとみなす文字。URL やパスを 1 語として選べるよう、`/` `.` `-` `:` などは含めない。
const WORD_SEPARATORS: &str = " \t\"'`()[]{}<>|│,;";

impl Selection {
    pub fn new(at: Point, kind: SelectionKind) -> Selection {
        Selection { anchor: at, head: at, kind }
    }

    /// 選んでいる範囲の始めと終わり (両端を含む)。単語や行の単位に広げる。
    pub fn range(&self, term: &Terminal) -> (Point, Point) {
        let (mut start, mut end) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        match self.kind {
            SelectionKind::Simple => {}
            SelectionKind::Word => {
                start = word_start(term, start);
                end = word_end(term, end);
            }
            SelectionKind::Line => {
                start.col = 0;
                end.col = usize::MAX / 2;
            }
        }
        (start, end)
    }

    pub fn contains(&self, term: &Terminal, line: u64, col: usize) -> bool {
        let (start, end) = self.range(term);
        let p = Point { line, col };
        start <= p && p <= end
    }

    pub fn text(&self, term: &Terminal) -> String {
        let (start, end) = self.range(term);
        term.text_between((start.line, start.col), (end.line, end.col))
    }
}

fn char_at(term: &Terminal, p: Point) -> Option<char> {
    let row = term.grid().line(p.line)?;
    if p.col >= row.len() {
        return None;
    }
    let cell = row.cell(p.col);
    if cell.flags & flags::WIDE_SPACER != 0 && p.col > 0 {
        return Some(row.cell(p.col - 1).ch);
    }
    Some(cell.ch)
}

fn is_word(c: char) -> bool {
    !WORD_SEPARATORS.contains(c)
}

fn word_start(term: &Terminal, mut p: Point) -> Point {
    let Some(c) = char_at(term, p) else { return p };
    let word = is_word(c);
    while p.col > 0 {
        let prev = Point { line: p.line, col: p.col - 1 };
        match char_at(term, prev) {
            Some(c) if is_word(c) == word && c != ' ' => p = prev,
            _ => break,
        }
    }
    p
}

fn word_end(term: &Terminal, mut p: Point) -> Point {
    let Some(c) = char_at(term, p) else { return p };
    let word = is_word(c);
    loop {
        let next = Point { line: p.line, col: p.col + 1 };
        match char_at(term, next) {
            Some(c) if is_word(c) == word && c != ' ' => p = next,
            _ => break,
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 単語と行の選択() {
        let mut t = Terminal::new(30, 2, 10);
        t.advance(b"ls /usr/local/bin (x)");
        let line = t.grid().abs_of_screen(0);
        let word = Selection::new(Point { line, col: 6 }, SelectionKind::Word);
        assert_eq!(word.text(&t), "/usr/local/bin");
        let paren = Selection::new(Point { line, col: 19 }, SelectionKind::Word);
        assert_eq!(paren.text(&t), "x");
        let whole = Selection::new(Point { line, col: 3 }, SelectionKind::Line);
        assert_eq!(whole.text(&t), "ls /usr/local/bin (x)");
    }

    #[test]
    fn 逆向きのドラッグ() {
        let mut t = Terminal::new(10, 1, 10);
        t.advance(b"abcdef");
        let line = t.grid().abs_of_screen(0);
        let mut sel = Selection::new(Point { line, col: 4 }, SelectionKind::Simple);
        sel.head = Point { line, col: 1 };
        assert_eq!(sel.text(&t), "bcde");
        assert!(sel.contains(&t, line, 2));
        assert!(!sel.contains(&t, line, 5));
    }
}

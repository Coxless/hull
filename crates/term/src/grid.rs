//! 画面とスクロールバック。
//!
//! 履歴と画面を 1 本の列に持つ。最後の `rows` 行が画面で、それより前が履歴。
//! 画面全体を 1 行上げるのは末尾に 1 行足すだけで済み、行のコピーは起きない。
//!
//! 行には絶対番号を振る。履歴からあふれて捨てた行も数えるので、
//! 出力が流れても同じ行は同じ番号のまま (選択やプラグインのハイライトが使う)。

use std::collections::VecDeque;

use crate::cell::{Cell, Row};

pub struct Grid {
    cols: usize,
    rows: usize,
    lines: VecDeque<Row>,
    max_history: usize,
    /// 先頭から捨てた行の数。`lines[0]` の絶対番号になる。
    dropped: u64,
    generation: u64,
    /// 履歴の新しいほうから数えて、まだ詰めていない行の数。
    /// 大量の出力のあいだは詰めず、止まってから `compact` でまとめて詰める
    /// (履歴からあふれて捨てる行は、詰める手間をかけずに済む)。
    unpacked: usize,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, max_history: usize) -> Grid {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut grid = Grid {
            cols,
            rows,
            lines: VecDeque::with_capacity(rows),
            max_history,
            dropped: 0,
            generation: 0,
            unpacked: 0,
        };
        for _ in 0..rows {
            let mut row = Row::new(cols, Cell::BLANK);
            row.generation = grid.bump();
            grid.lines.push_back(row);
        }
        grid
    }

    fn bump(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn history_len(&self) -> usize {
        self.lines.len() - self.rows
    }

    pub fn set_max_history(&mut self, max: usize) {
        self.max_history = max;
        while self.history_len() > max {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.unpacked = self.unpacked.min(self.history_len());
    }

    /// まだ詰めていない履歴の行を詰める (出力が止まったときに呼ぶ)。
    pub fn compact(&mut self) {
        let end = self.history_len();
        for i in end - self.unpacked..end {
            self.lines[i].pack();
        }
        self.unpacked = 0;
    }

    /// 画面の `r` 行目。
    pub fn row(&self, r: usize) -> &Row {
        &self.lines[self.history_len() + r]
    }

    /// 画面の `r` 行目を書き換える。描画のために世代を進める。
    pub fn row_mut(&mut self, r: usize) -> &mut Row {
        let generation = self.bump();
        let base = self.history_len();
        let row = &mut self.lines[base + r];
        row.generation = generation;
        row
    }

    /// `offset` 行だけ履歴をさかのぼったときに見える `r` 行目。
    pub fn visible_row(&self, offset: usize, r: usize) -> &Row {
        let offset = offset.min(self.history_len());
        &self.lines[self.history_len() - offset + r]
    }

    /// 画面の `r` 行目の絶対番号。
    pub fn abs_of_screen(&self, r: usize) -> u64 {
        self.dropped + (self.history_len() + r) as u64
    }

    /// 残っている中でいちばん古い行の絶対番号。
    pub fn first_abs(&self) -> u64 {
        self.dropped
    }

    /// 最後の行の次の絶対番号。
    pub fn end_abs(&self) -> u64 {
        self.dropped + self.lines.len() as u64
    }

    /// 絶対番号の行。捨てた行やまだない行は `None`。
    pub fn line(&self, abs: u64) -> Option<&Row> {
        let i = abs.checked_sub(self.dropped)?;
        self.lines.get(i as usize)
    }

    /// 画面全体を `n` 行上げる。上からあふれた行は履歴に入る。
    /// 履歴に入った行の数を返す (さかのぼって見ているときに位置を保つため)。
    pub fn scroll_up_full(&mut self, n: usize, fill: Cell) -> usize {
        let mut kept = 0;
        for _ in 0..n.min(self.rows) {
            // 一番上の行は、詰めずにそのまま履歴に入れる。捨てる行の領域は、新しい一番下の行に使い回す。
            let mut row = if self.history_len() >= self.max_history {
                self.dropped += 1;
                if self.unpacked >= self.history_len() && self.max_history > 0 {
                    self.unpacked -= 1;
                }
                self.lines.pop_front().expect("画面は 1 行以上ある")
            } else {
                kept += 1;
                Row::default()
            };
            if self.max_history > 0 {
                self.unpacked += 1;
            }
            row.reset(self.cols, fill);
            row.generation = self.bump();
            self.lines.push_back(row);
        }
        kept
    }

    /// 画面の `top..=bottom` を `n` 行上げる。上からあふれた行は捨てる。
    pub fn scroll_region_up(&mut self, top: usize, bottom: usize, n: usize, fill: Cell) {
        let base = self.history_len();
        for _ in 0..n.min(bottom + 1 - top) {
            let mut row = self.lines.remove(base + top).expect("範囲は画面の中");
            row.reset(self.cols, fill);
            row.generation = self.bump();
            self.lines.insert(base + bottom, row);
        }
    }

    /// 画面の `top..=bottom` を `n` 行下げる。下からあふれた行は捨てる。
    pub fn scroll_region_down(&mut self, top: usize, bottom: usize, n: usize, fill: Cell) {
        let base = self.history_len();
        for _ in 0..n.min(bottom + 1 - top) {
            let mut row = self.lines.remove(base + bottom).expect("範囲は画面の中");
            row.reset(self.cols, fill);
            row.generation = self.bump();
            self.lines.insert(base + top, row);
        }
    }

    pub fn clear_history(&mut self) {
        self.unpacked = 0;
        let n = self.history_len();
        self.lines.drain(..n);
        self.dropped += n as u64;
    }

    /// すべての行を描き直させる (色や大きさが変わったとき)。
    pub fn mark_all_dirty(&mut self) {
        for i in 0..self.lines.len() {
            let generation = self.bump();
            self.lines[i].generation = generation;
        }
    }

    /// 大きさを変える。カーソルの行を受け取り、変えたあとの行を返す。
    ///
    /// 行が減るときは、カーソルより下の空いた行を先に削り、残りは上から履歴に送る。
    /// 行が増えるときは、履歴から戻せるだけ戻す (戻した分だけカーソルが下がる)。
    pub fn resize(&mut self, cols: usize, rows: usize, cursor_row: usize) -> usize {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut cursor = cursor_row.min(self.rows - 1);
        self.compact();

        if rows < self.rows {
            let mut shrink = self.rows - rows;
            while shrink > 0
                && self.rows - 1 > cursor
                && self.lines.back().is_some_and(|r| r.cells.iter().all(Cell::is_blank))
            {
                self.lines.pop_back();
                self.rows -= 1;
                shrink -= 1;
            }
            for i in 0..shrink {
                let base = self.history_len();
                self.lines[base + i].pack();
            }
            self.rows -= shrink;
            cursor = cursor.saturating_sub(shrink);
            while self.history_len() > self.max_history {
                self.lines.pop_front();
                self.dropped += 1;
            }
        } else if rows > self.rows {
            let grow = rows - self.rows;
            let back = grow.min(self.history_len());
            self.rows += back;
            cursor += back;
            for _ in back..grow {
                self.lines.push_back(Row::new(cols, Cell::BLANK));
                self.rows += 1;
            }
        }

        self.cols = cols;
        let base = self.history_len();
        for i in base..self.lines.len() {
            self.lines[i].resize(cols);
        }
        self.mark_all_dirty();
        cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(grid: &mut Grid, r: usize, text: &str) {
        let row = grid.row_mut(r);
        for (i, ch) in text.chars().enumerate() {
            row.cells[i].ch = ch;
        }
    }

    #[test]
    fn 画面全体のスクロールで履歴に入る() {
        let mut g = Grid::new(4, 2, 10);
        put(&mut g, 0, "ab");
        put(&mut g, 1, "cd");
        assert_eq!(g.scroll_up_full(1, Cell::BLANK), 1);
        assert_eq!(g.history_len(), 1);
        assert_eq!(g.row(0).text(0, 4), "cd");
        assert_eq!(g.visible_row(1, 0).text(0, 4), "ab");
        // 履歴に入った行は、compact で切り詰めて詰める。
        g.compact();
        assert_eq!(g.line(0).unwrap().len(), 2);
        assert!(g.line(0).unwrap().cells.is_empty());
    }

    #[test]
    fn 履歴は上限で古いものから捨てる() {
        let mut g = Grid::new(4, 1, 2);
        for i in 0..5 {
            put(&mut g, 0, &i.to_string());
            g.scroll_up_full(1, Cell::BLANK);
        }
        assert_eq!(g.history_len(), 2);
        assert_eq!(g.first_abs(), 3);
        assert_eq!(g.line(3).unwrap().text(0, 4), "3");
        assert_eq!(g.line(4).unwrap().text(0, 4), "4");
        assert!(g.line(2).is_none());
    }

    #[test]
    fn 範囲のスクロールは履歴に入れない() {
        let mut g = Grid::new(4, 3, 10);
        put(&mut g, 0, "a");
        put(&mut g, 1, "b");
        put(&mut g, 2, "c");
        g.scroll_region_up(0, 1, 1, Cell::BLANK);
        assert_eq!(g.history_len(), 0);
        assert_eq!(g.row(0).text(0, 4), "b");
        assert_eq!(g.row(1).text(0, 4), "");
        assert_eq!(g.row(2).text(0, 4), "c");
        g.scroll_region_down(0, 2, 1, Cell::BLANK);
        assert_eq!(g.row(1).text(0, 4), "b");
    }

    #[test]
    fn 行を減らすと下の空行から削り_増やすと履歴から戻す() {
        let mut g = Grid::new(4, 4, 10);
        put(&mut g, 0, "a");
        put(&mut g, 1, "b");
        let cursor = g.resize(4, 2, 1);
        assert_eq!(cursor, 1);
        assert_eq!(g.history_len(), 0);
        let cursor = g.resize(4, 1, cursor);
        assert_eq!(cursor, 0);
        assert_eq!(g.history_len(), 1);
        assert_eq!(g.row(0).text(0, 4), "b");
        let cursor = g.resize(4, 3, cursor);
        assert_eq!(cursor, 1);
        assert_eq!(g.row(0).text(0, 4), "a");
        assert_eq!(g.row(0).cells.len(), 4);
        assert_eq!(g.row(0).len(), 4);
    }
}

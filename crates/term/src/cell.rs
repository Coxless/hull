//! 1 マス (セル) と 1 行の表現。
//!
//! セルは 16 バイトに収める。スクロールバック 1 万行を RSS 20MB 未満に収めるため、
//! 履歴に移った行は末尾の空白を切り詰めて持つ。

/// 文字色・背景色。`Default` は前景・背景それぞれの既定色を指す。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// セルの属性 (ビットの組み合わせ)。
pub mod flags {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const BLINK: u16 = 1 << 4;
    pub const INVERSE: u16 = 1 << 5;
    pub const HIDDEN: u16 = 1 << 6;
    pub const STRIKE: u16 = 1 << 7;
    /// 全角の文字の左半分。文字はここに入る。
    pub const WIDE: u16 = 1 << 8;
    /// 全角の文字の右半分。中身は持たない。
    pub const WIDE_SPACER: u16 = 1 << 9;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 10;
    pub const CURLY_UNDERLINE: u16 = 1 << 11;
    /// 結合文字が行の `combining` に入っている。
    pub const COMBINING: u16 = 1 << 12;

    /// 描画に関わらない属性を除いた、ペン (SGR) が持つ属性。
    pub const PEN: u16 = BOLD
        | DIM
        | ITALIC
        | UNDERLINE
        | BLINK
        | INVERSE
        | HIDDEN
        | STRIKE
        | DOUBLE_UNDERLINE
        | CURLY_UNDERLINE;
    pub const ANY_UNDERLINE: u16 = UNDERLINE | DOUBLE_UNDERLINE | CURLY_UNDERLINE;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
    /// OSC 8 のハイパーリンク。0 はリンクなし。
    pub link: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Cell::BLANK
    }
}

impl Cell {
    pub const BLANK: Cell =
        Cell { ch: ' ', fg: Color::Default, bg: Color::Default, flags: 0, link: 0 };

    /// 背景色だけを引き継いだ空白 (消去は今の背景色で塗る: BCE)。
    pub fn blank_with_bg(bg: Color) -> Cell {
        Cell { bg, ..Cell::BLANK }
    }

    /// 見た目が空白と変わらない (切り詰めてよい) セル。
    pub fn is_blank(&self) -> bool {
        self.ch == ' '
            && self.bg == Color::Default
            && self.flags & (flags::INVERSE | flags::ANY_UNDERLINE | flags::STRIKE) == 0
            && self.link == 0
    }
}

/// 履歴の行のセルの色と属性 (セルから文字を除いたもの)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Attr {
    fg: Color,
    bg: Color,
    flags: u16,
    link: u16,
}

/// 履歴に移した行を詰めた形。1 セルを 4 バイト (文字 21 ビット + 属性の番号 11 ビット) にし、
/// 属性は行ごとの小さな表に入れる。たいていの行は属性が数種類しかないので、16 バイトのセルの 4 分の 1 で済む。
#[derive(Clone, Debug, Default)]
pub(crate) struct Packed {
    cells: Vec<u32>,
    attrs: Vec<Attr>,
}

const ATTR_BITS: u32 = 11;
const MAX_ATTRS: usize = 1 << ATTR_BITS;

impl Packed {
    /// `cells` を詰める。領域は使い回す。属性が多すぎて詰められなければ `false`。
    fn fill(&mut self, cells: &[Cell]) -> bool {
        self.cells.clear();
        self.attrs.clear();
        self.cells.reserve(cells.len());
        for c in cells {
            let attr = Attr { fg: c.fg, bg: c.bg, flags: c.flags, link: c.link };
            // 同じ属性が続くことが多いので、まず最後のものと比べる。
            let index = match self.attrs.last() {
                Some(last) if *last == attr => self.attrs.len() - 1,
                _ => match self.attrs.iter().position(|a| *a == attr) {
                    Some(i) => i,
                    None => {
                        if self.attrs.len() == MAX_ATTRS {
                            return false;
                        }
                        self.attrs.push(attr);
                        self.attrs.len() - 1
                    }
                },
            };
            self.cells.push((c.ch as u32) << ATTR_BITS | index as u32);
        }
        true
    }

    fn cell(&self, col: usize) -> Option<Cell> {
        let v = *self.cells.get(col)?;
        let a = self.attrs[(v & (MAX_ATTRS as u32 - 1)) as usize];
        let ch = char::from_u32(v >> ATTR_BITS).unwrap_or(' ');
        Some(Cell { ch, fg: a.fg, bg: a.bg, flags: a.flags, link: a.link })
    }

    fn unpack(&self) -> Vec<Cell> {
        (0..self.cells.len()).filter_map(|i| self.cell(i)).collect()
    }
}

/// 1 行。画面の行は `cells` に 16 バイトのセルで持ち、履歴に移した行は詰めて持つ。
#[derive(Clone, Debug, Default)]
pub struct Row {
    /// 画面の行のセル。履歴に移して詰めた行では空 (読むときは `cell` と `len` を使う)。
    pub cells: Vec<Cell>,
    packed: Option<Box<Packed>>,
    /// この行の終わりで折り返して、次の行に続いている。
    pub wrapped: bool,
    /// 中身が変わるたびに増える番号。描画はこれで変わった行だけを作り直す。
    pub generation: u64,
    /// 結合文字 (列, 文字)。めったにないので行ごとに別に持つ。
    pub combining: Vec<(u16, char)>,
}

impl Row {
    pub fn new(cols: usize, fill: Cell) -> Row {
        Row { cells: vec![fill; cols], ..Row::default() }
    }

    /// 履歴に移すときに、末尾の空白を切り詰め、詰めた形にしてメモリを返す。
    pub fn pack(&mut self) {
        if self.packed.is_some() {
            return;
        }
        let len = self.cells.iter().rposition(|c| !c.is_blank()).map_or(0, |i| i + 1);
        let mut packed = Box::<Packed>::default();
        if packed.fill(&self.cells[..len]) {
            packed.cells.shrink_to_fit();
            packed.attrs.shrink_to_fit();
            self.packed = Some(packed);
            self.cells = Vec::new();
        } else {
            self.cells.truncate(len);
            self.cells.shrink_to_fit();
        }
        self.combining.shrink_to_fit();
    }

    /// 詰めた行を、書き換えられる形に戻す (履歴から画面に戻すとき)。
    pub fn unpack(&mut self) {
        if let Some(p) = self.packed.take() {
            self.cells = p.unpack();
        }
    }

    /// セルの数 (詰めた行では、切り詰めたあとの長さ)。
    pub fn len(&self) -> usize {
        match &self.packed {
            Some(p) => p.cells.len(),
            None => self.cells.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 幅を `cols` にそろえる (足りなければ空白で埋め、多ければ切る)。
    pub fn resize(&mut self, cols: usize) {
        self.unpack();
        self.cells.resize(cols, Cell::BLANK);
        self.combining.retain(|&(c, _)| (c as usize) < cols);
    }

    pub fn reset(&mut self, cols: usize, fill: Cell) {
        self.packed = None;
        self.cells.clear();
        self.cells.resize(cols, fill);
        self.wrapped = false;
        self.combining.clear();
    }

    pub fn cell(&self, col: usize) -> Cell {
        match &self.packed {
            Some(p) => p.cell(col),
            None => self.cells.get(col).copied(),
        }
        .unwrap_or(Cell::BLANK)
    }

    /// 行の文字列 (全角の右半分を飛ばし、結合文字を足す)。末尾の空白は落とす。
    pub fn text(&self, start: usize, end: usize) -> String {
        let mut s = String::new();
        let end = end.min(self.len());
        for col in start..end {
            let cell = self.cell(col);
            if cell.flags & flags::WIDE_SPACER != 0 {
                continue;
            }
            s.push(cell.ch);
            if cell.flags & flags::COMBINING != 0 {
                for &(c, ch) in &self.combining {
                    if c as usize == col {
                        s.push(ch);
                    }
                }
            }
        }
        let trimmed = s.trim_end_matches(' ').len();
        s.truncate(trimmed);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 詰めても中身は変わらない() {
        let mut row = Row::new(8, Cell::BLANK);
        row.cells[0] = Cell {
            ch: '日',
            flags: flags::WIDE | flags::BOLD,
            fg: Color::Indexed(1),
            ..Cell::BLANK
        };
        row.cells[1] = Cell {
            ch: ' ',
            flags: flags::WIDE_SPACER | flags::BOLD,
            fg: Color::Indexed(1),
            ..Cell::BLANK
        };
        row.cells[2] = Cell { ch: 'x', bg: Color::Rgb(1, 2, 3), link: 7, ..Cell::BLANK };
        let before: Vec<Cell> = (0..3).map(|i| row.cell(i)).collect();
        row.pack();
        assert!(row.cells.is_empty());
        assert_eq!(row.len(), 3);
        assert_eq!((0..3).map(|i| row.cell(i)).collect::<Vec<_>>(), before);
        assert_eq!(row.cell(5), Cell::BLANK);
        assert_eq!(row.text(0, 8), "日x");
        row.resize(8);
        assert_eq!(row.cells.len(), 8);
        assert_eq!(row.cell(2), before[2]);
    }

    #[test]
    fn 属性が多すぎる行は詰めない() {
        let mut row = Row::new(3000, Cell::BLANK);
        for (i, c) in row.cells.iter_mut().enumerate() {
            *c = Cell {
                ch: 'a',
                fg: Color::Rgb((i % 256) as u8, (i / 256) as u8, 0),
                ..Cell::BLANK
            };
        }
        row.pack();
        assert_eq!(row.len(), 3000);
        assert_eq!(row.cell(2999).fg, Color::Rgb((2999 % 256) as u8, (2999 / 256) as u8, 0));
    }
}

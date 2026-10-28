//! VT の処理。`vte` が切り出した制御シーケンスを、グリッドへの操作に変える。
//!
//! xterm 互換を土台に、truecolor、kitty keyboard protocol、modifyOtherKeys、
//! synchronized output (mode 2026)、bracketed paste、OSC 52 / 8 / 133 / 7 / 9 / 777 を扱う。
//! 窓や GPU には触れない。外に知らせることは `Event` に、PTY に返す応答は `reply` にためる。

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::cell::{Cell, Color, Row, flags};
use crate::grid::Grid;

pub type Rgb = [u8; 3];

/// 本体の外 (窓やプラグイン) に知らせること。
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Bell,
    Title(String),
    /// OSC 7。`file://host/path` から取り出したパス。
    Cwd(String),
    /// OSC 9 / OSC 777;notify。
    Notify {
        title: Option<String>,
        body: String,
    },
    /// OSC 9;4 (ConEmu の進捗表示)。通知とは別に扱う。
    Progress {
        state: u8,
        value: u8,
    },
    /// OSC 133。
    Prompt(PromptMark),
    /// OSC 52 で、クリップボード (`primary` なら選択範囲) に書く。
    Clipboard {
        primary: bool,
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptMark {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandEnd(Option<i32>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseMode {
    None,
    /// 押したときだけ (mode 9)。
    X10,
    /// 押す・離す (mode 1000)。
    Normal,
    /// ボタンを押したままの移動も (mode 1002)。
    Button,
    /// すべての移動 (mode 1003)。
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Debug)]
pub struct Modes {
    pub insert: bool,
    pub autowrap: bool,
    pub origin: bool,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub show_cursor: bool,
    pub reverse_video: bool,
    pub linefeed_newline: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    /// 代替画面でホイールを矢印キーに変える (mode 1007)。
    pub alternate_scroll: bool,
    /// synchronized output (mode 2026)。立っているあいだは描画を待つ。
    pub sync: bool,
    /// xterm の modifyOtherKeys (0, 1, 2)。
    pub modify_other_keys: u8,
    pub cursor_shape: CursorShape,
}

impl Default for Modes {
    fn default() -> Self {
        Modes {
            insert: false,
            autowrap: true,
            origin: false,
            app_cursor: false,
            app_keypad: false,
            show_cursor: true,
            reverse_video: false,
            linefeed_newline: false,
            bracketed_paste: false,
            focus_events: false,
            mouse: MouseMode::None,
            mouse_sgr: false,
            alternate_scroll: true,
            sync: false,
            modify_other_keys: 0,
            cursor_shape: CursorShape::Block,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Charset {
    Ascii,
    DecGraphics,
}

#[derive(Clone, Copy, Debug)]
struct Cursor {
    row: usize,
    col: usize,
    /// SGR で決まる、次に書く文字の色と属性。
    pen: Cell,
    /// 行末に書いた直後。次の文字で折り返す。
    pending_wrap: bool,
    charsets: [Charset; 2],
    active_charset: usize,
}

impl Default for Cursor {
    fn default() -> Self {
        Cursor {
            row: 0,
            col: 0,
            pen: Cell::BLANK,
            pending_wrap: false,
            charsets: [Charset::Ascii; 2],
            active_charset: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Saved {
    cursor: Cursor,
    origin: bool,
}

/// 色の表。0〜255 はパレット、256 は前景、257 は背景、258 はカーソル。
pub const COLOR_FG: usize = 256;
pub const COLOR_BG: usize = 257;
pub const COLOR_CURSOR: usize = 258;
const COLORS: usize = 259;

#[derive(Clone)]
pub struct Palette {
    base: [Rgb; COLORS],
    /// OSC 4 / 10 / 11 / 12 でアプリが変えた色。
    overrides: [Option<Rgb>; COLORS],
}

impl Default for Palette {
    fn default() -> Self {
        let mut base = [[0; 3]; COLORS];
        const ANSI: [Rgb; 16] = [
            [0x17, 0x14, 0x21],
            [0xc0, 0x1c, 0x28],
            [0x26, 0xa2, 0x69],
            [0xa2, 0x73, 0x4c],
            [0x12, 0x48, 0x8b],
            [0xa3, 0x47, 0xba],
            [0x2a, 0xa1, 0xb3],
            [0xd0, 0xcf, 0xcc],
            [0x5e, 0x5c, 0x64],
            [0xf6, 0x61, 0x51],
            [0x33, 0xd1, 0x7a],
            [0xe9, 0xad, 0x0c],
            [0x2a, 0x7b, 0xde],
            [0xc0, 0x61, 0xcb],
            [0x33, 0xc7, 0xde],
            [0xff, 0xff, 0xff],
        ];
        base[..16].copy_from_slice(&ANSI);
        let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
        for (n, slot) in base[16..232].iter_mut().enumerate() {
            *slot = [level(n / 36), level((n / 6) % 6), level(n % 6)];
        }
        for (n, slot) in base[232..256].iter_mut().enumerate() {
            let v = (8 + n * 10) as u8;
            *slot = [v, v, v];
        }
        base[COLOR_FG] = [0xff, 0xff, 0xff];
        base[COLOR_BG] = [0x1e, 0x1e, 0x1e];
        base[COLOR_CURSOR] = [0xff, 0xff, 0xff];
        Palette { base, overrides: [None; COLORS] }
    }
}

impl Palette {
    pub fn get(&self, i: usize) -> Rgb {
        self.overrides[i].unwrap_or(self.base[i])
    }

    pub fn set_base(&mut self, i: usize, rgb: Rgb) {
        self.base[i] = rgb;
    }

    fn set(&mut self, i: usize, rgb: Rgb) {
        self.overrides[i] = Some(rgb);
    }

    fn reset(&mut self, i: usize) {
        self.overrides[i] = None;
    }
}

pub struct Terminal {
    parser: vte::Parser,
    primary: Grid,
    alt: Grid,
    alt_active: bool,
    cursor: Cursor,
    saved: [Option<Saved>; 2],
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    pub modes: Modes,
    title: String,
    title_stack: Vec<String>,
    pub palette: Palette,
    events: Vec<Event>,
    reply: Vec<u8>,
    display_offset: usize,
    links: Vec<String>,
    last_char: Option<char>,
    cell_px: (u32, u32),
    /// kitty keyboard protocol のフラグのスタック (主画面と代替画面で別に持つ)。
    kitty_flags: [Vec<u8>; 2],
    /// まだ書いていない文字 (続けて届いた文字をまとめて書くため)。
    printing: Vec<char>,
    /// パレットや反転表示が変わり、全体を描き直す必要がある。
    colors_changed: bool,
}

const MAX_TITLE_STACK: usize = 16;
const MAX_KITTY_STACK: usize = 16;

impl Terminal {
    pub fn new(cols: usize, rows: usize, history: usize) -> Terminal {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut term = Terminal {
            parser: vte::Parser::new(),
            primary: Grid::new(cols, rows, history),
            alt: Grid::new(cols, rows, 0),
            alt_active: false,
            cursor: Cursor::default(),
            saved: [None, None],
            top: 0,
            bottom: rows - 1,
            tabs: Vec::new(),
            modes: Modes::default(),
            title: String::new(),
            title_stack: Vec::new(),
            palette: Palette::default(),
            events: Vec::new(),
            reply: Vec::new(),
            display_offset: 0,
            links: Vec::new(),
            last_char: None,
            cell_px: (8, 16),
            kitty_flags: [Vec::new(), Vec::new()],
            colors_changed: false,
            printing: Vec::with_capacity(4096),
        };
        term.reset_tabs();
        term
    }

    /// PTY から読んだバイト列を処理する。
    pub fn advance(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
        self.flush_print();
    }

    /// ためておいた文字を書く。同じ行に続けて書ける半角の文字は、行を 1 度だけ引いてまとめて書く
    /// (1 文字ずつ行を引くと、`cat` のような大量の出力で遅くなる)。
    fn flush_print(&mut self) {
        if self.printing.is_empty() {
            return;
        }
        let chars = std::mem::take(&mut self.printing);
        let mut i = 0;
        while i < chars.len() {
            i += self.print_run(&chars[i..]);
        }
        self.printing = chars;
        self.printing.clear();
    }

    /// 先頭から書けるだけ書き、書いた文字の数を返す (少なくとも 1)。
    fn print_run(&mut self, chars: &[char]) -> usize {
        let fast = self.cursor.charsets[self.cursor.active_charset] == Charset::Ascii
            && !self.modes.insert
            && !self.cursor.pending_wrap;
        if !fast || char_width(chars[0]) != 1 {
            self.print_char(chars[0]);
            return 1;
        }
        let cols = self.cols();
        let (row, start) = (self.cursor.row, self.cursor.col);
        let pen = self.cursor.pen;
        let template = Cell { ch: ' ', fg: pen.fg, bg: pen.bg, flags: pen.flags, link: pen.link };
        let fill = self.fill();
        let r = self.grid_mut().row_mut(row);
        let mut col = start;
        let mut n = 0;
        // 最後の列は折り返しの扱いがあるので、1 文字ずつの処理に任せる。
        while n < chars.len() && col + 1 < cols {
            let c = chars[n];
            if char_width(c) != 1 {
                break;
            }
            let old = r.cells[col].flags;
            // 全角の片側を上書きするなら、もう片側も消す。左半分をこの回で書いたなら、もう消えている。
            if old & flags::WIDE != 0 || old & flags::WIDE_SPACER != 0 && col == start {
                Self::clear_wide_pair(r, col, fill);
            }
            r.cells[col] = Cell { ch: c, ..template };
            col += 1;
            n += 1;
        }
        if n == 0 {
            self.print_char(chars[0]);
            return 1;
        }
        if !r.combining.is_empty() {
            r.combining.retain(|&(cc, _)| (cc as usize) < start || cc as usize >= col);
        }
        self.cursor.col = col;
        self.last_char = Some(chars[n - 1]);
        n
    }

    pub fn grid(&self) -> &Grid {
        if self.alt_active { &self.alt } else { &self.primary }
    }

    fn grid_mut(&mut self) -> &mut Grid {
        if self.alt_active { &mut self.alt } else { &mut self.primary }
    }

    pub fn cols(&self) -> usize {
        self.grid().cols()
    }

    pub fn rows(&self) -> usize {
        self.grid().rows()
    }

    pub fn is_alt_screen(&self) -> bool {
        self.alt_active
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor.row, self.cursor.col)
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn link(&self, id: u16) -> Option<&str> {
        self.links.get(usize::from(id).checked_sub(1)?).map(String::as_str)
    }

    pub fn kitty_flags(&self) -> u8 {
        self.kitty_flags[self.alt_active as usize].last().copied().unwrap_or(0)
    }

    /// 1 マスの大きさ (ピクセル)。CSI 14 t / 16 t の応答に使う。
    pub fn set_cell_size(&mut self, width: u32, height: u32) {
        self.cell_px = (width, height);
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// PTY に書き戻す応答 (DA や DSR など) を取り出す。
    pub fn take_reply(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.reply)
    }

    /// パレットや反転が変わったかを返し、印を下ろす。
    pub fn take_colors_changed(&mut self) -> bool {
        std::mem::take(&mut self.colors_changed)
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols() && rows == self.rows() {
            return;
        }
        let row = self.cursor.row;
        let (primary_row, alt_row) = if self.alt_active {
            let saved_row = self.saved[0].map_or(0, |s| s.cursor.row);
            (self.primary.resize(cols, rows, saved_row), self.alt.resize(cols, rows, row))
        } else {
            (self.primary.resize(cols, rows, row), self.alt.resize(cols, rows, 0))
        };
        self.cursor.row = if self.alt_active { alt_row } else { primary_row };
        if self.alt_active
            && let Some(saved) = &mut self.saved[0]
        {
            saved.cursor.row = primary_row;
            saved.cursor.col = saved.cursor.col.min(cols - 1);
        }
        self.cursor.col = self.cursor.col.min(cols - 1);
        self.cursor.pending_wrap = false;
        self.top = 0;
        self.bottom = rows - 1;
        self.reset_tabs();
        self.display_offset = self.display_offset.min(self.grid().history_len());
    }

    /// 出力が止まったときに、履歴の行を詰めてメモリを返す。
    pub fn compact_history(&mut self) {
        self.primary.compact();
    }

    pub fn set_max_history(&mut self, lines: usize) {
        self.primary.set_max_history(lines);
        self.display_offset = self.display_offset.min(self.grid().history_len());
    }

    // ---- スクロールバックの表示位置 ----

    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

    /// 履歴をさかのぼる (正) か戻る (負)。
    pub fn scroll_display(&mut self, delta: isize) {
        let max = self.grid().history_len() as isize;
        self.display_offset = (self.display_offset as isize + delta).clamp(0, max) as usize;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.display_offset = 0;
    }

    /// 絶対番号 `abs` の行が、画面のおよそ真ん中に来るようにさかのぼる。
    pub fn scroll_to_line(&mut self, abs: u64) {
        let grid = self.grid();
        let screen_top = grid.abs_of_screen(0);
        let want_top = abs.saturating_sub(grid.rows() as u64 / 2);
        let offset = screen_top.saturating_sub(want_top) as usize;
        self.display_offset = offset.min(grid.history_len());
    }

    /// 見えている `r` 行目。
    pub fn visible_row(&self, r: usize) -> &Row {
        self.grid().visible_row(self.display_offset, r)
    }

    /// 見えている `r` 行目の絶対番号。
    pub fn visible_abs(&self, r: usize) -> u64 {
        self.grid().abs_of_screen(r) - self.display_offset.min(self.grid().history_len()) as u64
    }

    /// 絶対番号の行の文字列。
    pub fn line_text(&self, abs: u64) -> Option<String> {
        let row = self.grid().line(abs)?;
        Some(row.text(0, row.len()))
    }

    /// 2 点のあいだの文字列 (両端を含む)。折り返した行はつなげる。
    pub fn text_between(&self, start: (u64, usize), end: (u64, usize)) -> String {
        let (start, end) = if start <= end { (start, end) } else { (end, start) };
        let mut out = String::new();
        let grid = self.grid();
        for abs in start.0..=end.0 {
            let Some(row) = grid.line(abs) else { continue };
            let from = if abs == start.0 { start.1 } else { 0 };
            let to = if abs == end.0 { end.1 + 1 } else { row.len() };
            out.push_str(&row.text(from, to));
            if abs != end.0 && !row.wrapped {
                out.push('\n');
            }
        }
        out
    }

    // ---- 内部の操作 ----

    fn reset_tabs(&mut self) {
        let cols = self.cols();
        self.tabs = (0..cols).map(|c| c % 8 == 0 && c != 0).collect();
    }

    fn fill(&self) -> Cell {
        Cell::blank_with_bg(self.cursor.pen.bg)
    }

    fn emit(&mut self, event: Event) {
        self.events.push(event);
    }

    fn scroll_up(&mut self, n: usize) {
        let fill = self.fill();
        let (top, bottom, rows) = (self.top, self.bottom, self.rows());
        if top == 0 && bottom == rows - 1 && !self.alt_active {
            let kept = self.primary.scroll_up_full(n, fill);
            if self.display_offset > 0 {
                self.display_offset = (self.display_offset + kept).min(self.primary.history_len());
            }
        } else {
            self.grid_mut().scroll_region_up(top, bottom, n, fill);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let fill = self.fill();
        let (top, bottom) = (self.top, self.bottom);
        self.grid_mut().scroll_region_down(top, bottom, n, fill);
    }

    fn linefeed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows() {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.top {
            self.scroll_down(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    fn wrap_line(&mut self) {
        let row = self.cursor.row;
        self.grid_mut().row_mut(row).wrapped = true;
        self.linefeed();
        self.cursor.col = 0;
    }

    fn goto(&mut self, row: usize, col: usize) {
        let (min_row, max_row) =
            if self.modes.origin { (self.top, self.bottom) } else { (0, self.rows() - 1) };
        let row = if self.modes.origin { row + self.top } else { row };
        self.cursor.row = row.clamp(min_row, max_row);
        self.cursor.col = col.min(self.cols() - 1);
        self.cursor.pending_wrap = false;
    }

    fn move_up(&mut self, n: usize) {
        let min = if self.cursor.row >= self.top { self.top } else { 0 };
        self.cursor.row = self.cursor.row.saturating_sub(n).max(min);
        self.cursor.pending_wrap = false;
    }

    fn move_down(&mut self, n: usize) {
        let max = if self.cursor.row <= self.bottom { self.bottom } else { self.rows() - 1 };
        self.cursor.row = (self.cursor.row + n).min(max);
        self.cursor.pending_wrap = false;
    }

    /// 全角の文字の片側だけを上書きするとき、もう片側を空白にする。
    fn clear_wide_pair(row: &mut Row, col: usize, fill: Cell) {
        let Some(cell) = row.cells.get(col).copied() else { return };
        if cell.flags & flags::WIDE_SPACER != 0 && col > 0 {
            row.cells[col - 1] = fill;
        }
        if cell.flags & flags::WIDE != 0 && col + 1 < row.cells.len() {
            row.cells[col + 1] = fill;
        }
    }

    fn erase_cells(&mut self, row: usize, from: usize, to: usize) {
        let fill = self.fill();
        let cols = self.cols();
        let to = to.min(cols);
        if from >= to {
            return;
        }
        let r = self.grid_mut().row_mut(row);
        Self::clear_wide_pair(r, from, fill);
        Self::clear_wide_pair(r, to - 1, fill);
        r.cells[from..to].fill(fill);
        r.combining.retain(|&(c, _)| (c as usize) < from || c as usize >= to);
        if to == cols {
            r.wrapped = false;
        }
    }

    fn erase_rows(&mut self, from: usize, to: usize) {
        let fill = self.fill();
        let cols = self.cols();
        for row in from..to {
            self.grid_mut().row_mut(row).reset(cols, fill);
        }
    }

    fn insert_blank(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols());
        let fill = self.fill();
        let n = n.min(cols - col);
        let r = self.grid_mut().row_mut(row);
        Self::clear_wide_pair(r, col, fill);
        r.cells.copy_within(col..cols - n, col + n);
        r.cells[col..col + n].fill(fill);
        r.combining.clear();
        self.cursor.pending_wrap = false;
    }

    fn delete_chars(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols());
        let fill = self.fill();
        let n = n.min(cols - col);
        let r = self.grid_mut().row_mut(row);
        Self::clear_wide_pair(r, col, fill);
        Self::clear_wide_pair(r, (col + n).min(cols - 1), fill);
        r.cells.copy_within(col + n..cols, col);
        r.cells[cols - n..].fill(fill);
        r.combining.clear();
        self.cursor.pending_wrap = false;
    }

    fn insert_lines(&mut self, n: usize) {
        let row = self.cursor.row;
        if row < self.top || row > self.bottom {
            return;
        }
        let fill = self.fill();
        let bottom = self.bottom;
        self.grid_mut().scroll_region_down(row, bottom, n, fill);
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn delete_lines(&mut self, n: usize) {
        let row = self.cursor.row;
        if row < self.top || row > self.bottom {
            return;
        }
        let fill = self.fill();
        let bottom = self.bottom;
        self.grid_mut().scroll_region_up(row, bottom, n, fill);
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn save_cursor(&mut self) {
        self.saved[self.alt_active as usize] =
            Some(Saved { cursor: self.cursor, origin: self.modes.origin });
    }

    fn restore_cursor(&mut self) {
        match self.saved[self.alt_active as usize] {
            Some(saved) => {
                self.cursor = saved.cursor;
                self.modes.origin = saved.origin;
            }
            None => {
                self.cursor = Cursor::default();
                self.modes.origin = false;
            }
        }
        self.cursor.row = self.cursor.row.min(self.rows() - 1);
        self.cursor.col = self.cursor.col.min(self.cols() - 1);
    }

    fn enter_alt_screen(&mut self, save: bool, clear: bool) {
        if self.alt_active {
            return;
        }
        if save {
            self.save_cursor();
        }
        self.alt_active = true;
        self.display_offset = 0;
        if clear {
            let rows = self.rows();
            self.erase_rows(0, rows);
        }
        self.alt.mark_all_dirty();
    }

    fn leave_alt_screen(&mut self, restore: bool) {
        if !self.alt_active {
            return;
        }
        self.alt_active = false;
        if restore {
            self.restore_cursor();
        }
        self.primary.mark_all_dirty();
    }

    /// 端末を初期状態に戻す (RIS)。履歴は残す。
    fn full_reset(&mut self) {
        self.leave_alt_screen(false);
        self.cursor = Cursor::default();
        self.saved = [None, None];
        self.modes = Modes::default();
        self.top = 0;
        self.bottom = self.rows() - 1;
        self.reset_tabs();
        let rows = self.rows();
        self.erase_rows(0, rows);
        self.palette.overrides = [None; COLORS];
        self.kitty_flags = [Vec::new(), Vec::new()];
        self.colors_changed = true;
    }

    fn soft_reset(&mut self) {
        self.modes.insert = false;
        self.modes.origin = false;
        self.modes.autowrap = true;
        self.modes.app_cursor = false;
        self.modes.app_keypad = false;
        self.modes.show_cursor = true;
        self.top = 0;
        self.bottom = self.rows() - 1;
        self.cursor.pen = Cell::BLANK;
        self.cursor.charsets = [Charset::Ascii; 2];
        self.cursor.active_charset = 0;
        self.saved = [None, None];
    }

    fn add_combining(&mut self, ch: char) {
        let (row, mut col) = (self.cursor.row, self.cursor.col);
        if !self.cursor.pending_wrap {
            if col == 0 {
                return;
            }
            col -= 1;
        }
        let r = self.grid_mut().row_mut(row);
        if r.cells[col].flags & flags::WIDE_SPACER != 0 && col > 0 {
            col -= 1;
        }
        if r.combining.len() < 64 {
            r.cells[col].flags |= flags::COMBINING;
            r.combining.push((col as u16, ch));
        }
    }

    fn set_mode(&mut self, mode: u16, private: bool, on: bool) {
        if !private {
            match mode {
                4 => self.modes.insert = on,
                20 => self.modes.linefeed_newline = on,
                _ => {}
            }
            return;
        }
        match mode {
            1 => self.modes.app_cursor = on,
            5 => {
                if self.modes.reverse_video != on {
                    self.modes.reverse_video = on;
                    self.colors_changed = true;
                }
            }
            6 => {
                self.modes.origin = on;
                self.goto(0, 0);
            }
            7 => self.modes.autowrap = on,
            25 => self.modes.show_cursor = on,
            9 => self.modes.mouse = if on { MouseMode::X10 } else { MouseMode::None },
            1000 => self.modes.mouse = if on { MouseMode::Normal } else { MouseMode::None },
            1002 => self.modes.mouse = if on { MouseMode::Button } else { MouseMode::None },
            1003 => self.modes.mouse = if on { MouseMode::Any } else { MouseMode::None },
            1004 => self.modes.focus_events = on,
            1006 => self.modes.mouse_sgr = on,
            1007 => self.modes.alternate_scroll = on,
            47 | 1047 => {
                if on {
                    self.enter_alt_screen(false, mode == 1047);
                } else {
                    if mode == 1047 {
                        let rows = self.rows();
                        self.erase_rows(0, rows);
                    }
                    self.leave_alt_screen(false);
                }
            }
            1048 => {
                if on {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if on {
                    self.enter_alt_screen(true, true);
                } else {
                    self.leave_alt_screen(true);
                }
            }
            2004 => self.modes.bracketed_paste = on,
            2026 => self.modes.sync = on,
            _ => {}
        }
    }

    /// DECRQM の答え (1: 立っている、2: 下りている、0: 知らない)。
    fn mode_state(&self, mode: u16, private: bool) -> u8 {
        let flag = |b: bool| if b { 1 } else { 2 };
        if !private {
            return match mode {
                4 => flag(self.modes.insert),
                20 => flag(self.modes.linefeed_newline),
                _ => 0,
            };
        }
        match mode {
            1 => flag(self.modes.app_cursor),
            5 => flag(self.modes.reverse_video),
            6 => flag(self.modes.origin),
            7 => flag(self.modes.autowrap),
            25 => flag(self.modes.show_cursor),
            9 => flag(self.modes.mouse == MouseMode::X10),
            1000 => flag(self.modes.mouse == MouseMode::Normal),
            1002 => flag(self.modes.mouse == MouseMode::Button),
            1003 => flag(self.modes.mouse == MouseMode::Any),
            1004 => flag(self.modes.focus_events),
            1006 => flag(self.modes.mouse_sgr),
            1007 => flag(self.modes.alternate_scroll),
            47 | 1047 | 1049 => flag(self.alt_active),
            2004 => flag(self.modes.bracketed_paste),
            2026 => flag(self.modes.sync),
            _ => 0,
        }
    }

    fn sgr(&mut self, params: &Params) {
        let pen = &mut self.cursor.pen;
        if params.is_empty() {
            *pen = Cell { link: pen.link, ..Cell::BLANK };
            return;
        }
        let mut iter = params.iter();
        while let Some(param) = iter.next() {
            match param[0] {
                0 => *pen = Cell { link: pen.link, ..Cell::BLANK },
                1 => pen.flags |= flags::BOLD,
                2 => pen.flags |= flags::DIM,
                3 => pen.flags |= flags::ITALIC,
                4 => {
                    pen.flags &= !flags::ANY_UNDERLINE;
                    match param.get(1).copied().unwrap_or(1) {
                        0 => {}
                        2 => pen.flags |= flags::DOUBLE_UNDERLINE,
                        3 => pen.flags |= flags::CURLY_UNDERLINE,
                        _ => pen.flags |= flags::UNDERLINE,
                    }
                }
                5 | 6 => pen.flags |= flags::BLINK,
                7 => pen.flags |= flags::INVERSE,
                8 => pen.flags |= flags::HIDDEN,
                9 => pen.flags |= flags::STRIKE,
                21 => {
                    pen.flags &= !flags::ANY_UNDERLINE;
                    pen.flags |= flags::DOUBLE_UNDERLINE;
                }
                22 => pen.flags &= !(flags::BOLD | flags::DIM),
                23 => pen.flags &= !flags::ITALIC,
                24 => pen.flags &= !flags::ANY_UNDERLINE,
                25 => pen.flags &= !flags::BLINK,
                27 => pen.flags &= !flags::INVERSE,
                28 => pen.flags &= !flags::HIDDEN,
                29 => pen.flags &= !flags::STRIKE,
                n @ 30..=37 => pen.fg = Color::Indexed((n - 30) as u8),
                38 => {
                    if let Some(c) = extended_color(param, &mut iter) {
                        pen.fg = c;
                    }
                }
                39 => pen.fg = Color::Default,
                n @ 40..=47 => pen.bg = Color::Indexed((n - 40) as u8),
                48 => {
                    if let Some(c) = extended_color(param, &mut iter) {
                        pen.bg = c;
                    }
                }
                49 => pen.bg = Color::Default,
                // 下線の色は持たないが、続く引数は読み飛ばす。
                58 => {
                    extended_color(param, &mut iter);
                }
                n @ 90..=97 => pen.fg = Color::Indexed((n - 90 + 8) as u8),
                n @ 100..=107 => pen.bg = Color::Indexed((n - 100 + 8) as u8),
                _ => {}
            }
        }
    }

    fn reply_str(&mut self, s: &str) {
        self.reply.extend_from_slice(s.as_bytes());
    }

    fn report_color(&mut self, osc: &str, index: Option<usize>, color: usize, bell: bool) {
        let [r, g, b] = self.palette.get(color);
        let prefix = match index {
            Some(i) => format!("{osc};{i}"),
            None => osc.to_string(),
        };
        let end = if bell { "\x07" } else { "\x1b\\" };
        let s = format!(
            "\x1b]{prefix};rgb:{:04x}/{:04x}/{:04x}{end}",
            u16::from(r) * 257,
            u16::from(g) * 257,
            u16::from(b) * 257
        );
        self.reply_str(&s);
    }

    fn csi_window_ops(&mut self, args: &[u16]) {
        match args.first().copied().unwrap_or(0) {
            14 => {
                let (w, h) = self.cell_px;
                let s = format!("\x1b[4;{};{}t", h * self.rows() as u32, w * self.cols() as u32);
                self.reply_str(&s);
            }
            16 => {
                let (w, h) = self.cell_px;
                self.reply_str(&format!("\x1b[6;{h};{w}t"));
            }
            18 => {
                let s = format!("\x1b[8;{};{}t", self.rows(), self.cols());
                self.reply_str(&s);
            }
            22 => {
                if self.title_stack.len() < MAX_TITLE_STACK {
                    self.title_stack.push(self.title.clone());
                }
            }
            23 => {
                if let Some(title) = self.title_stack.pop() {
                    self.title = title.clone();
                    self.emit(Event::Title(title));
                }
            }
            _ => {}
        }
    }

    fn kitty_keyboard(&mut self, intermediates: &[u8], args: &[u16]) {
        let stack = &mut self.kitty_flags[self.alt_active as usize];
        match intermediates.first() {
            Some(b'?') => {
                let flags = stack.last().copied().unwrap_or(0);
                self.reply_str(&format!("\x1b[?{flags}u"));
            }
            Some(b'>') => {
                if stack.len() >= MAX_KITTY_STACK {
                    stack.remove(0);
                }
                stack.push(args.first().copied().unwrap_or(0) as u8 & 0x1f);
            }
            Some(b'<') => {
                let n = args.first().copied().unwrap_or(1).max(1) as usize;
                let keep = stack.len().saturating_sub(n);
                stack.truncate(keep);
            }
            Some(b'=') => {
                let value = args.first().copied().unwrap_or(0) as u8 & 0x1f;
                let current = stack.last().copied().unwrap_or(0);
                let new = match args.get(1).copied().unwrap_or(1) {
                    2 => current | value,
                    3 => current & !value,
                    _ => value,
                };
                match stack.last_mut() {
                    Some(top) => *top = new,
                    None => stack.push(new),
                }
            }
            _ => {}
        }
    }

    fn osc_color_query_or_set(&mut self, params: &[&[u8]], bell: bool) {
        let Ok(code) = std::str::from_utf8(params[0]) else { return };
        let base = match code {
            "10" => COLOR_FG,
            "11" => COLOR_BG,
            "12" => COLOR_CURSOR,
            _ => return,
        };
        // OSC 10;fg;bg のように続けて書ける。
        for (i, spec) in params[1..].iter().enumerate() {
            let target = base + i;
            if target >= COLORS {
                break;
            }
            let osc = (10 + i + base - COLOR_FG).to_string();
            if *spec == b"?" {
                self.report_color(&osc, None, target, bell);
            } else if let Some(rgb) = parse_color(spec) {
                self.palette.set(target, rgb);
                self.colors_changed = true;
            }
        }
    }

    fn osc(&mut self, params: &[&[u8]], bell: bool) {
        let Some(&first) = params.first() else { return };
        let text = |i: usize| -> String {
            params
                .get(i..)
                .unwrap_or(&[])
                .iter()
                .map(|p| String::from_utf8_lossy(p))
                .collect::<Vec<_>>()
                .join(";")
        };
        match first {
            b"0" | b"2" => {
                let title = text(1);
                self.title = title.clone();
                self.emit(Event::Title(title));
            }
            b"4" => {
                let mut i = 1;
                while i + 1 < params.len() {
                    let index = std::str::from_utf8(params[i]).ok().and_then(|s| s.parse().ok());
                    if let Some(index) = index.filter(|&n: &usize| n < 256) {
                        if params[i + 1] == b"?" {
                            self.report_color("4", Some(index), index, bell);
                        } else if let Some(rgb) = parse_color(params[i + 1]) {
                            self.palette.set(index, rgb);
                            self.colors_changed = true;
                        }
                    }
                    i += 2;
                }
            }
            b"7" => {
                if let Some(path) = parse_file_url(&text(1)) {
                    self.emit(Event::Cwd(path));
                }
            }
            b"8" => {
                let uri = text(2);
                self.cursor.pen.link = if uri.is_empty() {
                    0
                } else {
                    // 同じ URI が続くことが多いので、直前と同じなら番号を使い回す。
                    match self.links.iter().rposition(|l| *l == uri) {
                        Some(i) => (i + 1) as u16,
                        None => {
                            if self.links.len() >= usize::from(u16::MAX) {
                                self.links.clear();
                            }
                            self.links.push(uri);
                            self.links.len() as u16
                        }
                    }
                };
            }
            b"9" => {
                if params.get(1) == Some(&&b"4"[..]) {
                    let num = |i: usize| -> u8 {
                        params
                            .get(i)
                            .and_then(|p| std::str::from_utf8(p).ok())
                            .and_then(|s| s.parse::<u8>().ok())
                            .unwrap_or(0)
                    };
                    self.emit(Event::Progress { state: num(2), value: num(3).min(100) });
                } else {
                    self.emit(Event::Notify { title: None, body: text(1) });
                }
            }
            b"10" | b"11" | b"12" => self.osc_color_query_or_set(params, bell),
            b"52" => {
                let Some(target) = params.get(1) else { return };
                let Some(data) = params.get(2) else { return };
                // 読み出し (`?`) には答えない。画面の中のプログラムにクリップボードを読ませない。
                if *data == b"?" {
                    return;
                }
                if let Some(bytes) = base64_decode(data) {
                    let primary =
                        !target.is_empty() && target.iter().all(|&c| c == b'p' || c == b's');
                    self.emit(Event::Clipboard {
                        primary,
                        text: String::from_utf8_lossy(&bytes).into_owned(),
                    });
                }
            }
            b"104" => {
                if params.len() == 1 || params[1].is_empty() {
                    for i in 0..256 {
                        self.palette.reset(i);
                    }
                } else {
                    for p in &params[1..] {
                        if let Some(i) =
                            std::str::from_utf8(p).ok().and_then(|s| s.parse::<usize>().ok())
                            && i < 256
                        {
                            self.palette.reset(i);
                        }
                    }
                }
                self.colors_changed = true;
            }
            b"110" | b"111" | b"112" => {
                let i = match first {
                    b"110" => COLOR_FG,
                    b"111" => COLOR_BG,
                    _ => COLOR_CURSOR,
                };
                self.palette.reset(i);
                self.colors_changed = true;
            }
            b"133" => {
                let mark = match params.get(1).and_then(|p| p.first()) {
                    Some(b'A') => PromptMark::PromptStart,
                    Some(b'B') => PromptMark::CommandStart,
                    Some(b'C') => PromptMark::OutputStart,
                    Some(b'D') => PromptMark::CommandEnd(
                        params
                            .get(2)
                            .and_then(|p| std::str::from_utf8(p).ok())
                            .and_then(|s| s.parse().ok()),
                    ),
                    _ => return,
                };
                self.emit(Event::Prompt(mark));
            }
            b"777" if params.get(1) == Some(&&b"notify"[..]) => {
                let title = params.get(2).map(|t| String::from_utf8_lossy(t).into_owned());
                self.emit(Event::Notify { title, body: text(3) });
            }
            _ => {}
        }
    }
}

/// 38 / 48 / 58 に続く色の指定を読む。`38;5;n` と `38:5:n`、`38;2;r;g;b` と `38:2::r:g:b` の両方を受ける。
fn extended_color<'a>(param: &[u16], iter: &mut impl Iterator<Item = &'a [u16]>) -> Option<Color> {
    if param.len() > 1 {
        return match param[1] {
            5 => param.get(2).map(|&n| Color::Indexed(n as u8)),
            2 => {
                // 38:2:<色空間>:r:g:b と 38:2:r:g:b の両方がある。
                let rgb = if param.len() >= 6 { &param[3..6] } else { param.get(2..5)? };
                Some(Color::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
            }
            _ => None,
        };
    }
    match iter.next()?[0] {
        5 => iter.next().map(|p| Color::Indexed(p[0] as u8)),
        2 => {
            let r = iter.next()?[0];
            let g = iter.next()?[0];
            let b = iter.next()?[0];
            Some(Color::Rgb(r as u8, g as u8, b as u8))
        }
        _ => None,
    }
}

/// `rgb:rr/gg/bb` (各 1〜4 桁) と `#rrggbb`、`#rgb` を読む。
pub fn parse_color(spec: &[u8]) -> Option<Rgb> {
    let s = std::str::from_utf8(spec).ok()?;
    if let Some(rest) = s.strip_prefix("rgb:") {
        let mut out = [0u8; 3];
        let mut parts = rest.split('/');
        for slot in &mut out {
            let part = parts.next()?;
            if part.is_empty() || part.len() > 4 {
                return None;
            }
            let v = u32::from_str_radix(part, 16).ok()?;
            let max = (1u32 << (4 * part.len())) - 1;
            *slot = (v * 255 / max) as u8;
        }
        return parts.next().is_none().then_some(out);
    }
    let hex = s.strip_prefix('#')?;
    let digit = |i: usize, n: usize| u8::from_str_radix(&hex[i..i + n], 16).ok();
    match hex.len() {
        6 => Some([digit(0, 2)?, digit(2, 2)?, digit(4, 2)?]),
        3 => Some([digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17]),
        _ => None,
    }
}

/// `file://host/path` からパスを取り出し、%xx をほどく。
fn parse_file_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn base64_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn dec_graphics(c: char) -> char {
    match c {
        '_' => ' ',
        '`' => '◆',
        'a' => '▒',
        'b' => '␉',
        'c' => '␌',
        'd' => '␍',
        'e' => '␊',
        'f' => '°',
        'g' => '±',
        'h' => '␤',
        'i' => '␋',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        c => c,
    }
}

/// 文字の幅 (0, 1, 2)。
pub fn char_width(c: char) -> usize {
    if (c as u32) < 0x7f {
        return 1;
    }
    c.width().unwrap_or(0)
}

/// CSI の数値の引数を、最大 16 個まで取り出す (サブパラメータは先頭だけ)。
fn collect_args(params: &Params) -> ([u16; 16], usize) {
    let mut out = [0u16; 16];
    let mut n = 0;
    for p in params.iter().take(16) {
        out[n] = p[0];
        n += 1;
    }
    (out, n)
}

impl Perform for Terminal {
    fn print(&mut self, c: char) {
        self.printing.push(c);
    }

    fn execute(&mut self, byte: u8) {
        self.flush_print();
        self.execute_byte(byte);
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.flush_print();
        self.csi(params, intermediates, ignore, action);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.flush_print();
        self.esc(intermediates, ignore, byte);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.flush_print();
        self.osc(params, bell_terminated);
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {
        self.flush_print();
    }
}

impl Terminal {
    fn print_char(&mut self, c: char) {
        let c = if self.cursor.charsets[self.cursor.active_charset] == Charset::DecGraphics {
            dec_graphics(c)
        } else {
            c
        };
        let width = char_width(c);
        if width == 0 {
            self.add_combining(c);
            return;
        }
        self.last_char = Some(c);
        let cols = self.cols();

        if self.cursor.pending_wrap && self.modes.autowrap {
            self.wrap_line();
        }
        if width == 2 && self.cursor.col + 1 >= cols {
            if !self.modes.autowrap || cols < 2 {
                return;
            }
            let (row, col) = (self.cursor.row, self.cursor.col);
            self.erase_cells(row, col, cols);
            self.wrap_line();
        }
        if self.modes.insert {
            self.insert_blank(width);
        }

        let (row, col) = (self.cursor.row, self.cursor.col);
        let pen = self.cursor.pen;
        let fill = self.fill();
        let r = self.grid_mut().row_mut(row);
        Self::clear_wide_pair(r, col, fill);
        if width == 2 {
            Self::clear_wide_pair(r, col + 1, fill);
        }
        let wide = if width == 2 { flags::WIDE } else { 0 };
        r.cells[col] =
            Cell { ch: c, fg: pen.fg, bg: pen.bg, flags: pen.flags | wide, link: pen.link };
        if width == 2 {
            r.cells[col + 1] = Cell {
                ch: ' ',
                fg: pen.fg,
                bg: pen.bg,
                flags: pen.flags | flags::WIDE_SPACER,
                link: pen.link,
            };
        }
        if !r.combining.is_empty() {
            r.combining.retain(|&(cc, _)| (cc as usize) < col || cc as usize >= col + width);
        }

        let next = col + width;
        if next >= cols {
            self.cursor.col = cols - 1;
            self.cursor.pending_wrap = self.modes.autowrap;
        } else {
            self.cursor.col = next;
        }
    }

    fn execute_byte(&mut self, byte: u8) {
        match byte {
            0x07 => self.emit(Event::Bell),
            0x08 => {
                self.cursor.col = self.cursor.col.saturating_sub(1);
                self.cursor.pending_wrap = false;
            }
            0x09 => {
                let cols = self.cols();
                let mut col = self.cursor.col + 1;
                while col < cols - 1 && !self.tabs[col] {
                    col += 1;
                }
                self.cursor.col = col.min(cols - 1);
            }
            0x0a..=0x0c => {
                self.linefeed();
                if self.modes.linefeed_newline {
                    self.cursor.col = 0;
                }
            }
            0x0d => {
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            0x0e => self.cursor.active_charset = 1,
            0x0f => self.cursor.active_charset = 0,
            _ => {}
        }
    }

    fn csi(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let (args, nargs) = collect_args(params);
        let args = &args[..nargs];
        // 0 と省略は 1 として扱う引数。
        let arg1 = |i: usize| args.get(i).copied().filter(|&v| v != 0).unwrap_or(1) as usize;
        let arg0 = |i: usize| args.get(i).copied().unwrap_or(0) as usize;
        let private = intermediates.first() == Some(&b'?');

        match (action, intermediates) {
            ('@', []) => self.insert_blank(arg1(0)),
            ('A', []) => self.move_up(arg1(0)),
            ('B' | 'e', []) => self.move_down(arg1(0)),
            ('C' | 'a', []) => {
                self.cursor.col = (self.cursor.col + arg1(0)).min(self.cols() - 1);
                self.cursor.pending_wrap = false;
            }
            ('D', []) => {
                self.cursor.col = self.cursor.col.saturating_sub(arg1(0));
                self.cursor.pending_wrap = false;
            }
            ('E', []) => {
                self.move_down(arg1(0));
                self.cursor.col = 0;
            }
            ('F', []) => {
                self.move_up(arg1(0));
                self.cursor.col = 0;
            }
            ('G' | '`', []) => {
                self.cursor.col = (arg1(0) - 1).min(self.cols() - 1);
                self.cursor.pending_wrap = false;
            }
            ('H' | 'f', []) => self.goto(arg1(0) - 1, arg1(1) - 1),
            ('I', []) => {
                for _ in 0..arg1(0) {
                    self.execute_byte(0x09);
                }
            }
            ('J', [] | [b'?']) => {
                let (row, col, rows) = (self.cursor.row, self.cursor.col, self.rows());
                match arg0(0) {
                    0 => {
                        self.erase_cells(row, col, usize::MAX);
                        self.erase_rows(row + 1, rows);
                    }
                    1 => {
                        self.erase_rows(0, row);
                        self.erase_cells(row, 0, col + 1);
                    }
                    2 => self.erase_rows(0, rows),
                    3 => {
                        self.grid_mut().clear_history();
                        self.display_offset = 0;
                    }
                    _ => {}
                }
            }
            ('K', [] | [b'?']) => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                match arg0(0) {
                    0 => self.erase_cells(row, col, usize::MAX),
                    1 => self.erase_cells(row, 0, col + 1),
                    2 => self.erase_cells(row, 0, usize::MAX),
                    _ => {}
                }
            }
            ('L', []) => self.insert_lines(arg1(0)),
            ('M', []) => self.delete_lines(arg1(0)),
            ('P', []) => self.delete_chars(arg1(0)),
            ('S', []) => self.scroll_up(arg1(0)),
            ('T', []) => self.scroll_down(arg1(0)),
            ('X', []) => {
                let (row, col) = (self.cursor.row, self.cursor.col);
                self.erase_cells(row, col, col + arg1(0));
            }
            ('Z', []) => {
                for _ in 0..arg1(0) {
                    let mut col = self.cursor.col;
                    while col > 0 {
                        col -= 1;
                        if self.tabs[col] {
                            break;
                        }
                    }
                    self.cursor.col = col;
                }
            }
            ('b', []) => {
                if let Some(c) = self.last_char {
                    for _ in 0..arg1(0).min(65535) {
                        self.print_char(c);
                    }
                }
            }
            ('c', []) if arg0(0) == 0 => self.reply_str("\x1b[?62;22;52c"),
            ('c', [b'>']) if arg0(0) == 0 => self.reply_str("\x1b[>1;10;0c"),
            ('d', []) => {
                let col = self.cursor.col;
                self.goto(arg1(0) - 1, col);
            }
            ('g', []) => match arg0(0) {
                0 => {
                    let col = self.cursor.col;
                    self.tabs[col] = false;
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            ('h', [] | [b'?']) => {
                for &mode in args {
                    self.set_mode(mode, private, true);
                }
            }
            ('l', [] | [b'?']) => {
                for &mode in args {
                    self.set_mode(mode, private, false);
                }
            }
            ('m', []) => self.sgr(params),
            ('m', [b'>']) => {
                // modifyOtherKeys: CSI > 4 ; n m。CSI > 4 m は既定に戻す。
                if arg0(0) == 4 {
                    self.modes.modify_other_keys = args.get(1).copied().unwrap_or(0).min(2) as u8;
                }
            }
            ('n', []) => match arg0(0) {
                5 => self.reply_str("\x1b[0n"),
                6 => {
                    let row = if self.modes.origin {
                        self.cursor.row - self.top
                    } else {
                        self.cursor.row
                    };
                    let s = format!("\x1b[{};{}R", row + 1, self.cursor.col + 1);
                    self.reply_str(&s);
                }
                _ => {}
            },
            ('p', [b'!']) => self.soft_reset(),
            ('p', [b'$']) | ('p', [b'?', b'$']) => {
                let private = intermediates[0] == b'?';
                let mode = arg0(0) as u16;
                let state = self.mode_state(mode, private);
                let q = if private { "?" } else { "" };
                self.reply_str(&format!("\x1b[{q}{mode};{state}$y"));
            }
            ('q', [b'>']) => {
                self.reply_str(concat!("\x1bP>|hull ", env!("CARGO_PKG_VERSION"), "\x1b\\"))
            }
            ('q', [b' ']) => {
                self.modes.cursor_shape = match arg0(0) {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
            }
            ('r', []) => {
                let rows = self.rows();
                let top = arg1(0) - 1;
                let bottom = args.get(1).copied().filter(|&v| v != 0).map_or(rows, usize::from) - 1;
                if top < bottom && bottom < rows {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            ('s', []) => self.save_cursor(),
            ('t', []) => self.csi_window_ops(args),
            ('u', []) => self.restore_cursor(),
            ('u', [b'?' | b'>' | b'<' | b'=']) => self.kitty_keyboard(intermediates, args),
            _ => {}
        }
    }

    fn esc(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match (byte, intermediates) {
            (b'7', []) => self.save_cursor(),
            (b'8', []) => self.restore_cursor(),
            (b'8', [b'#']) => {
                // DECALN: 画面を E で埋める (画面合わせ用)。
                let cols = self.cols();
                for row in 0..self.rows() {
                    let r = self.grid_mut().row_mut(row);
                    r.reset(cols, Cell { ch: 'E', ..Cell::BLANK });
                }
                self.goto(0, 0);
            }
            (b'D', []) => self.linefeed(),
            (b'E', []) => {
                self.linefeed();
                self.cursor.col = 0;
            }
            (b'H', []) => {
                let col = self.cursor.col;
                self.tabs[col] = true;
            }
            (b'M', []) => self.reverse_index(),
            (b'c', []) => self.full_reset(),
            (b'=', []) => self.modes.app_keypad = true,
            (b'>', []) => self.modes.app_keypad = false,
            (c, [i @ (b'(' | b')')]) => {
                let slot = if *i == b'(' { 0 } else { 1 };
                self.cursor.charsets[slot] =
                    if c == b'0' { Charset::DecGraphics } else { Charset::Ascii };
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: usize, rows: usize) -> Terminal {
        Terminal::new(cols, rows, 100)
    }

    fn screen(t: &Terminal) -> Vec<String> {
        (0..t.rows()).map(|r| t.grid().row(r).text(0, t.cols())).collect()
    }

    #[test]
    fn 文字を書いて折り返す() {
        let mut t = term(5, 3);
        t.advance(b"hello world");
        assert_eq!(screen(&t), ["hello", " worl", "d"]);
        assert!(t.grid().row(0).wrapped);
        assert_eq!(t.cursor(), (2, 1));
    }

    #[test]
    fn 改行で履歴に送る() {
        let mut t = term(5, 2);
        t.advance(b"a\r\nb\r\nc");
        assert_eq!(screen(&t), ["b", "c"]);
        assert_eq!(t.grid().history_len(), 1);
        assert_eq!(t.line_text(0).unwrap(), "a");
    }

    #[test]
    fn 全角は2マスを使い_行末では次の行へ() {
        let mut t = term(5, 2);
        t.advance("ab日本".as_bytes());
        let row = t.grid().row(0);
        assert_eq!(row.cells[2].ch, '日');
        assert!(row.cells[2].flags & flags::WIDE != 0);
        assert!(row.cells[3].flags & flags::WIDE_SPACER != 0);
        assert_eq!(screen(&t), ["ab日", "本"]);
    }

    #[test]
    fn 全角の片側を上書きするともう片側も消える() {
        let mut t = term(6, 1);
        t.advance("日本".as_bytes());
        t.advance(b"\x1b[2Gx");
        assert_eq!(screen(&t), [" x本"]);
    }

    #[test]
    fn sgr_の色と属性() {
        let mut t = term(10, 1);
        t.advance(b"\x1b[1;31mA\x1b[38;2;1;2;3;48;5;200mB\x1b[38:2::4:5:6mC\x1b[0mD");
        let row = t.grid().row(0);
        assert_eq!(row.cells[0].fg, Color::Indexed(1));
        assert!(row.cells[0].flags & flags::BOLD != 0);
        assert_eq!(row.cells[1].fg, Color::Rgb(1, 2, 3));
        assert_eq!(row.cells[1].bg, Color::Indexed(200));
        assert_eq!(row.cells[2].fg, Color::Rgb(4, 5, 6));
        assert_eq!(row.cells[3], Cell { ch: 'D', ..Cell::BLANK });
    }

    #[test]
    fn 消去は今の背景色で塗る() {
        let mut t = term(4, 2);
        t.advance(b"abcd\x1b[44m\x1b[2J");
        assert_eq!(t.grid().row(1).cells[0].bg, Color::Indexed(4));
    }

    #[test]
    fn スクロール範囲の中だけが動く() {
        let mut t = term(3, 4);
        t.advance(b"1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[3;1H\n");
        assert_eq!(screen(&t), ["1", "3", "", "4"]);
        assert_eq!(t.grid().history_len(), 0);
    }

    #[test]
    fn 代替画面から戻ると元の画面とカーソルに戻る() {
        let mut t = term(4, 2);
        t.advance(b"ab\x1b[?1049hxyz\x1b[?1049l");
        assert_eq!(screen(&t), ["ab", ""]);
        assert_eq!(t.cursor(), (0, 2));
    }

    #[test]
    fn 問い合わせに答える() {
        let mut t = term(10, 5);
        t.advance(b"\x1b[3;4H\x1b[6n\x1b[?2026$p\x1b[?u\x1b[>q");
        let reply = String::from_utf8(t.take_reply()).unwrap();
        assert_eq!(reply, "\x1b[3;4R\x1b[?2026;2$y\x1b[?0u\x1bP>|hull 0.3.0\x1b\\");
    }

    #[test]
    fn kitty_keyboard_のスタック() {
        let mut t = term(10, 5);
        t.advance(b"\x1b[>1u\x1b[>3u");
        assert_eq!(t.kitty_flags(), 3);
        t.advance(b"\x1b[=8;2u");
        assert_eq!(t.kitty_flags(), 11);
        t.advance(b"\x1b[<u");
        assert_eq!(t.kitty_flags(), 1);
        // 代替画面は別のスタックを持つ。
        t.advance(b"\x1b[?1049h");
        assert_eq!(t.kitty_flags(), 0);
    }

    #[test]
    fn osc_をイベントにする() {
        let mut t = term(10, 5);
        t.advance(b"\x1b]0;title\x07\x1b]7;file://host/tmp/a%20b\x1b\\\x07");
        t.advance(b"\x1b]133;D;1\x07\x1b]9;hi;there\x07\x1b]9;4;1;50\x07");
        t.advance(b"\x1b]777;notify;T;B\x07\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(
            t.take_events(),
            [
                Event::Title("title".into()),
                Event::Cwd("/tmp/a b".into()),
                Event::Bell,
                Event::Prompt(PromptMark::CommandEnd(Some(1))),
                Event::Notify { title: None, body: "hi;there".into() },
                Event::Progress { state: 1, value: 50 },
                Event::Notify { title: Some("T".into()), body: "B".into() },
                Event::Clipboard { primary: false, text: "hello".into() },
            ]
        );
    }

    #[test]
    fn 色の問い合わせ() {
        let mut t = term(10, 5);
        t.advance(b"\x1b]11;#102030\x07\x1b]11;?\x07");
        assert_eq!(t.take_reply(), b"\x1b]11;rgb:1010/2020/3030\x07");
    }

    #[test]
    fn ハイパーリンク() {
        let mut t = term(10, 1);
        t.advance(b"\x1b]8;;https://example.com\x1b\\ab\x1b]8;;\x1b\\c");
        let row = t.grid().row(0);
        assert_eq!(t.link(row.cells[0].link), Some("https://example.com"));
        assert_eq!(row.cells[2].link, 0);
    }

    #[test]
    fn 行の挿入と削除() {
        let mut t = term(3, 3);
        t.advance(b"a\r\nb\r\nc\x1b[2;1H\x1b[L");
        assert_eq!(screen(&t), ["a", "", "b"]);
        t.advance(b"\x1b[M");
        assert_eq!(screen(&t), ["a", "b", ""]);
    }

    #[test]
    fn 文字の挿入_削除_消去() {
        let mut t = term(6, 1);
        t.advance(b"abcdef\x1b[3G\x1b[2@");
        assert_eq!(screen(&t), ["ab  cd"]);
        t.advance(b"\x1b[2P");
        assert_eq!(screen(&t), ["abcd"]);
        t.advance(b"\x1b[1X");
        assert_eq!(screen(&t), ["ab d"]);
    }

    #[test]
    fn 行の線画の文字() {
        let mut t = term(4, 1);
        t.advance(b"\x1b(0lqk\x1b(Bq");
        assert_eq!(screen(&t), ["┌─┐q"]);
    }

    #[test]
    fn 結合文字は前の文字につく() {
        let mut t = term(4, 1);
        t.advance("e\u{301}x".as_bytes());
        assert_eq!(t.grid().row(0).text(0, 4), "e\u{301}x");
    }

    #[test]
    fn 大きさを変えても履歴と画面がつながる() {
        let mut t = term(4, 3);
        t.advance(b"1\r\n2\r\n3\r\n4");
        t.resize(4, 2);
        assert_eq!(screen(&t), ["3", "4"]);
        assert_eq!(t.cursor(), (1, 1));
        t.resize(4, 4);
        assert_eq!(screen(&t), ["1", "2", "3", "4"]);
        assert_eq!(t.cursor(), (3, 1));
    }

    #[test]
    fn 範囲の文字列() {
        let mut t = term(4, 3);
        t.advance(b"abcdef\r\nxy");
        let top = t.grid().abs_of_screen(0);
        assert_eq!(t.text_between((top, 1), (top + 2, 0)), "bcdef\nx");
    }
}

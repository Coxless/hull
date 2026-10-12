use std::collections::{BTreeMap, VecDeque};

/// 直近の出力をためるリングバッファ。
///
/// 再接続した UI に流し直して直前の画面を戻すために使う。あふれて捨てた部分に
/// 「全画面に切り替える」などのモード設定が入っていても画面が戻るよう、
/// 捨てたバイト列からモードの状態だけを追い続け、[`Ring::snapshot`] の先頭で復元する。
#[derive(Debug)]
pub struct Ring {
    buf: VecDeque<u8>,
    capacity: usize,
    /// 捨てたバイト列を読み終えた時点の状態。
    evicted: ModeTracker,
    /// あふれて、先頭を捨てたことがある。
    truncated: bool,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self { buf: VecDeque::new(), capacity, evicted: ModeTracker::default(), truncated: false }
    }

    pub fn push(&mut self, data: &[u8]) {
        let overflow = (self.buf.len() + data.len()).saturating_sub(self.capacity);
        let from_buf = overflow.min(self.buf.len());
        self.truncated |= overflow > 0;
        for byte in self.buf.drain(..from_buf) {
            self.evicted.feed(byte);
        }
        let from_data = overflow - from_buf;
        for &byte in &data[..from_data] {
            self.evicted.feed(byte);
        }
        self.buf.extend(&data[from_data..]);
    }

    /// 流し直すバイト列。捨てた部分で設定されたモードを先頭で復元し、
    /// 先頭が制御シーケンスや文字の途中なら、その切れ端は落とす。
    pub fn snapshot(&self) -> Vec<u8> {
        let mut tracker = self.evicted.clone();
        let mut skip = 0;
        for &byte in &self.buf {
            if skip >= MAX_SKIP || !tracker.is_mid_sequence(byte) {
                break;
            }
            tracker.feed(byte);
            skip += 1;
        }

        let mut out = tracker.restore_sequence();
        out.reserve(self.buf.len() - skip);
        out.extend(self.buf.iter().skip(skip));
        out
    }

    /// あふれて、先頭を捨てたことがあるか。
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// 切れ端として読み飛ばす長さの上限 (タイトル設定などの長い OSC でも画面を失わないように)。
const MAX_SKIP: usize = 4096;

/// 再接続で復元する DEC プライベートモード。
const TRACKED_MODES: &[u16] = &[
    1,  // カーソルキーのアプリケーションモード
    25, // カーソルの表示
    47, 1047, 1049, // 代替画面 (vim や less などの全画面アプリ)
    1000, 1002, 1003, 1004, 1005, 1006, 1015, // マウスとフォーカスの報告
    2004, // 括弧付き貼り付け
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
    /// OSC / DCS などの文字列。BEL か ST (`ESC \`) で終わる。
    String,
    StringEscape,
}

#[derive(Debug, Clone, Default)]
struct ModeTracker {
    state: State,
    private: bool,
    params: Vec<u16>,
    /// 最後に明示的に設定された値。
    modes: BTreeMap<u16, bool>,
    /// UTF-8 の複数バイト文字の途中なら、残りの続きバイト数。
    utf8_remaining: u8,
}

impl ModeTracker {
    fn feed(&mut self, byte: u8) {
        match self.state {
            State::Ground => match byte {
                0x1b => {
                    self.state = State::Escape;
                    self.utf8_remaining = 0;
                }
                0xC0..=0xDF => self.utf8_remaining = 1,
                0xE0..=0xEF => self.utf8_remaining = 2,
                0xF0..=0xF7 => self.utf8_remaining = 3,
                0x80..=0xBF => self.utf8_remaining = self.utf8_remaining.saturating_sub(1),
                _ => self.utf8_remaining = 0,
            },
            State::Escape => match byte {
                b'[' => {
                    self.state = State::Csi;
                    self.private = false;
                    self.params.clear();
                    self.params.push(0);
                }
                b']' | b'P' | b'^' | b'_' | b'X' => self.state = State::String,
                0x1b => {}
                // `ESC (` のような中間バイト付きは、終端バイトまで Escape のまま読む。
                0x20..=0x2F => {}
                _ => self.state = State::Ground,
            },
            State::Csi => match byte {
                b'?' => self.private = true,
                b'0'..=b'9' => {
                    if let Some(last) = self.params.last_mut() {
                        *last = last.saturating_mul(10).saturating_add(u16::from(byte - b'0'));
                    }
                }
                b';' | b':' => {
                    if self.params.len() < 32 {
                        self.params.push(0);
                    }
                }
                0x40..=0x7E => {
                    if self.private && (byte == b'h' || byte == b'l') {
                        for &param in &self.params {
                            if TRACKED_MODES.contains(&param) {
                                self.modes.insert(param, byte == b'h');
                            }
                        }
                    }
                    self.state = State::Ground;
                }
                0x1b => self.state = State::Escape,
                _ => {}
            },
            State::String => match byte {
                0x07 => self.state = State::Ground,
                0x1b => self.state = State::StringEscape,
                _ => {}
            },
            State::StringEscape => match byte {
                b'\\' => self.state = State::Ground,
                0x1b => {}
                _ => self.state = State::String,
            },
        }
    }

    /// 制御シーケンスや複数バイト文字の途中で、`next` がその続きか。
    fn is_mid_sequence(&self, next: u8) -> bool {
        match self.state {
            State::Ground => self.utf8_remaining > 0 && (0x80..0xC0).contains(&next),
            _ => true,
        }
    }

    /// 追ってきたモードを設定し直す制御シーケンス。
    fn restore_sequence(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (mode, on) in &self.modes {
            // 既定値と同じ設定は送らない。
            let default = *mode == 25;
            if *on != default {
                out.extend(format!("\x1b[?{mode}{}", if *on { 'h' } else { 'l' }).bytes());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_everything_while_under_capacity() {
        let mut ring = Ring::new(16);
        ring.push(b"hello ");
        ring.push(b"world");
        assert_eq!(ring.snapshot(), b"hello world");
        assert_eq!(ring.len(), 11);
        assert!(!ring.is_truncated());
    }

    #[test]
    fn drops_oldest_bytes_when_full() {
        let mut ring = Ring::new(8);
        ring.push(b"0123456789");
        assert_eq!(ring.snapshot(), b"23456789");
        assert!(ring.is_truncated());
        ring.push(b"ab");
        assert_eq!(ring.snapshot(), b"456789ab");
        ring.push(b"ABCDEFGHIJKLMNOP");
        assert_eq!(ring.snapshot(), b"IJKLMNOP");
    }

    #[test]
    fn restores_alternate_screen_entered_before_the_kept_part() {
        let mut ring = Ring::new(8);
        ring.push(b"\x1b[?1049h\x1b[?2004h");
        ring.push(b"vim screen contents");
        assert_eq!(ring.snapshot(), b"\x1b[?1049h\x1b[?2004hcontents");
    }

    #[test]
    fn does_not_restore_modes_that_were_reset() {
        let mut ring = Ring::new(4);
        ring.push(b"\x1b[?1049h..\x1b[?1049l\x1b[?25l\x1b[?25h");
        ring.push(b"$ ls");
        assert_eq!(ring.snapshot(), b"$ ls");
    }

    #[test]
    fn tracks_several_modes_in_one_sequence() {
        let mut ring = Ring::new(2);
        ring.push(b"\x1b[?1000;1006h\x1b[?25l");
        ring.push(b"ok");
        assert_eq!(ring.snapshot(), b"\x1b[?25l\x1b[?1000h\x1b[?1006hok");
    }

    #[test]
    fn skips_the_tail_of_a_control_sequence_cut_at_the_start() {
        let mut ring = Ring::new(9);
        // `\x1b[?1049h` の途中で切れて、残りの `049h` が先頭に来る。
        ring.push(b"\x1b[?1049hdone!");
        assert_eq!(ring.len(), 9);
        assert_eq!(ring.snapshot(), b"\x1b[?1049hdone!");
    }

    #[test]
    fn skips_the_tail_of_an_osc_title_cut_at_the_start() {
        let mut ring = Ring::new(10);
        ring.push(b"\x1b]0;my title\x07prompt");
        assert_eq!(ring.snapshot(), b"prompt");
    }

    #[test]
    fn skips_the_tail_of_a_multibyte_character_cut_at_the_start() {
        let mut ring = Ring::new(4);
        // 「あ」(3バイト) の2バイト目から残る。
        ring.push("あab".as_bytes());
        assert_eq!(ring.snapshot(), b"ab");
        let mut ring = Ring::new(5);
        ring.push("xあい".as_bytes());
        assert_eq!(ring.snapshot(), "い".as_bytes());
    }

    #[test]
    fn keeps_whole_multibyte_characters() {
        let mut ring = Ring::new(6);
        ring.push("あいう".as_bytes());
        assert_eq!(ring.snapshot(), "いう".as_bytes());
    }
}

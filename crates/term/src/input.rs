//! キーとマウスを、PTY に書くバイト列に変える。
//!
//! キーは、xterm の従来の形、modifyOtherKeys (tmux の extended-keys が使う)、
//! kitty keyboard protocol の 3 通り。どれを使うかは端末のモードで決まる。

use crate::term::{Modes, MouseMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// 文字キー。シフトを効かせる前の文字 (例: Shift+a でも 'a')。
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1〜F12。
    F(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub logo: bool,
}

impl Mods {
    /// xterm と kitty で共通の修飾キーの番号 (1 足す前)。
    fn bits(self) -> u8 {
        self.shift as u8 | (self.alt as u8) << 1 | (self.ctrl as u8) << 2 | (self.logo as u8) << 3
    }

    pub fn is_empty(self) -> bool {
        self.bits() == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

/// kitty keyboard protocol のフラグ。
pub mod kitty {
    pub const DISAMBIGUATE: u8 = 1;
    pub const REPORT_EVENTS: u8 = 2;
    pub const REPORT_ALTERNATES: u8 = 4;
    pub const ALL_AS_ESCAPES: u8 = 8;
    pub const ASSOCIATED_TEXT: u8 = 16;
}

/// キーを符号化する。`text` はキーで入る文字 (シフトなどを効かせたあと)。
/// 何も送らないときは `None`。
pub fn encode_key(
    key: Key,
    mods: Mods,
    action: KeyAction,
    text: Option<&str>,
    modes: &Modes,
    kitty_flags: u8,
) -> Option<Vec<u8>> {
    if kitty_flags != 0 {
        return encode_kitty(key, mods, action, text, modes, kitty_flags);
    }
    if action == KeyAction::Release {
        return None;
    }
    encode_legacy(key, mods, text, modes)
}

fn csi_mod(code: &str, mods: Mods, end: char) -> Vec<u8> {
    format!("\x1b[{code};{}{end}", mods.bits() + 1).into_bytes()
}

fn encode_legacy(key: Key, mods: Mods, text: Option<&str>, modes: &Modes) -> Option<Vec<u8>> {
    let m = mods.bits();
    let cursor = |c: char| -> Vec<u8> {
        if m != 0 {
            csi_mod("1", mods, c)
        } else if modes.app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if m != 0 { csi_mod(&n.to_string(), mods, '~') } else { format!("\x1b[{n}~").into_bytes() }
    };
    // modifyOtherKeys の形: CSI 27 ; mod ; code ~
    let other = |code: u32| format!("\x1b[27;{};{code}~", m + 1).into_bytes();
    let with_alt = |mut bytes: Vec<u8>| {
        if mods.alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };

    Some(match key {
        Key::Up => cursor('A'),
        Key::Down => cursor('B'),
        Key::Right => cursor('C'),
        Key::Left => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F(n @ 1..=4) => {
            let c = (b'P' + n - 1) as char;
            if m != 0 { csi_mod("1", mods, c) } else { format!("\x1bO{c}").into_bytes() }
        }
        Key::F(n) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => return None,
            };
            tilde(code)
        }
        Key::Enter if modes.modify_other_keys == 2 && (mods.ctrl || mods.shift) => other(13),
        Key::Enter => {
            with_alt(if modes.linefeed_newline { b"\r\n".to_vec() } else { b"\r".to_vec() })
        }
        Key::Tab if modes.modify_other_keys == 2 && mods.ctrl => other(9),
        Key::Tab if mods.shift => with_alt(b"\x1b[Z".to_vec()),
        Key::Tab => with_alt(b"\t".to_vec()),
        Key::Backspace if modes.modify_other_keys == 2 && (mods.ctrl || mods.shift) => other(127),
        Key::Backspace => with_alt(if mods.ctrl { vec![0x08] } else { vec![0x7f] }),
        Key::Escape if modes.modify_other_keys == 2 && m != 0 => other(27),
        Key::Escape => with_alt(vec![0x1b]),
        Key::Char(c) => {
            if modes.modify_other_keys == 2 && (mods.ctrl || mods.alt) && needs_other_keys(c, mods)
            {
                let code =
                    if mods.shift { text.and_then(|t| t.chars().next()).unwrap_or(c) } else { c };
                return Some(format!("\x1b[27;{};{}~", m + 1, code as u32).into_bytes());
            }
            if mods.ctrl
                && let Some(b) = ctrl_byte(c)
            {
                return Some(with_alt(vec![b]));
            }
            let text = text.filter(|t| !t.is_empty())?;
            // Ctrl で文字が変わらなかった (制御文字にならない) キーは、文字をそのまま送る。
            let bytes = text.as_bytes().to_vec();
            with_alt(bytes)
        }
    })
}

/// modifyOtherKeys=2 で CSI 27 の形にするか。従来の形で区別できるものは、そのまま送る。
fn needs_other_keys(c: char, mods: Mods) -> bool {
    if mods.ctrl && !mods.shift && ctrl_byte(c).is_some() && c.is_ascii_lowercase() {
        return false;
    }
    if mods.alt && !mods.ctrl {
        return false;
    }
    true
}

/// Ctrl+文字の制御文字。
fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

fn encode_kitty(
    key: Key,
    mods: Mods,
    action: KeyAction,
    text: Option<&str>,
    modes: &Modes,
    flags: u8,
) -> Option<Vec<u8>> {
    use kitty::*;
    let report_events = flags & REPORT_EVENTS != 0;
    let all_escapes = flags & ALL_AS_ESCAPES != 0;
    if action == KeyAction::Release && !report_events {
        return None;
    }
    let m = mods.bits();
    let event = match action {
        KeyAction::Press => 1,
        KeyAction::Repeat => 2,
        KeyAction::Release => 3,
    };

    // 従来の形のまま送れるもの (DISAMBIGUATE だけで、修飾なしの Enter / Tab / Backspace と文字)。
    if !all_escapes && action != KeyAction::Release {
        let plain_text = matches!(key, Key::Char(_)) && (m == 0 || m == 1);
        let plain_ctl = matches!(key, Key::Enter | Key::Tab | Key::Backspace) && m == 0;
        if plain_text || plain_ctl {
            return encode_legacy(key, mods, text, modes);
        }
    }

    let mod_field = |with_event: bool| -> String {
        if with_event && report_events && event != 1 {
            format!("{}:{event}", m + 1)
        } else if m != 0 {
            (m + 1).to_string()
        } else {
            String::new()
        }
    };

    // 矢印や F1〜F4 は CSI 1;mod X の形。
    let letter = match key {
        Key::Up => Some('A'),
        Key::Down => Some('B'),
        Key::Right => Some('C'),
        Key::Left => Some('D'),
        Key::Home => Some('H'),
        Key::End => Some('F'),
        Key::F(1) => Some('P'),
        Key::F(2) => Some('Q'),
        Key::F(4) => Some('S'),
        _ => None,
    };
    if let Some(c) = letter {
        let mf = mod_field(true);
        return Some(if mf.is_empty() {
            format!("\x1b[{c}").into_bytes()
        } else {
            format!("\x1b[1;{mf}{c}").into_bytes()
        });
    }

    let tilde = match key {
        Key::Insert => Some(2),
        Key::Delete => Some(3),
        Key::PageUp => Some(5),
        Key::PageDown => Some(6),
        Key::F(3) => Some(13),
        Key::F(5) => Some(15),
        Key::F(6) => Some(17),
        Key::F(7) => Some(18),
        Key::F(8) => Some(19),
        Key::F(9) => Some(20),
        Key::F(10) => Some(21),
        Key::F(11) => Some(23),
        Key::F(12) => Some(24),
        _ => None,
    };
    if let Some(n) = tilde {
        let mf = mod_field(true);
        return Some(if mf.is_empty() {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{mf}~").into_bytes()
        });
    }

    let code = match key {
        Key::Enter => 13,
        Key::Tab => 9,
        Key::Backspace => 127,
        Key::Escape => 27,
        Key::Char(c) => c as u32,
        _ => return None,
    };
    let mut mf = mod_field(true);
    let mut s = format!("\x1b[{code}");
    if flags & REPORT_ALTERNATES != 0
        && mods.shift
        && let Some(shifted) = text.and_then(|t| t.chars().next()).filter(|&s| s as u32 != code)
    {
        s.push_str(&format!(":{}", shifted as u32));
    }
    let assoc = (flags & ASSOCIATED_TEXT != 0 && action != KeyAction::Release)
        .then(|| text.filter(|t| t.chars().all(|c| !c.is_control())))
        .flatten();
    if let Some(t) = assoc {
        if mf.is_empty() {
            mf = "1".into();
        }
        let codes: Vec<String> = t.chars().map(|c| (c as u32).to_string()).collect();
        s.push_str(&format!(";{mf};{}", codes.join(":")));
    } else if !mf.is_empty() {
        s.push_str(&format!(";{mf}"));
    }
    s.push('u');
    Some(s.into_bytes())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    /// ボタンを押していない移動。
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// マウスの操作を符号化する。アプリがマウスを受け取らないときは `None`。
/// `col` と `row` は 0 始まり。
pub fn encode_mouse(
    button: MouseButton,
    action: MouseAction,
    mods: Mods,
    col: usize,
    row: usize,
    modes: &Modes,
) -> Option<Vec<u8>> {
    let wheel = matches!(
        button,
        MouseButton::WheelUp
            | MouseButton::WheelDown
            | MouseButton::WheelLeft
            | MouseButton::WheelRight
    );
    match (modes.mouse, action) {
        (MouseMode::None, _) => return None,
        (MouseMode::X10, MouseAction::Press) => {}
        (MouseMode::X10, _) => return None,
        (MouseMode::Normal, MouseAction::Motion) => return None,
        (MouseMode::Button, MouseAction::Motion) if button == MouseButton::None => return None,
        _ => {}
    }
    if wheel && action == MouseAction::Release {
        return None;
    }
    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
        MouseButton::WheelLeft => 66,
        MouseButton::WheelRight => 67,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    if modes.mouse != MouseMode::X10 {
        code += mods.shift as u32 * 4 + mods.alt as u32 * 8 + mods.ctrl as u32 * 16;
    }
    if modes.mouse_sgr {
        let end = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).into_bytes());
    }
    if action == MouseAction::Release {
        code = (code & !3) | 3;
    }
    // 従来の形は 1 バイトに入る 223 列までしか表せない。
    let (x, y) = (col + 1 + 32, row + 1 + 32);
    if x > 255 || y > 255 {
        return None;
    }
    Some(vec![0x1b, b'[', b'M', (code + 32) as u8, x as u8, y as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes() -> Modes {
        Modes::default()
    }

    fn key(k: Key, mods: Mods, text: Option<&str>, m: &Modes, kitty: u8) -> String {
        String::from_utf8(encode_key(k, mods, KeyAction::Press, text, m, kitty).unwrap()).unwrap()
    }

    const CTRL: Mods = Mods { ctrl: true, shift: false, alt: false, logo: false };
    const SHIFT: Mods = Mods { ctrl: false, shift: true, alt: false, logo: false };
    const ALT: Mods = Mods { ctrl: false, shift: false, alt: true, logo: false };

    #[test]
    fn 従来の形() {
        let m = modes();
        assert_eq!(key(Key::Char('a'), Mods::default(), Some("a"), &m, 0), "a");
        assert_eq!(key(Key::Char('c'), CTRL, Some("\x03"), &m, 0), "\x03");
        assert_eq!(key(Key::Char('x'), ALT, Some("x"), &m, 0), "\x1bx");
        assert_eq!(key(Key::Up, Mods::default(), None, &m, 0), "\x1b[A");
        assert_eq!(key(Key::Up, CTRL, None, &m, 0), "\x1b[1;5A");
        assert_eq!(key(Key::Tab, SHIFT, None, &m, 0), "\x1b[Z");
        assert_eq!(key(Key::F(5), Mods::default(), None, &m, 0), "\x1b[15~");
        assert_eq!(key(Key::F(1), Mods::default(), None, &m, 0), "\x1bOP");
        let mut app = modes();
        app.app_cursor = true;
        assert_eq!(key(Key::Left, Mods::default(), None, &app, 0), "\x1bOD");
    }

    #[test]
    fn modify_other_keys() {
        let mut m = modes();
        m.modify_other_keys = 2;
        assert_eq!(key(Key::Enter, SHIFT, None, &m, 0), "\x1b[27;2;13~");
        assert_eq!(key(Key::Char('a'), CTRL, Some("\x01"), &m, 0), "\x01");
        let ctrl_shift = Mods { ctrl: true, shift: true, ..Mods::default() };
        assert_eq!(key(Key::Char('a'), ctrl_shift, Some("A"), &m, 0), "\x1b[27;6;65~");
        assert_eq!(key(Key::Char('1'), CTRL, Some("1"), &m, 0), "\x1b[27;5;49~");
    }

    #[test]
    fn kitty_の形() {
        let m = modes();
        assert_eq!(key(Key::Escape, Mods::default(), None, &m, 1), "\x1b[27u");
        assert_eq!(key(Key::Char('a'), Mods::default(), Some("a"), &m, 1), "a");
        assert_eq!(key(Key::Char('a'), CTRL, Some("\x01"), &m, 1), "\x1b[97;5u");
        assert_eq!(key(Key::Enter, Mods::default(), None, &m, 1), "\r");
        assert_eq!(key(Key::Enter, SHIFT, None, &m, 1), "\x1b[13;2u");
        assert_eq!(key(Key::Up, CTRL, None, &m, 1), "\x1b[1;5A");
        assert_eq!(key(Key::Char('a'), Mods::default(), Some("a"), &m, 8), "\x1b[97u");
        assert_eq!(key(Key::Char('a'), Mods::default(), Some("a"), &m, 8 | 16), "\x1b[97;1;97u");
        let release = encode_key(Key::Char('a'), Mods::default(), KeyAction::Release, None, &m, 1);
        assert_eq!(release, None);
        let release =
            encode_key(Key::Char('a'), Mods::default(), KeyAction::Release, None, &m, 1 | 2 | 8);
        assert_eq!(release.unwrap(), b"\x1b[97;1:3u");
    }

    #[test]
    fn マウス() {
        let mut m = modes();
        assert_eq!(
            encode_mouse(MouseButton::Left, MouseAction::Press, Mods::default(), 0, 0, &m),
            None
        );
        m.mouse = MouseMode::Normal;
        m.mouse_sgr = true;
        let press = encode_mouse(MouseButton::Left, MouseAction::Press, Mods::default(), 4, 2, &m);
        assert_eq!(press.unwrap(), b"\x1b[<0;5;3M");
        let release = encode_mouse(MouseButton::Left, MouseAction::Release, CTRL, 4, 2, &m);
        assert_eq!(release.unwrap(), b"\x1b[<16;5;3m");
        assert_eq!(
            encode_mouse(MouseButton::Left, MouseAction::Motion, Mods::default(), 1, 1, &m),
            None
        );
        m.mouse_sgr = false;
        let press =
            encode_mouse(MouseButton::WheelUp, MouseAction::Press, Mods::default(), 0, 0, &m);
        assert_eq!(press.unwrap(), vec![0x1b, b'[', b'M', 96, 33, 33]);
    }
}

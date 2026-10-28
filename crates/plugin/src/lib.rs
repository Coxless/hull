//! hull のプラグインのプロトコル。
//!
//! プラグインは普通の実行ファイルで、hull が設定に書かれたものだけを起動する。
//! hull とは Unix ソケット (fd 3、環境変数 `HULL_PLUGIN_FD`) の上で、1 行に 1 つの JSON をやりとりする。
//!
//! - hull → プラグイン: [`HostEvent`] (`{"event": "bell"}` のように `event` で種類を表す)
//! - プラグイン → hull: [`Action`] (`{"action": "write", "data": "ls\r"}` のように `action` で種類を表す)
//!
//! hull は PTY の生のバイト列を流さず、解析済みのイベントだけを渡す。プラグインを待つこともない。
//! プラグインの読み出しが遅れて hull の列 (プラグインごとに上限つき) があふれたら、
//! 古いイベントから捨て、捨てた件数を [`HostEvent::Dropped`] で知らせる。
//!
//! 行は絶対番号で指す。出力が流れても、同じ行は同じ番号のまま。

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};

/// プロトコルの版。互換性のない変更をしたら上げる。
pub const PROTOCOL_VERSION: u32 = 1;

/// hull からプラグインへのイベント。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HostEvent {
    /// 起動したときに最初に届く。
    Hello {
        version: u32,
        cols: usize,
        rows: usize,
    },
    /// このプラグインに割り当てたキーバインドが押された (`plugin:<名前>/<name>`)。
    Keybind {
        name: String,
    },
    /// キー入力。[`Action::GrabKeys`] で受け取っているあいだだけ届く。
    /// `key` はキーの名前 ("a"、"enter"、"escape" など。IME で確定した文字は空)、`text` は入る文字。
    Key {
        key: String,
        text: String,
        mods: Vec<String>,
    },
    /// OSC 133 (プロンプトとコマンドの区切り)。
    Prompt {
        mark: PromptMark,
        exit_code: Option<i32>,
    },
    /// OSC 7 (今のディレクトリ)。
    Cwd {
        path: String,
    },
    /// OSC 9 / OSC 777 の通知。
    Notify {
        title: Option<String>,
        body: String,
    },
    /// OSC 9;4 (進捗表示)。
    Progress {
        state: u8,
        value: u8,
    },
    Bell,
    Title {
        title: String,
    },
    Focus {
        focused: bool,
    },
    /// マウスで文字を選んだ。
    Selection {
        text: String,
    },
    Resize {
        cols: usize,
        rows: usize,
    },
    /// 列があふれて捨てたイベントの数。
    Dropped {
        count: u64,
    },
    /// [`Action::ReadText`] の答え。
    Text {
        id: u64,
        text: TextResult,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptMark {
    PromptStart,
    CommandStart,
    OutputStart,
    CommandEnd,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct TextResult {
    /// `lines[0]` の絶対番号。
    pub first_line: u64,
    pub lines: Vec<String>,
    /// 見えている画面の一番上の行の絶対番号。
    pub view_top: u64,
    pub cols: usize,
    pub rows: usize,
    /// 全画面のアプリ (vim など) が代替画面を使っている。
    pub alt_screen: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSource {
    /// 見えている画面だけ。
    Screen,
    /// スクロールバックと画面のすべて。
    All,
}

/// 範囲 (行と、列の始めと終わり。終わりは含まない)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub line: u64,
    pub start: usize,
    pub end: usize,
}

/// オーバーレイに出す文字。`row` (見えている画面の行。負なら下から数える) か `line` (絶対番号) で位置を決める。
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct OverlayItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    pub col: usize,
    pub text: String,
    /// "#rrggbb"。省略すると黒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    /// "#rrggbb"。省略すると黄色。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
}

/// プラグインから hull への操作。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// PTY に書く (シェルへの入力になる)。
    Write {
        data: String,
    },
    /// キー入力を受け取る (`true`) か、返す (`false`)。受け取っているあいだ、キーは端末に行かない。
    GrabKeys {
        grab: bool,
    },
    /// 受け取ったキーを、端末に打ったことにする。
    ForwardKey {
        key: String,
        text: String,
        mods: Vec<String>,
    },
    /// 画面やスクロールバックの文字を読む。答えは [`HostEvent::Text`] で届く。
    ReadText {
        id: u64,
        source: TextSource,
    },
    /// 範囲をハイライトする。同じ `id` で送り直すと置き換わる。
    Highlight {
        id: String,
        ranges: Vec<Range>,
        color: String,
    },
    /// ハイライトを消す。`id` を省くと、このプラグインのものをすべて消す。
    ClearHighlight {
        #[serde(default)]
        id: Option<String>,
    },
    /// オーバーレイに文字を出す。同じ `id` で送り直すと置き換わる。
    Overlay {
        id: String,
        items: Vec<OverlayItem>,
    },
    ClearOverlay {
        #[serde(default)]
        id: Option<String>,
    },
    /// 行が見えるようにスクロールする。`line` を省くと一番下へ戻る。
    ScrollTo {
        #[serde(default)]
        line: Option<u64>,
    },
    /// 窓に注意を向けさせる (xdg-activation)。
    Attention,
    Clipboard {
        text: String,
        #[serde(default)]
        primary: bool,
    },
    OpenUrl {
        url: String,
    },
    /// 窓の題名の頭に印をつける (`None` で外す)。
    TitleMark {
        #[serde(default)]
        mark: Option<String>,
    },
}

/// 文字の幅 (hull の画面と同じ数え方)。
pub fn char_width(c: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    if (c as u32) < 0x7f {
        return 1;
    }
    c.width().unwrap_or(0)
}

/// 行の文字列の `byte` バイト目が、画面の何列目か。
pub fn col_of(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().map(char_width).sum()
}

/// 文字列の幅 (列)。
pub fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// プラグインの側で hull とつなぐ。
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    /// 答えを待つあいだに届いたイベント。
    backlog: VecDeque<HostEvent>,
}

impl Client {
    /// hull から渡されたソケットにつなぐ。
    pub fn connect() -> io::Result<Client> {
        let fd: i32 =
            std::env::var("HULL_PLUGIN_FD").ok().and_then(|v| v.parse().ok()).ok_or_else(|| {
                io::Error::other("HULL_PLUGIN_FD がありません (hull から起動してください)")
            })?;
        // SAFETY: hull がこのプロセスに渡した fd を、ここで 1 度だけ受け取る。
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        Client::from_stream(stream)
    }

    pub fn from_stream(stream: UnixStream) -> io::Result<Client> {
        let writer = stream.try_clone()?;
        Ok(Client { reader: BufReader::new(stream), writer, next_id: 1, backlog: VecDeque::new() })
    }

    fn read_event(&mut self) -> io::Result<Option<HostEvent>> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(event) => return Ok(Some(event)),
                // 知らないイベント (hull のほうが新しい) は読み飛ばす。
                Err(_) => continue,
            }
        }
    }

    /// 次のイベントを待つ。hull が終わったら `None`。
    pub fn next_event(&mut self) -> io::Result<Option<HostEvent>> {
        if let Some(event) = self.backlog.pop_front() {
            return Ok(Some(event));
        }
        self.read_event()
    }

    pub fn send(&mut self, action: &Action) -> io::Result<()> {
        let mut line = serde_json::to_string(action).map_err(io::Error::other)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())
    }

    /// 文字を読み、答えが届くまで待つ。
    pub fn read_text(&mut self, source: TextSource) -> io::Result<TextResult> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Action::ReadText { id, source })?;
        loop {
            match self.read_event()? {
                Some(HostEvent::Text { id: got, text }) if got == id => return Ok(text),
                Some(other) => self.backlog.push_back(other),
                None => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn イベントの形() {
        let json = serde_json::to_string(&HostEvent::Keybind { name: "search".into() }).unwrap();
        assert_eq!(json, r#"{"event":"keybind","name":"search"}"#);
        let json = serde_json::to_string(&HostEvent::Bell).unwrap();
        assert_eq!(json, r#"{"event":"bell"}"#);
    }

    #[test]
    fn アクションを読む() {
        let a: Action = serde_json::from_str(r#"{"action":"write","data":"ls\r"}"#).unwrap();
        assert_eq!(a, Action::Write { data: "ls\r".into() });
        let a: Action = serde_json::from_str(r#"{"action":"clear_highlight"}"#).unwrap();
        assert_eq!(a, Action::ClearHighlight { id: None });
        let a: Action = serde_json::from_str(r#"{"action":"scroll_to"}"#).unwrap();
        assert_eq!(a, Action::ScrollTo { line: None });
    }

    #[test]
    fn 列の数え方() {
        assert_eq!(col_of("日本語abc", "日本語".len()), 6);
        assert_eq!(width("aあ"), 3);
    }

    #[test]
    fn 答えを待つあいだのイベントは取っておく() {
        let (host, plugin) = UnixStream::pair().unwrap();
        let mut client = Client::from_stream(plugin).unwrap();
        let mut host_reader = BufReader::new(host.try_clone().unwrap());
        let mut host_writer = host;
        let t = std::thread::spawn(move || {
            let mut line = String::new();
            host_reader.read_line(&mut line).unwrap();
            let action: Action = serde_json::from_str(&line).unwrap();
            let Action::ReadText { id, .. } = action else { panic!() };
            writeln!(host_writer, r#"{{"event":"bell"}}"#).unwrap();
            let text = HostEvent::Text { id, text: TextResult::default() };
            writeln!(host_writer, "{}", serde_json::to_string(&text).unwrap()).unwrap();
        });
        client.read_text(TextSource::Screen).unwrap();
        t.join().unwrap();
        assert_eq!(client.next_event().unwrap(), Some(HostEvent::Bell));
    }
}

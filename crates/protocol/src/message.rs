use serde::{Deserialize, Serialize};

/// 互換性のない変更をしたときだけ上げる。
pub const PROTOCOL_VERSION: u32 = 1;

/// UI からデーモンへのメッセージ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// 接続して最初に送る。
    Hello { version: u32 },
    /// 画面をつなぐ。デーモンは PTY をこの大きさにして `Replay` を返す。
    Attach { cols: u16, rows: u16 },
    /// キー入力のバイト列。
    Input {
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// 画面の大きさが変わった。
    Resize { cols: u16, rows: u16 },
    /// ウィンドウを閉じるときの切断の通知。bash は残る。
    Detach,
}

/// デーモンから UI へのメッセージ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    /// `Hello` への応答。
    Welcome {
        version: u32,
        daemon_version: String,
        /// bash の PID。取得できない環境では `None`。
        pid: Option<u32>,
    },
    /// `Attach` への応答。直前の画面を戻すための直近の出力。
    Replay {
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// PTY の出力のバイト列。
    Output {
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// bash が終了した。このあとデーモンも終了する。
    Exited { code: u32 },
    /// 続けられない誤り (バージョン違いなど)。このあと切断する。
    Error { message: String },
}

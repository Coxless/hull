use serde::{Deserialize, Serialize};

/// 互換性のない変更をしたときだけ上げる。
pub const PROTOCOL_VERSION: u32 = 2;

/// UI からデーモンへのメッセージ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// 接続して最初に送る。
    Hello { version: u32 },
    /// 画面をつなぐ。デーモンは PTY をこの大きさにする。
    ///
    /// ブロックの表示では `History` が返り、実行中のブロックがあれば、そのここまでの出力が
    /// `Replay` で続く。生の表示 (`raw`) では、直前の画面を戻す `Replay` だけが返る。
    Attach {
        cols: u16,
        rows: u16,
        /// 区切りを通さず、bash の出力をそのまま受け取る。
        #[serde(default)]
        raw: bool,
    },
    /// キー入力のバイト列。
    Input {
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// 画面の大きさが変わった。
    Resize { cols: u16, rows: u16 },
    /// ウィンドウを閉じるときの切断の通知。bash は残る。
    Detach,
    /// 入力欄のテキストを実行する。プロンプトで入力を待っているときだけ受け付ける。
    Run { text: String },
    /// 終了したブロックの出力を取る。`BlockOutput` が返る。
    FetchOutput { id: u64 },
}

/// ブロック1つ。コマンドと出力の1往復。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockInfo {
    /// セッションの中で 1 から順に付く番号。
    pub id: u64,
    /// 実行したテキスト。入力欄を通さずに実行されたコマンドでは空。
    /// `None` は、コマンドの外で出た出力 (`.bashrc` の出力など)。
    pub command: Option<String>,
    /// 実行したときのディレクトリ。
    pub cwd: Option<String>,
    /// 開始時刻 (Unix エポックからのミリ秒)。
    pub started_at: u64,
    /// 実行したときの画面の大きさ。出力はこの幅で表示する。
    pub cols: u16,
    pub rows: u16,
}

/// 終わったブロックの結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockResult {
    /// 終了コード。わからなかったときは `None`。
    pub exit_code: Option<i32>,
    /// 終了時刻 (Unix エポックからのミリ秒)。
    pub finished_at: u64,
    /// 代替画面を使った (vim などの全画面アプリ)。
    pub fullscreen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedBlock {
    pub info: BlockInfo,
    pub result: BlockResult,
}

/// bash の今の場所。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromptState {
    pub cwd: Option<String>,
    /// git ブランチ。リポジトリの外では `None`。
    pub branch: Option<String>,
}

/// `Run` を実行しなかった理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunRejection {
    /// 入力が途中 (引用符が閉じていないなど)。bash には取り消しを送ってある。
    Incomplete,
    /// コマンドの実行中か、bash がまだプロンプトを出していない。
    Busy,
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
        /// bash が統合スクリプトを読み込んでいて、コマンドごとに区切れる。
        #[serde(default)]
        integrated: bool,
    },
    /// 直前の画面を戻すための出力。生の表示では直近の出力、ブロックの表示では
    /// 実行中のブロックのここまでの出力。
    Replay {
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// 実行中のブロックの出力のバイト列。生の表示では、bash のすべての出力。
    Output {
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// bash が終了した。このあとデーモンも終了する。
    Exited {
        code: u32,
    },
    /// 続けられない誤り (バージョン違いなど)。このあと切断する。
    Error {
        message: String,
    },
    /// ブロックの表示での `Attach` への応答。出力は含めない (`FetchOutput` で取る)。
    History {
        blocks: Vec<FinishedBlock>,
        /// 実行中のブロック。
        running: Option<BlockInfo>,
        prompt: PromptState,
        /// プロンプトで入力を待っていて、`Run` を受け付けられる。
        ready: bool,
    },
    /// プロンプトが出て、`Run` を受け付けられる。
    Prompt {
        prompt: PromptState,
    },
    /// ブロックが始まった。続く `Output` はこのブロックの出力。
    BlockStarted {
        block: BlockInfo,
    },
    BlockFinished {
        id: u64,
        result: BlockResult,
    },
    /// `FetchOutput` への応答。
    BlockOutput {
        id: u64,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
        /// 出力の先頭を省いた (長すぎたか、古いブロックで捨てた)。
        truncated: bool,
    },
    /// `Run` を実行しなかった。`text` は送られてきたテキスト。
    RunRejected {
        reason: RunRejection,
        text: String,
    },
}

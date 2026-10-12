//! 結合テスト用の小さなクライアント。

// テストのファイルごとに使う部品が違うので、使われないものがあってよい。
#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use hull_terminal_daemon::{run, Config, ShellSpec};
use hull_terminal_protocol::{
    decode, read_frame,
    transport::{self, LocalRecvHalf, LocalSendHalf},
    write_frame, ClientMessage, ServerMessage, PROTOCOL_VERSION,
};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};

/// bash の起動も含めて待つ時間。
pub const TIMEOUT: Duration = Duration::from_secs(60);

pub struct Daemon {
    pub endpoint: String,
    pub handle: JoinHandle<anyhow::Result<u32>>,
    dir: tempfile::TempDir,
}

/// テストごとに別の接続先でデーモンを起動する。
pub fn start_daemon() -> Daemon {
    start_daemon_with(1024 * 1024)
}

pub fn start_daemon_with(ring_capacity: usize) -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = test_endpoint(&dir);
    let config = Config {
        endpoint: endpoint.clone(),
        shell: Some(test_shell()),
        ring_capacity,
        ..Config::default()
    };
    Daemon { endpoint, handle: tokio::spawn(run(config)), dir }
}

/// 統合スクリプトを読み込む bash で、デーモンを起動する。
///
/// 開発者の設定に左右されないよう、ホームディレクトリを空の場所に移し、
/// そこに `bashrc` の内容の `.bashrc` を置く。
pub fn start_integrated_daemon(bashrc: &str) -> Daemon {
    start_integrated_daemon_with(bashrc, |config| config)
}

/// [`start_integrated_daemon`] と同じ。`adjust` で設定を変えられる。
pub fn start_integrated_daemon_with(bashrc: &str, adjust: impl FnOnce(Config) -> Config) -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join(".bashrc"), bashrc).unwrap();

    let mut shell = ShellSpec::detect().expect("bash が見つかりません");
    shell.args = vec!["--noprofile".into(), "-i".into()];
    shell.env = vec![("HOME".into(), home.to_string_lossy().into_owned())];

    let endpoint = test_endpoint(&dir);
    let config =
        adjust(Config { endpoint: endpoint.clone(), shell: Some(shell), ..Config::default() });
    Daemon { endpoint, handle: tokio::spawn(run(config)), dir }
}

impl Daemon {
    /// テスト用のホームディレクトリ ([`start_integrated_daemon`] で起動したとき)。
    pub fn home(&self) -> std::path::PathBuf {
        self.dir.path().join("home")
    }
}

pub fn test_endpoint(dir: &tempfile::TempDir) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.path().join(format!("daemon-{n}.sock")).to_string_lossy().into_owned()
}

/// 開発者の `.bashrc` に左右されないよう、設定も統合スクリプトも読まずに起動する。
pub fn test_shell() -> ShellSpec {
    let mut shell = ShellSpec::detect().expect("bash が見つかりません");
    shell.args = vec!["--norc".into(), "--noprofile".into(), "-i".into()];
    shell.integration = false;
    shell
}

pub struct Client {
    recv: LocalRecvHalf,
    send: LocalSendHalf,
    /// 受け取った出力 (Replay と Output) をつなげたもの。
    pub output: Vec<u8>,
}

impl Client {
    /// デーモンが待ち受けを始めるまで繰り返し接続を試みる。
    pub async fn connect(endpoint: &str) -> Self {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match transport::connect(endpoint).await {
                Ok((recv, send)) => return Self { recv, send, output: Vec::new() },
                Err(e) if Instant::now() > deadline => panic!("接続できません: {e}"),
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    }

    /// Hello を送り、Welcome の PID を返す。
    pub async fn hello(&mut self) -> Option<u32> {
        self.send(ClientMessage::Hello { version: PROTOCOL_VERSION }).await;
        match self.next_message().await {
            Some(ServerMessage::Welcome { version, pid, .. }) => {
                assert_eq!(version, PROTOCOL_VERSION);
                pid
            }
            other => panic!("Welcome が来るはずが {other:?}"),
        }
    }

    /// Attach を送り、Replay の中身を返す。
    pub async fn attach(&mut self, cols: u16, rows: u16) -> Vec<u8> {
        self.send(ClientMessage::Attach { cols, rows, raw: true }).await;
        match self.next_message().await {
            Some(ServerMessage::Replay { data }) => {
                self.output.extend(&data);
                data
            }
            other => panic!("Replay が来るはずが {other:?}"),
        }
    }

    pub async fn connect_and_attach(endpoint: &str) -> Self {
        let mut client = Self::connect(endpoint).await;
        client.hello().await;
        client.attach(80, 24).await;
        client
    }

    pub async fn send(&mut self, message: ClientMessage) {
        write_frame(&mut self.send, &message).await.unwrap();
        self.send.flush().await.unwrap();
    }

    /// 1行ぶんのキー入力を送る (Enter は CR)。
    pub async fn type_line(&mut self, line: &str) {
        self.send(ClientMessage::Input { data: format!("{line}\r").into_bytes() }).await;
    }

    /// キー入力のバイト列をそのまま送る。
    pub async fn send_bytes(&mut self, data: &[u8]) {
        self.send(ClientMessage::Input { data: data.to_vec() }).await;
    }

    pub async fn next_message(&mut self) -> Option<ServerMessage> {
        loop {
            let body = timeout(TIMEOUT, read_frame(&mut self.recv))
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "時間内にメッセージが届きません。これまでの出力:\n{}",
                        String::from_utf8_lossy(&self.output)
                    )
                })
                .unwrap()?;
            if let Ok(message) = decode(&body) {
                return Some(message);
            }
        }
    }

    /// `needle` が出力に現れるまで読み、現れた位置の直後までの出力を返す。
    pub async fn read_until(&mut self, needle: &str) -> String {
        let mut searched = 0;
        loop {
            if let Some(pos) = find(&self.output[searched..], needle.as_bytes()) {
                let end = searched + pos + needle.len();
                return String::from_utf8_lossy(&self.output[..end]).into_owned();
            }
            searched = self.output.len().saturating_sub(needle.len());
            match self.next_message().await {
                Some(ServerMessage::Output { data } | ServerMessage::Replay { data }) => {
                    self.output.extend(&data)
                }
                other => panic!(
                    "{needle:?} を待っている間に {other:?} が届きました。これまでの出力:\n{}",
                    String::from_utf8_lossy(&self.output)
                ),
            }
        }
    }

    /// Exited が届くまで読み、終了コードを返す。
    pub async fn read_until_exit(&mut self) -> u32 {
        loop {
            match self.next_message().await {
                Some(ServerMessage::Output { data }) => self.output.extend(&data),
                Some(ServerMessage::Exited { code }) => return code,
                other => panic!("Exited が来るはずが {other:?}"),
            }
        }
    }

    pub fn clear_output(&mut self) {
        self.output.clear();
    }
}

pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// `text` の中の `prefix<数字>suffix` から数字を取り出す。
pub fn number_between(text: &str, prefix: &str, suffix: &str) -> Option<u64> {
    text.match_indices(prefix).find_map(|(start, _)| {
        let rest = &text[start + prefix.len()..];
        let end = rest.find(suffix)?;
        rest[..end].parse().ok()
    })
}

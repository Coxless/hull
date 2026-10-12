//! フロントとデーモンの間の中継。
//!
//! 出力は Channel でバイト列のままフロントへ渡し、ブロックの始まりや終わりなどの知らせは
//! JSON で渡す。入力・リサイズ・実行するテキストはコマンドで受けてデーモンへ送る。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hull_terminal_protocol::{
    decode, default_endpoint, read_frame, transport::LocalRecvHalf, transport::LocalSendHalf,
    write_frame, BlockInfo, BlockResult, ClientMessage, FinishedBlock, PromptState, RunRejection,
    ServerMessage, PROTOCOL_VERSION,
};
use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request, Response};
use tauri::State;
use tokio::io::{AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::daemon;

/// フロントがまだ描いていない出力がこれを超えたら、デーモンからの読み取りを止める。
/// 止めている間は PTY が詰まり、出力しているコマンドが待たされる (`yes` などで UI を固めないため)。
const HIGH_WATER: usize = 2 * 1024 * 1024;
/// 続けて届いた出力は、この大きさまで1回にまとめてフロントへ渡す。
const MAX_BATCH: usize = 256 * 1024;
/// デーモンの応答 (Welcome) を待つ時間。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// 終了したブロックの出力が返ってくるまで待つ時間。
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct PtyState {
    connection: Mutex<Option<Connection>>,
}

struct Connection {
    outgoing: mpsc::UnboundedSender<ClientMessage>,
    flow: Arc<Flow>,
    fetches: Arc<Fetches>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
}

/// フロントがまだ描き終えていない出力の量。
#[derive(Default)]
struct Flow {
    unacked: Mutex<usize>,
    acked: Notify,
}

impl Flow {
    fn sent(&self, bytes: usize) {
        *lock(&self.unacked) += bytes;
    }

    fn ack(&self, bytes: usize) {
        let mut unacked = lock(&self.unacked);
        *unacked = unacked.saturating_sub(bytes);
        drop(unacked);
        self.acked.notify_waiters();
    }

    async fn wait_for_room(&self) {
        loop {
            // 確かめる前に待ち受けを登録して、その間の ack を取りこぼさないようにする。
            let acked = self.acked.notified();
            tokio::pin!(acked);
            acked.as_mut().enable();
            if *lock(&self.unacked) < HIGH_WATER {
                return;
            }
            acked.await;
        }
    }
}

/// 終了したブロックの出力。
struct FetchedOutput {
    data: Vec<u8>,
    truncated: bool,
}

/// `block_output` が待っている応答。ブロックの番号ごとに持つ。
#[derive(Default)]
struct Fetches {
    waiting: Mutex<HashMap<u64, Vec<oneshot::Sender<FetchedOutput>>>>,
}

impl Fetches {
    fn wait(&self, id: u64) -> oneshot::Receiver<FetchedOutput> {
        let (tx, rx) = oneshot::channel();
        lock(&self.waiting).entry(id).or_default().push(tx);
        rx
    }

    fn resolve(&self, id: u64, data: Vec<u8>, truncated: bool) {
        // 同じブロックを続けて頼まれていたら、応答も同じ数だけ届く。先に待っていたほうから返す。
        let mut waiting = lock(&self.waiting);
        let Some(queue) = waiting.get_mut(&id) else { return };
        let waiter = queue.remove(0);
        if queue.is_empty() {
            waiting.remove(&id);
        }
        let _ = waiter.send(FetchedOutput { data, truncated });
    }

    /// 接続が切れた。待っているものをすべて失敗させる。
    fn clear(&self) {
        lock(&self.waiting).clear();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl PtyState {
    fn send(&self, message: ClientMessage) -> Result<(), String> {
        let guard = lock(&self.connection);
        guard.as_ref().ok_or("デーモンにつながっていません")?.send(message)
    }

    /// 終了したブロックの出力を頼み、応答の受け口を返す。
    fn fetch(&self, id: u64) -> Result<oneshot::Receiver<FetchedOutput>, String> {
        let guard = lock(&self.connection);
        let connection = guard.as_ref().ok_or("デーモンにつながっていません")?;
        connection.request_output(id)
    }

    fn replace(&self, connection: Option<Connection>) -> Option<Connection> {
        std::mem::replace(&mut *lock(&self.connection), connection)
    }

    /// Detach を送って切断する。bash はデーモンの下に残る。
    pub async fn detach(&self) {
        if let Some(connection) = self.replace(None) {
            connection.close().await;
        }
    }
}

impl Connection {
    fn send(&self, message: ClientMessage) -> Result<(), String> {
        self.outgoing.send(message).map_err(|_| "デーモンとの接続が切れました".to_string())
    }

    fn request_output(&self, id: u64) -> Result<oneshot::Receiver<FetchedOutput>, String> {
        // 応答を取りこぼさないよう、頼む前に受け口を用意する。
        let waiter = self.fetches.wait(id);
        self.send(ClientMessage::FetchOutput { id })?;
        Ok(waiter)
    }

    async fn close(self) {
        self.reader.abort();
        let _ = self.outgoing.send(ClientMessage::Detach);
        // 送信口を閉じると、書き込みタスクは残りを送りきって終わる。
        drop(self.outgoing);
        let _ = tokio::time::timeout(Duration::from_millis(500), self.writer).await;
    }
}

/// フロントに渡す、出力以外の知らせ。
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum Control {
    /// 次に渡すバイト列は、直前の画面を戻すための出力 (デーモンの `Replay`)。
    Replay,
    /// ブロックの表示でつないだときの、今の状態。
    History {
        blocks: Vec<FinishedView>,
        running: Option<BlockView>,
        prompt: PromptView,
        ready: bool,
    },
    /// プロンプトが出て、実行するテキストを受け付けられる。
    Prompt {
        prompt: PromptView,
    },
    /// ブロックが始まった。続くバイト列は、このブロックの出力。
    BlockStarted {
        block: BlockView,
    },
    BlockFinished {
        id: u64,
        result: ResultView,
    },
    /// 送ったテキストは実行されなかった。
    RunRejected {
        reason: RejectionView,
        text: String,
    },
    /// bash が終了した。
    Exited {
        code: u32,
    },
    /// デーモンとの接続が切れた。
    Disconnected {
        reason: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BlockView {
    id: u64,
    command: Option<String>,
    cwd: Option<String>,
    started_at: u64,
    cols: u16,
    rows: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResultView {
    exit_code: Option<i32>,
    finished_at: u64,
    fullscreen: bool,
}

#[derive(Serialize)]
struct FinishedView {
    info: BlockView,
    result: ResultView,
}

#[derive(Serialize)]
struct PromptView {
    cwd: Option<String>,
    branch: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum RejectionView {
    Incomplete,
    Busy,
}

impl From<BlockInfo> for BlockView {
    fn from(block: BlockInfo) -> Self {
        let BlockInfo { id, command, cwd, started_at, cols, rows } = block;
        Self { id, command, cwd, started_at, cols, rows }
    }
}

impl From<BlockResult> for ResultView {
    fn from(result: BlockResult) -> Self {
        let BlockResult { exit_code, finished_at, fullscreen } = result;
        Self { exit_code, finished_at, fullscreen }
    }
}

impl From<FinishedBlock> for FinishedView {
    fn from(block: FinishedBlock) -> Self {
        Self { info: block.info.into(), result: block.result.into() }
    }
}

impl From<PromptState> for PromptView {
    fn from(prompt: PromptState) -> Self {
        Self { cwd: prompt.cwd, branch: prompt.branch }
    }
}

fn control(message: &Control) -> InvokeResponseBody {
    InvokeResponseBody::Json(serde_json::to_string(message).unwrap_or_else(|_| "null".into()))
}

/// フロントへの送り先。
trait EventSink: Send + Sync + 'static {
    /// 送れなければ (フロントがいなくなっていれば) false。
    fn deliver(&self, body: InvokeResponseBody) -> bool;
}

impl EventSink for Channel<InvokeResponseBody> {
    fn deliver(&self, body: InvokeResponseBody) -> bool {
        self.send(body).is_ok()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachInfo {
    daemon_version: String,
    pid: Option<u32>,
    /// この接続のためにデーモンを起動したか。false なら、動いていた bash に再接続した。
    started_daemon: bool,
    /// bash が統合スクリプトを読み込んでいて、コマンドごとに区切れる。
    integrated: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppOptions {
    /// 描画方法 (`webgl` / `dom`)。`None` なら WebGL を試し、使えなければ DOM にする。
    renderer: Option<String>,
    /// 生の表示で始める。
    raw: bool,
}

/// 描画方法を指定する環境変数 (`webgl` / `dom`)。
const RENDERER_ENV: &str = "HULL_TERMINAL_RENDERER";
/// 表示を指定する環境変数。`raw` なら、区切りを通さない生の表示で始める。
const MODE_ENV: &str = "HULL_TERMINAL_MODE";

#[tauri::command]
pub fn app_options() -> AppOptions {
    let renderer = std::env::var(RENDERER_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| (!daemon::gpu_available()).then(|| "dom".to_string()));
    let raw = std::env::var(MODE_ENV).is_ok_and(|mode| mode == "raw");
    AppOptions { renderer, raw }
}

/// デーモンに接続し (いなければ起動し)、画面をつなぐ。
///
/// `raw` が false ならブロックの表示でつなぐ。最初に `history` が届き、そのあとブロックの
/// 始まりと終わりの知らせと、実行中のブロックの出力 (バイト列) が流れる。
/// `raw` が true なら、bash のすべての出力がバイト列で流れる。
/// どちらでも、直前の画面を戻すための出力は、`replay` の知らせの次のバイト列で届く。
#[tauri::command]
pub async fn pty_attach(
    state: State<'_, PtyState>,
    cols: u16,
    rows: u16,
    raw: bool,
    on_event: Channel<InvokeResponseBody>,
) -> Result<AttachInfo, String> {
    // 前の接続 (画面の再読み込みや、表示の切り替えの前のもの) が残っていれば切る。
    state.detach().await;

    let endpoint = default_endpoint();
    let daemon::Connected { recv, send, started_daemon } =
        daemon::connect_or_spawn(&endpoint).await?;
    let (welcome, connection) = connect(recv, send, cols, rows, raw, on_event).await?;
    state.replace(Some(connection));

    Ok(AttachInfo {
        daemon_version: welcome.daemon_version,
        pid: welcome.pid,
        started_daemon,
        integrated: welcome.integrated,
    })
}

struct Welcome {
    daemon_version: String,
    pid: Option<u32>,
    integrated: bool,
}

/// つながったソケットであいさつを済ませ、中継を始める。
async fn connect(
    recv: LocalRecvHalf,
    send: LocalSendHalf,
    cols: u16,
    rows: u16,
    raw: bool,
    sink: impl EventSink,
) -> Result<(Welcome, Connection), String> {
    let mut recv = BufReader::with_capacity(MAX_BATCH, recv);
    let mut send = BufWriter::new(send);

    let io = |e| format!("デーモンとの通信に失敗しました: {e}");
    write_frame(&mut send, &ClientMessage::Hello { version: PROTOCOL_VERSION })
        .await
        .map_err(io)?;
    send.flush().await.map_err(|e| io(e.into()))?;

    let welcome = tokio::time::timeout(HANDSHAKE_TIMEOUT, welcome(&mut recv))
        .await
        .map_err(|_| "デーモンが応答しません".to_string())??;

    write_frame(&mut send, &ClientMessage::Attach { cols, rows, raw }).await.map_err(io)?;
    send.flush().await.map_err(|e| io(e.into()))?;

    let flow = Arc::new(Flow::default());
    let fetches = Arc::new(Fetches::default());
    let (outgoing, outgoing_rx) = mpsc::unbounded_channel();
    let reader = tauri::async_runtime::spawn(forward_output(
        recv,
        sink,
        Arc::clone(&flow),
        Arc::clone(&fetches),
    ));
    let writer = tauri::async_runtime::spawn(forward_input(outgoing_rx, send));
    Ok((welcome, Connection { outgoing, flow, fetches, reader, writer }))
}

async fn welcome(recv: &mut BufReader<LocalRecvHalf>) -> Result<Welcome, String> {
    loop {
        let body = read_frame(recv)
            .await
            .map_err(|e| format!("デーモンとの通信に失敗しました: {e}"))?
            .ok_or("デーモンが接続を閉じました")?;
        match decode::<ServerMessage>(&body) {
            Ok(ServerMessage::Welcome { daemon_version, pid, integrated, .. }) => {
                return Ok(Welcome { daemon_version, pid, integrated })
            }
            Ok(ServerMessage::Error { message }) => return Err(message),
            _ => {}
        }
    }
}

/// デーモンからの出力と知らせをフロントへ流す。届いた順のまま渡す。
async fn forward_output(
    mut recv: BufReader<LocalRecvHalf>,
    sink: impl EventSink,
    flow: Arc<Flow>,
    fetches: Arc<Fetches>,
) {
    let mut batch = Vec::new();
    let flush = |batch: &mut Vec<u8>| {
        if batch.is_empty() {
            return true;
        }
        flow.sent(batch.len());
        sink.deliver(InvokeResponseBody::Raw(std::mem::take(batch)))
    };

    let end = loop {
        flow.wait_for_room().await;
        let body = match read_frame(&mut recv).await {
            Ok(Some(body)) => body,
            Ok(None) => break Control::Disconnected { reason: None },
            Err(e) => break Control::Disconnected { reason: Some(e.to_string()) },
        };
        let notice = match decode::<ServerMessage>(&body) {
            Ok(ServerMessage::Output { data }) => {
                batch.extend_from_slice(&data);
                // 次のフレームがもう届いていれば、まとめてから渡す。
                if batch.len() < MAX_BATCH && has_complete_frame(&recv) {
                    continue;
                }
                if !flush(&mut batch) {
                    return;
                }
                continue;
            }
            // Replay は、空でも単独で渡す。フロントは、そこに含まれる問い合わせ
            // (カーソル位置や色など) には応答しない。
            Ok(ServerMessage::Replay { data }) => {
                if !flush(&mut batch) || !sink.deliver(control(&Control::Replay)) {
                    return;
                }
                flow.sent(data.len());
                if !sink.deliver(InvokeResponseBody::Raw(data)) {
                    return;
                }
                continue;
            }
            Ok(ServerMessage::BlockOutput { id, data, truncated }) => {
                fetches.resolve(id, data, truncated);
                continue;
            }
            Ok(ServerMessage::History { blocks, running, prompt, ready }) => Control::History {
                blocks: blocks.into_iter().map(Into::into).collect(),
                running: running.map(Into::into),
                prompt: prompt.into(),
                ready,
            },
            Ok(ServerMessage::Prompt { prompt }) => Control::Prompt { prompt: prompt.into() },
            Ok(ServerMessage::BlockStarted { block }) => {
                Control::BlockStarted { block: block.into() }
            }
            Ok(ServerMessage::BlockFinished { id, result }) => {
                Control::BlockFinished { id, result: result.into() }
            }
            Ok(ServerMessage::RunRejected { reason, text }) => Control::RunRejected {
                reason: match reason {
                    RunRejection::Incomplete => RejectionView::Incomplete,
                    RunRejection::Busy => RejectionView::Busy,
                },
                text,
            },
            Ok(ServerMessage::Exited { code }) => break Control::Exited { code },
            Ok(ServerMessage::Error { message }) => {
                break Control::Disconnected { reason: Some(message) }
            }
            // 未知のメッセージ (新しいデーモンのもの) は読み飛ばす。
            Ok(ServerMessage::Welcome { .. }) | Err(_) => continue,
        };
        // 知らせは、それより前の出力を渡してから渡す。
        if !flush(&mut batch) || !sink.deliver(control(&notice)) {
            return;
        }
    };
    fetches.clear();
    if flush(&mut batch) {
        sink.deliver(control(&end));
    }
}

/// 読み取り済みのバッファに、次のフレームが丸ごと入っているか。
fn has_complete_frame(recv: &BufReader<LocalRecvHalf>) -> bool {
    let buffered = recv.buffer();
    match buffered.first_chunk::<4>() {
        Some(header) => buffered.len() - 4 >= u32::from_be_bytes(*header) as usize,
        None => false,
    }
}

/// フロントからの入力やリサイズをデーモンへ送る。
async fn forward_input(
    mut outgoing: mpsc::UnboundedReceiver<ClientMessage>,
    mut send: BufWriter<LocalSendHalf>,
) {
    while let Some(message) = outgoing.recv().await {
        if write_frame(&mut send, &message).await.is_err() {
            return;
        }
        while let Ok(message) = outgoing.try_recv() {
            if write_frame(&mut send, &message).await.is_err() {
                return;
            }
        }
        if send.flush().await.is_err() {
            return;
        }
    }
    let _ = send.flush().await;
}

/// キー入力。本体はバイト列のまま受け取る。
#[tauri::command]
pub fn pty_input(request: Request<'_>, state: State<'_, PtyState>) -> Result<(), String> {
    let InvokeBody::Raw(data) = request.body() else {
        return Err("入力はバイト列で送ってください".into());
    };
    state.send(ClientMessage::Input { data: data.clone() })
}

#[tauri::command]
pub fn pty_resize(state: State<'_, PtyState>, cols: u16, rows: u16) -> Result<(), String> {
    state.send(ClientMessage::Resize { cols, rows })
}

/// 入力欄のテキストを実行する。実行されなかったときは、`runRejected` の知らせが届く。
#[tauri::command]
pub fn session_run(state: State<'_, PtyState>, text: String) -> Result<(), String> {
    state.send(ClientMessage::Run { text })
}

/// 終了したブロックの出力を取る。
///
/// 返すバイト列の先頭の1バイトは、出力の先頭を省いたかどうか (1 なら省いた)。残りが出力。
#[tauri::command]
pub async fn block_output(state: State<'_, PtyState>, id: u64) -> Result<Response, String> {
    let waiter = state.fetch(id)?;
    let output = tokio::time::timeout(FETCH_TIMEOUT, waiter)
        .await
        .map_err(|_| format!("ブロック #{id} の出力が返ってきません"))?
        .map_err(|_| "デーモンとの接続が切れました".to_string())?;
    let mut body = Vec::with_capacity(output.data.len() + 1);
    body.push(u8::from(output.truncated));
    body.extend_from_slice(&output.data);
    Ok(Response::new(body))
}

/// フロントが出力を描き終えたぶんの報告。
#[tauri::command]
pub fn pty_ack(state: State<'_, PtyState>, bytes: usize) {
    if let Some(connection) = lock(&state.connection).as_ref() {
        connection.flow.ack(bytes);
    }
}

#[cfg(test)]
mod tests {
    //! デーモンを実際に起動し、フロントに届く知らせと出力を確かめる。

    use hull_terminal_daemon::{run, Config, ShellSpec};
    use hull_terminal_protocol::transport;
    use serde_json::{json, Value};

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(60);

    struct TestSink(mpsc::UnboundedSender<InvokeResponseBody>);

    impl EventSink for TestSink {
        fn deliver(&self, body: InvokeResponseBody) -> bool {
            self.0.send(body).is_ok()
        }
    }

    /// フロントの代わりに、届いたものを順に受け取る。
    struct Front {
        connection: Connection,
        events: mpsc::UnboundedReceiver<InvokeResponseBody>,
        welcome: Welcome,
        _home: tempfile::TempDir,
    }

    /// 開発者の設定に左右されない bash でデーモンを起動し、つなぐ。
    async fn attach(raw: bool) -> Front {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".bashrc"), "PS1='PROMPT> '\n").unwrap();
        let mut shell = ShellSpec::detect().unwrap();
        shell.args = vec!["--noprofile".into(), "-i".into()];
        shell.env = vec![("HOME".into(), dir.path().to_string_lossy().into_owned())];
        let endpoint = dir.path().join("daemon.sock").to_string_lossy().into_owned();
        tokio::spawn(run(Config {
            endpoint: endpoint.clone(),
            shell: Some(shell),
            ..Config::default()
        }));

        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let (recv, send) = loop {
            match transport::connect(&endpoint).await {
                Ok(halves) => break halves,
                Err(e) if tokio::time::Instant::now() > deadline => panic!("接続できません: {e}"),
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        let (tx, events) = mpsc::unbounded_channel();
        let (welcome, connection) = connect(recv, send, 80, 24, raw, TestSink(tx)).await.unwrap();
        Front { connection, events, welcome, _home: dir }
    }

    enum Event {
        Notice(Value),
        Bytes(Vec<u8>),
    }

    impl Front {
        async fn next(&mut self) -> Event {
            let body = tokio::time::timeout(TIMEOUT, self.events.recv())
                .await
                .expect("時間内に届くこと")
                .expect("中継が続いていること");
            match body {
                InvokeResponseBody::Json(text) => {
                    Event::Notice(serde_json::from_str(&text).unwrap())
                }
                InvokeResponseBody::Raw(data) => {
                    self.connection.flow.ack(data.len());
                    Event::Bytes(data)
                }
            }
        }

        /// `kind` の知らせが届くまで読む。その間に届いた出力と、その知らせを返す。
        async fn until(&mut self, kind: &str) -> (String, Value) {
            let mut output = Vec::new();
            loop {
                match self.next().await {
                    Event::Bytes(data) => output.extend(data),
                    Event::Notice(notice) if notice["type"] == kind => {
                        return (String::from_utf8_lossy(&output).into_owned(), notice)
                    }
                    Event::Notice(_) => {}
                }
            }
        }

        fn run(&self, text: &str) {
            self.connection.send(ClientMessage::Run { text: text.into() }).unwrap();
        }

        /// ブロックの表示でつないだ直後に、最初のプロンプトまで読む。
        async fn ready(&mut self) {
            let (_, history) = self.until("history").await;
            if history["ready"] != true {
                self.until("prompt").await;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_arrives_as_a_block_with_its_output() {
        let mut front = attach(false).await;
        assert!(front.welcome.integrated);
        assert!(front.welcome.pid.is_some());
        front.ready().await;

        front.run("echo out-$((6*7))");
        let (before, started) = front.until("blockStarted").await;
        assert_eq!(before, "", "ブロックが始まる前に出力は届かない");
        let block = &started["block"];
        assert_eq!(block["command"], "echo out-$((6*7))");
        assert_eq!((&block["cols"], &block["rows"]), (&json!(80), &json!(24)));
        assert!(block["startedAt"].as_u64().unwrap() > 0);

        let (output, finished) = front.until("blockFinished").await;
        assert_eq!(output, "out-42\r\n");
        assert_eq!(finished["id"], block["id"]);
        assert_eq!(finished["result"]["exitCode"], 0);
        assert_eq!(finished["result"]["fullscreen"], false);
        assert!(finished["result"]["finishedAt"].as_u64() >= block["startedAt"].as_u64());

        let (_, prompt) = front.until("prompt").await;
        assert!(prompt["prompt"]["cwd"].is_string());
        assert_eq!(prompt["prompt"]["branch"], Value::Null);

        // 終了したブロックの出力を取れる。
        let id = block["id"].as_u64().unwrap();
        let fetched = front.connection.request_output(id).unwrap().await.unwrap();
        assert_eq!((fetched.data.as_slice(), fetched.truncated), (b"out-42\r\n".as_slice(), false));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unfinished_text_is_rejected() {
        let mut front = attach(false).await;
        front.ready().await;

        front.run("echo \"abc");
        let (_, rejected) = front.until("runRejected").await;
        assert_eq!(
            rejected,
            json!({"type": "runRejected", "reason": "incomplete", "text": "echo \"abc"})
        );
        front.until("prompt").await;

        // 実行中は受け付けない。
        front.run("read line");
        front.until("blockStarted").await;
        front.run("echo no");
        let (_, rejected) = front.until("runRejected").await;
        assert_eq!(rejected["reason"], "busy");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reattaching_restores_the_history_and_the_running_block() {
        let mut front = attach(false).await;
        front.ready().await;
        front.run("echo done-$((1+1))");
        front.until("prompt").await;
        front.run("echo waiting-$((2+2)); read line");
        front.until("blockStarted").await;
        let mut seen = String::new();
        // 改行は別のかたまりで届くことがあるので、行の終わりまで待つ。
        while !seen.contains("waiting-4\r\n") {
            if let Event::Bytes(data) = front.next().await {
                seen.push_str(&String::from_utf8_lossy(&data));
            }
        }

        // 同じデーモンに、もう1つつなぐ。
        let endpoint = front._home.path().join("daemon.sock").to_string_lossy().into_owned();
        let (recv, send) = transport::connect(&endpoint).await.unwrap();
        let (tx, events) = mpsc::unbounded_channel();
        let (welcome, connection) = connect(recv, send, 80, 24, false, TestSink(tx)).await.unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut second = Front { connection, events, welcome, _home: home };

        let (_, history) = second.until("history").await;
        assert_eq!(history["ready"], false);
        assert_eq!(history["running"]["command"], "echo waiting-$((2+2)); read line");
        let commands: Vec<&Value> = history["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| !block["info"]["command"].is_null())
            .collect();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["info"]["command"], "echo done-$((1+1))");
        assert_eq!(commands[0]["result"]["exitCode"], 0);

        // 実行中のブロックのここまでの出力は、replay の知らせの次に届く。
        let (before, _) = second.until("replay").await;
        assert_eq!(before, "");
        match second.next().await {
            Event::Bytes(data) => assert_eq!(data, b"waiting-4\r\n"),
            Event::Notice(notice) => panic!("出力が来るはずが {notice}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_raw_attach_gets_the_replay_and_then_everything() {
        let mut front = attach(true).await;
        let (before, _) = front.until("replay").await;
        assert_eq!(before, "");

        front
            .connection
            .send(ClientMessage::Input { data: b"echo raw-$((1+1))\r".to_vec() })
            .unwrap();
        let mut seen = String::new();
        while !seen.contains("raw-2\r\n") {
            match front.next().await {
                Event::Bytes(data) => seen.push_str(&String::from_utf8_lossy(&data)),
                Event::Notice(notice) => panic!("生の表示に知らせが届きました: {notice}"),
            }
        }

        // 知らない番号にはデーモンが答えない。bash が終了したら、待っている要求は失敗する。
        let waiter = front.connection.request_output(9999).unwrap();
        front.connection.send(ClientMessage::Input { data: b"exit 5\r".to_vec() }).unwrap();
        let (_, exited) = front.until("exited").await;
        assert_eq!(exited, json!({"type": "exited", "code": 5}));
        assert!(waiter.await.is_err());
    }
}

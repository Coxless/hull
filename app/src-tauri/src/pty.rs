//! フロントとデーモンの間の中継。
//!
//! 出力は Channel でバイト列のままフロントへ渡し、入力とリサイズはコマンドで受けてデーモンへ送る。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hull_terminal_protocol::{
    decode, default_endpoint, read_frame, transport::LocalRecvHalf, transport::LocalSendHalf,
    write_frame, ClientMessage, ServerMessage, PROTOCOL_VERSION,
};
use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request};
use tauri::State;
use tokio::io::{AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::{mpsc, Notify};

use crate::daemon;

/// フロントがまだ描いていない出力がこれを超えたら、デーモンからの読み取りを止める。
/// 止めている間は PTY が詰まり、出力しているコマンドが待たされる (`yes` などで UI を固めないため)。
const HIGH_WATER: usize = 2 * 1024 * 1024;
/// 続けて届いた出力は、この大きさまで1回にまとめてフロントへ渡す。
const MAX_BATCH: usize = 256 * 1024;
/// デーモンの応答 (Welcome) を待つ時間。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct PtyState {
    connection: Mutex<Option<Connection>>,
}

struct Connection {
    outgoing: mpsc::UnboundedSender<ClientMessage>,
    flow: Arc<Flow>,
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl PtyState {
    fn send(&self, message: ClientMessage) -> Result<(), String> {
        let guard = lock(&self.connection);
        let connection = guard.as_ref().ok_or("デーモンにつながっていません")?;
        connection.outgoing.send(message).map_err(|_| "デーモンとの接続が切れました".to_string())
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
#[serde(tag = "type", rename_all = "camelCase")]
enum Control {
    /// bash が終了した。
    Exited { code: u32 },
    /// デーモンとの接続が切れた。
    Disconnected { reason: Option<String> },
}

fn control(message: &Control) -> InvokeResponseBody {
    InvokeResponseBody::Json(serde_json::to_string(message).unwrap_or_else(|_| "null".into()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachInfo {
    daemon_version: String,
    pid: Option<u32>,
    /// この接続のためにデーモンを起動したか。false なら、動いていた bash に再接続した。
    started_daemon: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppOptions {
    /// 描画方法の指定 (`webgl` / `dom`)。未指定なら WebGL を試し、使えなければ DOM にする。
    renderer: Option<String>,
    /// この OS では動かないなどの警告。
    warning: Option<String>,
}

#[tauri::command]
pub fn app_options() -> AppOptions {
    AppOptions {
        renderer: std::env::var("HULL_TERMINAL_RENDERER").ok().filter(|v| !v.is_empty()),
        warning: daemon::platform_warning(),
    }
}

/// デーモンに接続し (いなければ起動し)、画面をつなぐ。
///
/// 出力は `on_event` にバイト列のまま流れる。最初の1通は、直前の画面を戻すための Replay
/// (新しい bash なら空)。
#[tauri::command]
pub async fn pty_attach(
    state: State<'_, PtyState>,
    cols: u16,
    rows: u16,
    on_event: Channel<InvokeResponseBody>,
) -> Result<AttachInfo, String> {
    // 前の接続 (画面の再読み込み前のもの) が残っていれば切る。
    state.detach().await;

    let endpoint = default_endpoint();
    let daemon::Connected { recv, send, started_daemon } =
        daemon::connect_or_spawn(&endpoint).await?;
    let mut recv = BufReader::with_capacity(MAX_BATCH, recv);
    let mut send = BufWriter::new(send);

    let io = |e| format!("デーモンとの通信に失敗しました: {e}");
    write_frame(&mut send, &ClientMessage::Hello { version: PROTOCOL_VERSION })
        .await
        .map_err(io)?;
    send.flush().await.map_err(|e| io(e.into()))?;

    let (daemon_version, pid) = tokio::time::timeout(HANDSHAKE_TIMEOUT, welcome(&mut recv))
        .await
        .map_err(|_| "デーモンが応答しません".to_string())??;

    write_frame(&mut send, &ClientMessage::Attach { cols, rows }).await.map_err(io)?;
    send.flush().await.map_err(|e| io(e.into()))?;

    let flow = Arc::new(Flow::default());
    let (outgoing, outgoing_rx) = mpsc::unbounded_channel();
    let reader = tauri::async_runtime::spawn(forward_output(recv, on_event, Arc::clone(&flow)));
    let writer = tauri::async_runtime::spawn(forward_input(outgoing_rx, send));
    state.replace(Some(Connection { outgoing, flow, reader, writer }));

    Ok(AttachInfo { daemon_version, pid, started_daemon })
}

async fn welcome(recv: &mut BufReader<LocalRecvHalf>) -> Result<(String, Option<u32>), String> {
    loop {
        let body = read_frame(recv)
            .await
            .map_err(|e| format!("デーモンとの通信に失敗しました: {e}"))?
            .ok_or("デーモンが接続を閉じました")?;
        match decode::<ServerMessage>(&body) {
            Ok(ServerMessage::Welcome { daemon_version, pid, .. }) => {
                return Ok((daemon_version, pid))
            }
            Ok(ServerMessage::Error { message }) => return Err(message),
            _ => {}
        }
    }
}

/// デーモンからの出力をフロントへ流す。
async fn forward_output(
    mut recv: BufReader<LocalRecvHalf>,
    on_event: Channel<InvokeResponseBody>,
    flow: Arc<Flow>,
) {
    let mut batch = Vec::new();
    let flush = |batch: &mut Vec<u8>| {
        if batch.is_empty() {
            return true;
        }
        flow.sent(batch.len());
        on_event.send(InvokeResponseBody::Raw(std::mem::take(batch))).is_ok()
    };

    let end = loop {
        flow.wait_for_room().await;
        let body = match read_frame(&mut recv).await {
            Ok(Some(body)) => body,
            Ok(None) => break Control::Disconnected { reason: None },
            Err(e) => break Control::Disconnected { reason: Some(e.to_string()) },
        };
        match decode::<ServerMessage>(&body) {
            // Replay は、空でも単独で渡す。フロントは最初の1通を「画面の再現」として扱い、
            // そこに含まれる問い合わせ (カーソル位置や色など) には応答しない。
            Ok(ServerMessage::Replay { data }) => {
                flow.sent(data.len());
                if on_event.send(InvokeResponseBody::Raw(data)).is_err() {
                    return;
                }
            }
            Ok(ServerMessage::Output { data }) => {
                batch.extend_from_slice(&data);
                // 次のフレームがもう届いていれば、まとめてから渡す。
                if batch.len() < MAX_BATCH && has_complete_frame(&recv) {
                    continue;
                }
                if !flush(&mut batch) {
                    return;
                }
            }
            Ok(ServerMessage::Exited { code }) => break Control::Exited { code },
            Ok(ServerMessage::Error { message }) => {
                break Control::Disconnected { reason: Some(message) }
            }
            // 未知のメッセージ (新しいデーモンのもの) は読み飛ばす。
            Ok(ServerMessage::Welcome { .. }) | Err(_) => {}
        }
    };
    if flush(&mut batch) {
        let _ = on_event.send(control(&end));
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

/// フロントが出力を描き終えたぶんの報告。
#[tauri::command]
pub fn pty_ack(state: State<'_, PtyState>, bytes: usize) {
    if let Some(connection) = lock(&state.connection).as_ref() {
        connection.flow.ack(bytes);
    }
}

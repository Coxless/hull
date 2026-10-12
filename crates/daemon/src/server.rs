use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use hull_terminal_protocol::{
    decode, default_endpoint, read_frame,
    transport::{self, LocalRecvHalf, LocalSendHalf},
    write_frame, ClientMessage, ServerMessage, PROTOCOL_VERSION,
};
use tokio::io::{AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::block::StoredOutput;
use crate::integration::Integration;
use crate::session::{ClientEvent, Session};
use crate::{ShellSpec, DEFAULT_OUTPUT_BUDGET, DEFAULT_RING_CAPACITY};

/// 接続してから `Hello` が届くまで待つ時間。
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// bash が終わってから、残りの出力を読み終えるまで待つ時間。
/// bash が残したバックグラウンドのプロセスが PTY を持ち続けると、読み取りは終わらない。
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
/// 終了をクライアントに届けきるまで待つ時間。
const GOODBYE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct Config {
    /// 待ち受ける Unix ソケットのパス。
    pub endpoint: String,
    /// 起動するシェル。`None` なら [`ShellSpec::detect`] で探す。
    pub shell: Option<ShellSpec>,
    /// 再接続のときに流し直す出力の量 (バイト)。ブロック1つの出力の上限でもある。
    pub ring_capacity: usize,
    /// 終了したブロックの出力を、合わせてどこまで持つか (バイト)。
    pub output_budget: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            shell: None,
            ring_capacity: DEFAULT_RING_CAPACITY,
            output_budget: DEFAULT_OUTPUT_BUDGET,
        }
    }
}

/// デーモンを動かす。bash が終わったら、その終了コードを返す。
pub async fn run(config: Config) -> anyhow::Result<u32> {
    // 先に待ち受けを確保する。別のデーモンが生きていれば、bash を起動する前にここで失敗する。
    let listener = transport::listen(&config.endpoint)
        .await
        .with_context(|| format!("{} で待ち受けられません", config.endpoint))?;

    let shell = match config.shell {
        Some(shell) => shell,
        None => ShellSpec::detect()?,
    };
    let integration = if shell.integration { install_integration(&config.endpoint) } else { None };
    let (session, mut end) =
        Session::spawn(&shell, config.ring_capacity, config.output_budget, integration.as_ref())?;
    let session = Arc::new(session);

    let mut clients = JoinSet::new();
    let code = loop {
        tokio::select! {
            accepted = transport::accept(&listener) => match accepted {
                Ok((recv, send)) => {
                    clients.spawn(serve_client(recv, send, Arc::clone(&session)));
                }
                Err(e) => {
                    eprintln!("接続を受け付けられません: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            code = &mut end.exited => break code.unwrap_or(1),
            Some(_) = clients.join_next(), if !clients.is_empty() => {}
        }
    };

    // これ以上は受け付けない。Unix ではソケットファイルもここで消える。
    drop(listener);
    session.close_pty();
    let _ = timeout(DRAIN_TIMEOUT, end.drained).await;
    session.announce_exit(code).await;
    let _ = timeout(GOODBYE_TIMEOUT, async { while clients.join_next().await.is_some() {} }).await;
    Ok(code)
}

/// 統合スクリプトを、ソケットと同じディレクトリに書き出す。
///
/// 書けなくても bash は起動する (合図が出ないので、コマンドの区切りはわからない)。
fn install_integration(endpoint: &str) -> Option<Integration> {
    let dir = Path::new(endpoint).parent()?;
    match Integration::install(dir) {
        Ok(integration) => Some(integration),
        Err(e) => {
            eprintln!("統合スクリプトを用意できません: {e:#}");
            None
        }
    }
}

async fn serve_client(recv: LocalRecvHalf, send: LocalSendHalf, session: Arc<Session>) {
    if let Err(e) = client_loop(recv, send, &session).await {
        eprintln!("クライアントとの通信を終了しました: {e:#}");
    }
}

async fn client_loop(
    recv: LocalRecvHalf,
    send: LocalSendHalf,
    session: &Session,
) -> anyhow::Result<()> {
    let mut writer = BufWriter::new(send);

    // フレームの読み取りはキャンセル安全ではないので、専用のタスクで読んで渡す。
    let (message_tx, mut messages) = mpsc::channel::<ClientMessage>(64);
    let reader = tokio::spawn(async move {
        let mut recv = BufReader::new(recv);
        while let Ok(Some(body)) = read_frame(&mut recv).await {
            // 解釈できないフレームは、新しい UI が送った未知のメッセージとして読み飛ばす。
            let Ok(message) = decode::<ClientMessage>(&body) else { continue };
            if message_tx.send(message).await.is_err() {
                break;
            }
        }
    });
    let _abort_reader = AbortOnDrop(reader);

    match timeout(HELLO_TIMEOUT, messages.recv()).await {
        Ok(Some(ClientMessage::Hello { version })) if version == PROTOCOL_VERSION => {
            let welcome = ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                pid: session.pid(),
                integrated: session.integrated(),
            };
            write_frame(&mut writer, &welcome).await?;
            writer.flush().await?;
        }
        Ok(Some(ClientMessage::Hello { version })) => {
            let message = format!(
                "プロトコルのバージョンが合いません (UI: {version}, デーモン: {PROTOCOL_VERSION})。hull-terminal を入れ直すか、動いているセッションを終了してください"
            );
            write_frame(&mut writer, &ServerMessage::Error { message }).await?;
            writer.flush().await?;
            return Ok(());
        }
        _ => return Ok(()),
    }

    let mut events: Option<mpsc::Receiver<ClientEvent>> = None;
    loop {
        tokio::select! {
            message = messages.recv() => match message {
                None | Some(ClientMessage::Detach) => break,
                Some(ClientMessage::Attach { cols, rows, raw }) => {
                    session.resize(cols, rows);
                    // 生の表示かどうかは、最初の Attach で決まる。
                    if events.is_none() {
                        let attachment = session.attach(raw);
                        for message in &attachment.greeting {
                            write_frame(&mut writer, message).await?;
                        }
                        writer.flush().await?;
                        events = Some(attachment.events);
                    }
                }
                Some(ClientMessage::Run { text }) => {
                    if let Err(reason) = session.run(&text) {
                        write_frame(&mut writer, &ServerMessage::RunRejected { reason, text }).await?;
                        writer.flush().await?;
                    }
                }
                Some(ClientMessage::FetchOutput { id }) => {
                    // 知らない番号には答えない (実行中のブロックの出力は Replay と Output で届く)。
                    if let Some(output) = session.block_output(id) {
                        let StoredOutput { data, truncated } = output;
                        let message = ServerMessage::BlockOutput { id, data, truncated };
                        write_frame(&mut writer, &message).await?;
                        writer.flush().await?;
                    }
                }
                Some(ClientMessage::Input { data }) => session.write_input(data),
                Some(ClientMessage::Resize { cols, rows }) => session.resize(cols, rows),
                Some(ClientMessage::Hello { .. }) => {}
            },
            event = next_event(&mut events) => {
                match write_events(&mut writer, event, &mut events).await {
                    Ok(Flow::Continue) => {}
                    Ok(Flow::Finished) => break,
                    // 相手が急にいなくなると、出力は書けなくなる。それでも、切れる直前に
                    // 送られてきた入力は届いているので、読み終えるまで続けて bash に渡す。
                    Err(_) => events = None,
                }
            }
        }
    }
    Ok(())
}

enum Flow {
    Continue,
    /// bash の終了を伝えた。
    Finished,
}

/// `first` と、たまっているぶんのイベントをまとめて書いてから flush する。
async fn write_events(
    writer: &mut BufWriter<LocalSendHalf>,
    first: Option<ClientEvent>,
    events: &mut Option<mpsc::Receiver<ClientEvent>>,
) -> anyhow::Result<Flow> {
    let mut event = first;
    let flow = loop {
        match event {
            Some(ClientEvent::Output(data)) => {
                write_frame(writer, &ServerMessage::Output { data: data.to_vec() }).await?;
            }
            Some(ClientEvent::Message(message)) => write_frame(writer, &message).await?,
            Some(ClientEvent::Exited(code)) => {
                write_frame(writer, &ServerMessage::Exited { code }).await?;
                break Flow::Finished;
            }
            None => break Flow::Finished,
        }
        match events.as_mut().map(|rx| rx.try_recv()) {
            Some(Ok(next)) => event = Some(next),
            _ => break Flow::Continue,
        }
    };
    writer.flush().await?;
    Ok(flow)
}

async fn next_event(events: &mut Option<mpsc::Receiver<ClientEvent>>) -> Option<ClientEvent> {
    match events {
        Some(events) => events.recv().await,
        None => std::future::pending().await,
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

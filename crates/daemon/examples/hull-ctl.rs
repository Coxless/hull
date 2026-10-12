//! デーモンを手で確かめるための小さなクライアント。
//!
//! ```text
//! cargo run -p hull-terminal-daemon &
//! cargo run -p hull-terminal-daemon --example hull-ctl -- 'echo hello'
//! cargo run -p hull-terminal-daemon --example hull-ctl            # これまでのブロックの一覧
//! cargo run -p hull-terminal-daemon --example hull-ctl -- --output 1   # 終了したブロックの出力
//! cargo run -p hull-terminal-daemon --example hull-ctl -- --raw 'echo hello'
//! cargo run -p hull-terminal-daemon --example hull-ctl -- exit
//! ```
//!
//! 引数を1つのコマンドとして実行し、できたブロックを順に表示する。見出しと結果は標準エラーに、
//! 出力は標準出力に書く。次のプロンプトが出るか、bash が終了したら終わる。
//!
//! `--raw` を付けると、区切りを通さない生の表示でつなぐ。引数を1行のキー入力として送り、
//! 返ってきた出力をそのまま書く。出力が 500 ミリ秒とぎれたら終わる。

use std::io::Write;
use std::time::Duration;

use hull_terminal_protocol::{
    decode, default_endpoint, read_frame,
    transport::{self, LocalRecvHalf, LocalSendHalf},
    write_frame, BlockInfo, BlockResult, ClientMessage, FinishedBlock, RunRejection, ServerMessage,
    PROTOCOL_VERSION,
};
use tokio::io::AsyncWriteExt;

const IDLE: Duration = Duration::from_millis(500);

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut endpoint = default_endpoint();
    let mut raw = false;
    let mut output: Option<u64> = None;
    let mut words = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--endpoint" if words.is_empty() => {
                endpoint = args.next().ok_or_else(|| anyhow::anyhow!("接続先がありません"))?;
            }
            "--raw" if words.is_empty() => raw = true,
            "--output" if words.is_empty() => {
                let id = args.next().and_then(|id| id.parse().ok());
                output = Some(id.ok_or_else(|| anyhow::anyhow!("ブロックの番号がありません"))?);
            }
            _ => words.push(arg),
        }
    }
    let line = words.join(" ");

    let (mut recv, mut send) = transport::connect(&endpoint).await?;
    write_frame(&mut send, &ClientMessage::Hello { version: PROTOCOL_VERSION }).await?;
    write_frame(&mut send, &ClientMessage::Attach { cols: 80, rows: 24, raw }).await?;
    send.flush().await?;

    if let Some(id) = output {
        show_output(&mut recv, &mut send, id).await?;
    } else if raw {
        show_raw(&mut recv, &mut send, &line).await?;
    } else {
        show_blocks(&mut recv, &mut send, &line).await?;
    }
    // bash が終了していれば、デーモンはもういない。
    if write_frame(&mut send, &ClientMessage::Detach).await.is_ok() {
        let _ = send.flush().await;
    }
    Ok(())
}

async fn next_message(recv: &mut LocalRecvHalf) -> anyhow::Result<Option<ServerMessage>> {
    loop {
        let Some(body) = read_frame(recv).await? else { return Ok(None) };
        // 解釈できないフレームは、新しいデーモンが送った未知のメッセージとして読み飛ばす。
        if let Ok(message) = decode(&body) {
            return Ok(Some(message));
        }
    }
}

async fn show_blocks(
    recv: &mut LocalRecvHalf,
    send: &mut LocalSendHalf,
    line: &str,
) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout();
    // 送ったコマンドの結果が出そろうまで読む。送る前は、プロンプトを待つ。
    let mut sent = false;
    let mut running: Option<BlockInfo> = None;
    while let Some(message) = next_message(recv).await? {
        let ready = match message {
            ServerMessage::Welcome { daemon_version, pid, integrated, .. } => {
                eprintln!("[デーモン {daemon_version} / bash の PID {pid:?}]");
                if !integrated {
                    anyhow::bail!(
                        "bash が統合スクリプトを読み込んでいません。--raw を付けてください"
                    );
                }
                false
            }
            ServerMessage::History { blocks, running: now_running, ready, .. } => {
                if line.is_empty() {
                    for FinishedBlock { info, result } in &blocks {
                        eprintln!("{}  {}", header(info), footer(info, result));
                    }
                }
                if let Some(block) = &now_running {
                    eprintln!("{}  [実行中]", header(block));
                }
                running = now_running;
                ready
            }
            ServerMessage::BlockStarted { block } => {
                eprintln!("{}", header(&block));
                running = Some(block);
                false
            }
            ServerMessage::Output { data } | ServerMessage::Replay { data } => {
                stdout.write_all(&data)?;
                stdout.flush()?;
                false
            }
            ServerMessage::BlockFinished { result, .. } => {
                if let Some(block) = running.take() {
                    eprintln!("{}", footer(&block, &result));
                }
                false
            }
            ServerMessage::Prompt { .. } => true,
            ServerMessage::RunRejected { reason, .. } => {
                anyhow::bail!(match reason {
                    RunRejection::Incomplete => "入力が途中なので、実行しませんでした",
                    RunRejection::Busy => "コマンドの実行中なので、受け付けられませんでした",
                });
            }
            ServerMessage::Exited { code } => {
                eprintln!("[bash が終了しました (終了コード {code})]");
                return Ok(());
            }
            ServerMessage::Error { message } => anyhow::bail!(message),
            ServerMessage::BlockOutput { .. } => false,
        };
        if !ready {
            continue;
        }
        if sent || line.is_empty() {
            break;
        }
        write_frame(send, &ClientMessage::Run { text: line.into() }).await?;
        send.flush().await?;
        sent = true;
    }
    Ok(())
}

/// 終了したブロックの出力を取って、標準出力に書く。
async fn show_output(
    recv: &mut LocalRecvHalf,
    send: &mut LocalSendHalf,
    id: u64,
) -> anyhow::Result<()> {
    write_frame(send, &ClientMessage::FetchOutput { id }).await?;
    send.flush().await?;
    loop {
        match tokio::time::timeout(IDLE, next_message(recv)).await {
            Ok(Ok(Some(ServerMessage::BlockOutput { data, truncated, .. }))) => {
                if truncated {
                    eprintln!("[先頭は省かれています]");
                }
                std::io::stdout().write_all(&data)?;
                return Ok(());
            }
            Ok(Ok(Some(ServerMessage::Error { message }))) => anyhow::bail!(message),
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) | Err(_) => anyhow::bail!("終了したブロック #{id} はありません"),
            Ok(Err(e)) => return Err(e),
        }
    }
}

fn header(block: &BlockInfo) -> String {
    match block.command.as_deref() {
        None => format!("#{} (コマンドの外の出力)", block.id),
        Some("") => format!("#{} (入力欄を通さずに実行)", block.id),
        Some(command) => format!("#{} $ {}", block.id, command.replace('\n', "\n     ")),
    }
}

fn footer(block: &BlockInfo, result: &BlockResult) -> String {
    let elapsed = result.finished_at.saturating_sub(block.started_at);
    let fullscreen = if result.fullscreen { " / 全画面" } else { "" };
    match result.exit_code {
        Some(code) => format!("[終了コード {code} / {elapsed} ミリ秒{fullscreen}]"),
        None => format!("[{elapsed} ミリ秒{fullscreen}]"),
    }
}

async fn show_raw(
    recv: &mut LocalRecvHalf,
    send: &mut LocalSendHalf,
    line: &str,
) -> anyhow::Result<()> {
    if !line.is_empty() {
        let data = format!("{line}\r").into_bytes();
        write_frame(send, &ClientMessage::Input { data }).await?;
        send.flush().await?;
    }

    let mut stdout = std::io::stdout();
    loop {
        let message = match tokio::time::timeout(IDLE, next_message(recv)).await {
            Ok(message) => message?,
            Err(_) => break,
        };
        match message {
            Some(ServerMessage::Welcome { daemon_version, pid, .. }) => {
                eprintln!("[デーモン {daemon_version} / bash の PID {pid:?}]");
            }
            // 画面の再現ではなく送ったコマンドの結果を見たいので、Replay は表示しない。
            Some(ServerMessage::Output { data }) => {
                stdout.write_all(&data)?;
                stdout.flush()?;
            }
            Some(ServerMessage::Exited { code }) => {
                eprintln!("\n[bash が終了しました (終了コード {code})]");
                break;
            }
            Some(ServerMessage::Error { message }) => anyhow::bail!(message),
            Some(_) => {}
            None => break,
        }
    }
    Ok(())
}

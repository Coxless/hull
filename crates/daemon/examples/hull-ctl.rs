//! デーモンを手で確かめるための小さなクライアント。
//!
//! ```text
//! cargo run -p hull-terminal-daemon &
//! cargo run -p hull-terminal-daemon --example hull-ctl -- 'echo hello'
//! cargo run -p hull-terminal-daemon --example hull-ctl -- exit
//! ```
//!
//! 引数を1行として bash に送り、返ってきた出力をそのまま標準出力に書く。
//! 出力が 500 ミリ秒とぎれるか、bash が終了したら終わる。

use std::io::Write;
use std::time::Duration;

use hull_terminal_protocol::{
    decode, default_endpoint, read_frame, transport, write_frame, ClientMessage, ServerMessage,
    PROTOCOL_VERSION,
};
use tokio::io::AsyncWriteExt;

const IDLE: Duration = Duration::from_millis(500);

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let endpoint = match args.first().map(String::as_str) {
        Some("--endpoint") if args.len() >= 2 => {
            let endpoint = args.remove(1);
            args.remove(0);
            endpoint
        }
        _ => default_endpoint(),
    };
    let line = args.join(" ");

    let (mut recv, mut send) = transport::connect(&endpoint).await?;
    write_frame(&mut send, &ClientMessage::Hello { version: PROTOCOL_VERSION }).await?;
    write_frame(&mut send, &ClientMessage::Attach { cols: 80, rows: 24 }).await?;
    if !line.is_empty() {
        let data = format!("{line}\r").into_bytes();
        write_frame(&mut send, &ClientMessage::Input { data }).await?;
    }
    send.flush().await?;

    let mut stdout = std::io::stdout();
    loop {
        let body = match tokio::time::timeout(IDLE, read_frame(&mut recv)).await {
            Ok(body) => body?,
            Err(_) => break,
        };
        let Some(body) = body else { break };
        match decode::<ServerMessage>(&body) {
            Ok(ServerMessage::Welcome { daemon_version, pid, .. }) => {
                eprintln!("[デーモン {daemon_version} / bash の PID {pid:?}]");
            }
            // 画面の再現ではなく送ったコマンドの結果を見たいので、Replay は表示しない。
            Ok(ServerMessage::Replay { .. }) => {}
            Ok(ServerMessage::Output { data }) => {
                stdout.write_all(&data)?;
                stdout.flush()?;
            }
            Ok(ServerMessage::Exited { code }) => {
                eprintln!("\n[bash が終了しました (終了コード {code})]");
                return Ok(());
            }
            Ok(ServerMessage::Error { message }) => anyhow::bail!(message),
            Err(_) => {}
        }
    }
    write_frame(&mut send, &ClientMessage::Detach).await?;
    send.flush().await?;
    Ok(())
}

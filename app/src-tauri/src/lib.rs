//! hull-terminal の UI 側の Rust コア。
//!
//! デーモンに接続し (いなければ起動し)、PTY の出力とブロックの知らせをフロントへ流し、
//! フロントからの入力・リサイズ・実行するテキストをデーモンへ送る。

mod daemon;
mod pty;

use tauri::{Manager, RunEvent};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(pty::PtyState::default())
        .invoke_handler(tauri::generate_handler![
            pty::app_options,
            pty::pty_attach,
            pty::pty_input,
            pty::pty_resize,
            pty::pty_ack,
            pty::session_run,
            pty::block_output,
            pty::complete,
        ])
        .build(tauri::generate_context!())
        .expect("hull-terminal を起動できません")
        .run(|app, event| {
            // ウィンドウを閉じるときは Detach を送って切断する。bash はデーモンの下に残る。
            if let RunEvent::Exit = event {
                tauri::async_runtime::block_on(app.state::<pty::PtyState>().detach());
            }
        });
}

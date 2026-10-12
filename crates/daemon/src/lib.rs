//! hull-terminal-daemon: bash を PTY の下で1つ起動し、UI との間で入出力を中継する。
//!
//! UI とは別プロセスなので、ウィンドウを閉じても bash は生き続ける。
//! bash が終わったらデーモンも終わる。

mod integration;
// セッションにつなぐのは、ブロックの管理を入れるとき (Phase 2 の作業3)。
#[allow(dead_code)]
mod marker;
mod ring;
mod server;
mod session;
mod shell;

pub use ring::Ring;
pub use server::{run, Config};
pub use shell::ShellSpec;

/// 再接続のときに流し直す出力の量の既定値。
pub const DEFAULT_RING_CAPACITY: usize = 1024 * 1024;

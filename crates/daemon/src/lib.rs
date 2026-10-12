//! hull-terminal-daemon: bash を PTY の下で1つ起動し、UI との間で入出力を中継する。
//!
//! bash の出力は、統合スクリプトが出す合図の位置で区切り、コマンドごとのブロックとして記録する。
//! UI とは別プロセスなので、ウィンドウを閉じても bash は生き続ける。
//! bash が終わったらデーモンも終わる。

mod block;
mod complete;
mod integration;
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

/// 終了したブロックの出力を、合わせてどこまで持つかの既定値。
pub const DEFAULT_OUTPUT_BUDGET: usize = 32 * 1024 * 1024;

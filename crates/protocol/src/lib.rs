//! hull-terminal の UI とデーモンが共有するプロトコル。
//!
//! フレームは「4バイトの長さ (ビッグエンディアン) + MessagePack 本体」で、
//! 出力と入力はバイト列のまま運ぶ。
//!
//! # 互換性の方針
//!
//! - 接続の最初に [`ClientMessage::Hello`] と [`ServerMessage::Welcome`] で
//!   [`PROTOCOL_VERSION`] を交換する。違っていればデーモンは
//!   [`ServerMessage::Error`] を返して切断する。
//! - メッセージやフィールドを足すだけの変更では `PROTOCOL_VERSION` を上げない。
//!   受け取った側は、解釈できないフレームを読み飛ばす ([`decode`] が失敗したら無視する)。
//!   足したフィールドには `#[serde(default)]` を付ける。
//! - 既存のメッセージの意味を変えるときだけ `PROTOCOL_VERSION` を上げる。

mod endpoint;
mod frame;
mod message;
pub mod transport;

pub use endpoint::{default_endpoint, state_dir, ENDPOINT_ENV};
pub use frame::{decode, encode, read_frame, write_frame, FrameDecoder, FrameError, MAX_FRAME_LEN};
pub use message::{ClientMessage, ServerMessage, PROTOCOL_VERSION};

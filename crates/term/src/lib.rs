//! hull の端末の中核。VT の処理、画面とスクロールバック、キーとマウスの符号化、選択範囲。
//!
//! 窓、GPU、PTY には触れないので、単体でテストできる。
//! 窓ごとの状態はすべて `Terminal` に入れ、グローバルな状態は持たない。

pub mod cell;
pub mod grid;
pub mod input;
pub mod selection;
pub mod term;

pub use cell::{Cell, Color, Row, flags};
pub use grid::Grid;
pub use selection::{Point, Selection, SelectionKind};
pub use term::{CursorShape, Event, Modes, MouseMode, Palette, PromptMark, Rgb, Terminal};

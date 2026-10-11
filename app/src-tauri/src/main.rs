// Windows のリリースビルドでコンソール画面を出さない。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    hull_terminal_lib::run()
}

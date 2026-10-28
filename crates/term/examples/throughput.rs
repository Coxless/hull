//! VT の処理だけの速さを測る (描画や PTY を含まない)。
//!
//!     cargo run --release -p hull-term --example throughput -- ファイル [回数] [スクロールバックの行数]

use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("ファイルを指定してください");
    let times: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let history: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(10_000);
    let data = std::fs::read(&path).expect("ファイルを読めません");
    let mut term = hull_term::Terminal::new(100, 30, history);
    let start = Instant::now();
    for _ in 0..times {
        for chunk in data.chunks(65536) {
            term.advance(chunk);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let mb = (data.len() * times) as f64 / 1e6;
    println!("{mb:.1} MB を {secs:.3} 秒 ({:.0} MB/s)", mb / secs);
}

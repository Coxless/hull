//! スクロールだけの速さを測る (グリッドを直接と、改行を流したときの比較)。

use std::time::Instant;

fn main() {
    let n = 5_000_000;
    let mut grid = hull_term::Grid::new(100, 30, 10_000);
    let t = Instant::now();
    for _ in 0..n {
        grid.scroll_up_full(1, hull_term::Cell::BLANK);
    }
    println!("Grid::scroll_up_full: {:.1} ns/行", t.elapsed().as_nanos() as f64 / n as f64);

    let mut term = hull_term::Terminal::new(100, 30, 10_000);
    term.advance(b"\x1b[30;1H");
    let data = vec![b'\n'; n];
    let t = Instant::now();
    term.advance(&data);
    println!("改行: {:.1} ns/行", t.elapsed().as_nanos() as f64 / n as f64);
}

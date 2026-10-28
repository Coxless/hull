//! 罫線とブロック要素を、フォントを使わずに描く。
//!
//! フォントのグリフのままだと、行の高さによって線のあいだに隙間ができる。
//! tmux の境界線や Claude Code の角丸の枠がつながって見えるように、マスの大きさから線を作る。

/// マスの中の図形 (マスの左上が原点のピクセル)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        alpha: f32,
    },
    /// 角丸の弧 (中心、半径、太さ)。円のうち、マスの真ん中の側の 4 分の 1 だけを描く。
    Arc {
        cx: f32,
        cy: f32,
        radius: f32,
        thickness: f32,
    },
}

/// 上・右・下・左の線の太さ (0: なし、1: 細い、2: 太い)。
fn arms(c: char) -> Option<[u8; 4]> {
    Some(match c {
        '─' => [0, 1, 0, 1],
        '━' => [0, 2, 0, 2],
        '│' => [1, 0, 1, 0],
        '┃' => [2, 0, 2, 0],
        '┌' => [0, 1, 1, 0],
        '┐' => [0, 0, 1, 1],
        '└' => [1, 1, 0, 0],
        '┘' => [1, 0, 0, 1],
        '├' => [1, 1, 1, 0],
        '┤' => [1, 0, 1, 1],
        '┬' => [0, 1, 1, 1],
        '┴' => [1, 1, 0, 1],
        '┼' => [1, 1, 1, 1],
        '┏' => [0, 2, 2, 0],
        '┓' => [0, 0, 2, 2],
        '┗' => [2, 2, 0, 0],
        '┛' => [2, 0, 0, 2],
        '┣' => [2, 2, 2, 0],
        '┫' => [2, 0, 2, 2],
        '┳' => [0, 2, 2, 2],
        '┻' => [2, 2, 0, 2],
        '╋' => [2, 2, 2, 2],
        '╴' => [0, 0, 0, 1],
        '╵' => [1, 0, 0, 0],
        '╶' => [0, 1, 0, 0],
        '╷' => [0, 0, 1, 0],
        '╸' => [0, 0, 0, 2],
        '╹' => [2, 0, 0, 0],
        '╺' => [0, 2, 0, 0],
        '╻' => [0, 0, 2, 0],
        _ => return None,
    })
}

/// この文字を自分で描くか。
pub fn is_box_char(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259f)
        && (arms(c).is_some() || matches!(c, '╭' | '╮' | '╯' | '╰') || block(c, 1.0, 1.0).is_some())
}

/// ブロック要素 (█ ▀ ▄ ▌ ▐ ░ ▒ ▓ と 1/8 刻みのもの)。
fn block(c: char, w: f32, h: f32) -> Option<Shape> {
    let rect = |x: f32, y: f32, rw: f32, rh: f32| Shape::Rect { x, y, w: rw, h: rh, alpha: 1.0 };
    let shade = |alpha: f32| Shape::Rect { x: 0.0, y: 0.0, w, h, alpha };
    let eighth_h = |n: f32| (h * n / 8.0).round();
    let eighth_w = |n: f32| (w * n / 8.0).round();
    Some(match c {
        '█' => rect(0.0, 0.0, w, h),
        '▀' => rect(0.0, 0.0, w, eighth_h(4.0)),
        '▔' => rect(0.0, 0.0, w, eighth_h(1.0)),
        '▌' => rect(0.0, 0.0, eighth_w(4.0), h),
        '▐' => rect(w - eighth_w(4.0), 0.0, eighth_w(4.0), h),
        '▕' => rect(w - eighth_w(1.0), 0.0, eighth_w(1.0), h),
        '░' => shade(0.25),
        '▒' => shade(0.5),
        '▓' => shade(0.75),
        // ▁ (1/8) 〜 ▇ (7/8) と ▄ (1/2)。
        '\u{2581}'..='\u{2587}' => {
            let n = (c as u32 - 0x2580) as f32;
            let bh = eighth_h(n);
            rect(0.0, h - bh, w, bh)
        }
        // ▉ (7/8) 〜 ▏ (1/8)。
        '\u{2589}'..='\u{258f}' => {
            let n = (0x2590 - c as u32) as f32;
            rect(0.0, 0.0, eighth_w(n), h)
        }
        _ => return None,
    })
}

/// 文字の図形。描けない文字は空。`w` と `h` はマスの大きさ、`light` は細い線の太さ。
pub fn shapes(c: char, w: f32, h: f32, light: f32) -> Vec<Shape> {
    if let Some(b) = block(c, w, h) {
        return vec![b];
    }
    let heavy = (light * 2.0).max(light + 1.0);
    let thick = |weight: u8| if weight == 2 { heavy } else { light };
    let (wc, hc) = ((w / 2.0).floor(), (h / 2.0).floor());
    let mut out = Vec::new();

    if let Some([up, right, down, left]) = arms(c) {
        let rect =
            |x: f32, y: f32, rw: f32, rh: f32| Shape::Rect { x, y, w: rw, h: rh, alpha: 1.0 };
        // 交わる所が欠けないよう、縦横の線は真ん中で少し重ねる。
        let cross_v = thick(up.max(down));
        let cross_h = thick(left.max(right));
        if up > 0 {
            let t = thick(up);
            out.push(rect(wc - (t / 2.0).floor(), 0.0, t, hc - (cross_h / 2.0).floor() + cross_h));
        }
        if down > 0 {
            let t = thick(down);
            let y = hc - (cross_h / 2.0).floor();
            out.push(rect(wc - (t / 2.0).floor(), y, t, h - y));
        }
        if left > 0 {
            let t = thick(left);
            out.push(rect(0.0, hc - (t / 2.0).floor(), wc - (cross_v / 2.0).floor() + cross_v, t));
        }
        if right > 0 {
            let t = thick(right);
            let x = wc - (cross_v / 2.0).floor();
            out.push(rect(x, hc - (t / 2.0).floor(), w - x, t));
        }
        return out;
    }

    // 角丸: 縦線と横線の中心を、4 分の 1 の円でつなぐ。
    // 縦線と横線の位置は ─ や │ と同じにする (マスの幅が偶数だと真ん中からずれるので、裏返しでは作らない)。
    let (right, down) = match c {
        '╭' => (true, true),
        '╮' => (false, true),
        '╰' => (true, false),
        '╯' => (false, false),
        _ => return out,
    };
    let vx = wc - (light / 2.0).floor() + light / 2.0;
    let hy = hc - (light / 2.0).floor() + light / 2.0;
    let radius = (if right { w - vx } else { vx }).min(if down { h - hy } else { hy });
    let cx = if right { vx + radius } else { vx - radius };
    let cy = if down { hy + radius } else { hy - radius };
    out.push(Shape::Arc { cx, cy, radius, thickness: light });
    // 弧の先から、マスの端までの直線。
    let (y0, y1) = if down { (cy, h) } else { (0.0, cy) };
    if y1 > y0 {
        out.push(Shape::Rect { x: vx - light / 2.0, y: y0, w: light, h: y1 - y0, alpha: 1.0 });
    }
    let (x0, x1) = if right { (cx, w) } else { (0.0, cx) };
    if x1 > x0 {
        out.push(Shape::Rect { x: x0, y: hy - light / 2.0, w: x1 - x0, h: light, alpha: 1.0 });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 横線はマスの端から端まで() {
        let s = shapes('─', 9.0, 18.0, 1.0);
        let total: f32 = s
            .iter()
            .map(|s| match s {
                Shape::Rect { w, .. } => *w,
                _ => 0.0,
            })
            .sum();
        // 左右の腕が真ん中で重なるので、合わせると幅より長い。
        assert!(total >= 9.0);
        assert!(s.iter().all(|s| matches!(s, Shape::Rect { y: 9.0, h: 1.0, .. })));
    }

    #[test]
    fn 縦線はマスの上から下まで() {
        let s = shapes('│', 9.0, 18.0, 1.0);
        let top = s.iter().any(|s| matches!(s, Shape::Rect { y: 0.0, .. }));
        let bottom = s.iter().any(|s| matches!(s, Shape::Rect { y, h, .. } if y + h == 18.0));
        assert!(top && bottom);
    }

    #[test]
    fn 角丸の端は縦線と横線にそろう() {
        let (w, h) = (8.0, 18.0);
        let vertical = shapes('│', w, h, 1.0);
        let Shape::Rect { x: line_x, .. } = vertical[0] else { panic!() };
        for c in ['╭', '╮', '╰', '╯'] {
            let s = shapes(c, w, h, 1.0);
            let xs: Vec<f32> = s
                .iter()
                .filter_map(|s| match s {
                    Shape::Rect { x, w: 1.0, .. } => Some(*x),
                    _ => None,
                })
                .collect();
            assert_eq!(xs, [line_x], "{c}");
        }
    }

    #[test]
    fn 対象の文字() {
        assert!(is_box_char('┼'));
        assert!(is_box_char('╭'));
        assert!(is_box_char('▄'));
        assert!(!is_box_char('═'));
        assert!(!is_box_char('a'));
    }
}

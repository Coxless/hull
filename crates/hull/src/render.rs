//! GLES3 での描画。
//!
//! すべての図形 (背景、グリフ、線、カーソル) を 1 種類の四角形のインスタンスで描く。
//! 行ごとのインスタンスは CPU 側で作って取っておき、行の世代番号が変わったときだけ作り直す。
//! スクロールしても行の世代番号は変わらないので、作り直さずに位置だけ変えて使い回す。
//!
//! 窓には、前のフレームから変わった行の帯だけを描く (EGL の buffer age で、
//! バッファに残っている中身がいつのものかを知る)。

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use glow::HasContext;
use hull_term::{Cell, Color, CursorShape, Point, Row, Terminal, flags, term::char_width};

use crate::boxdraw::{self, Shape};
use crate::font::{Fonts, Style};
use crate::theme::Scheme;

type Rgb = [u8; 3];

/// GPU に送る 1 つの四角形。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Inst {
    /// x, y, 幅, 高さ (ピクセル)。
    rect: [f32; 4],
    /// グリフならアトラスの位置。図形なら種類ごとの値。
    uv: [f32; 4],
    color: [u8; 4],
    kind: f32,
}

const KIND_RECT: f32 = 0.0;
const KIND_MASK: f32 = 1.0;
const KIND_COLOR: f32 = 2.0;
/// 角丸の四角 (uv.x = 半径、uv.y = 1 なら上の角だけ)。
const KIND_ROUNDED: f32 = 3.0;
/// 見出しのボタンの絵 (uv.x = 絵の番号、uv.y = 線の太さ)。
const KIND_ICON: f32 = 4.0;
/// 罫線の角丸の弧 (uv = 中心 x, 中心 y, 半径, 太さ。マスの中の位置)。
const KIND_ARC: f32 = 5.0;

const VERTEX_SHADER: &str = r#"#version 300 es
layout(location = 0) in vec4 a_rect;
layout(location = 1) in vec4 a_uv;
layout(location = 2) in vec4 a_color;
layout(location = 3) in float a_kind;
uniform vec2 u_viewport;
uniform vec2 u_mask_size;
uniform vec2 u_color_size;
out vec2 v_uv;
out vec2 v_local;
out vec2 v_size;
out vec4 v_params;
out vec4 v_color;
flat out int v_kind;
void main() {
    vec2 corner = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
    vec2 pos = a_rect.xy + corner * a_rect.zw;
    gl_Position = vec4(pos.x / u_viewport.x * 2.0 - 1.0, 1.0 - pos.y / u_viewport.y * 2.0, 0.0, 1.0);
    v_kind = int(a_kind + 0.5);
    vec2 atlas = v_kind == 2 ? u_color_size : u_mask_size;
    v_uv = (a_uv.xy + corner * a_uv.zw) / atlas;
    v_local = corner * a_rect.zw;
    v_size = a_rect.zw;
    v_params = a_uv;
    v_color = a_color;
}
"#;

const FRAGMENT_SHADER: &str = r#"#version 300 es
precision mediump float;
in vec2 v_uv;
in vec2 v_local;
in vec2 v_size;
in vec4 v_params;
in vec4 v_color;
flat in int v_kind;
uniform sampler2D u_mask;
uniform sampler2D u_color;
out vec4 frag;

float segment(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a, ba = b - a;
    float h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

float box_outline(vec2 p, vec2 half_size) {
    vec2 d = abs(p) - half_size;
    return abs(length(max(d, 0.0)) + min(max(d.x, d.y), 0.0));
}

void main() {
    float a = v_color.a;
    if (v_kind == 1) {
        a *= texture(u_mask, v_uv).r;
    } else if (v_kind == 2) {
        vec4 t = texture(u_color, v_uv);
        frag = vec4(t.rgb * t.a, t.a) * v_color.a;
        return;
    } else if (v_kind == 3) {
        float r = v_params.x;
        vec2 half_size = v_size * 0.5;
        vec2 p = v_local - half_size;
        if (v_params.y > 0.5 && p.y > 0.0) {
            r = 0.0;
        }
        vec2 q = abs(p) - half_size + r;
        float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
        a *= clamp(0.5 - d, 0.0, 1.0);
    } else if (v_kind == 4) {
        vec2 p = v_local - v_size * 0.5;
        float s = min(v_size.x, v_size.y) * 0.5;
        float t = v_params.y * 0.5;
        float d;
        int icon = int(v_params.x + 0.5);
        if (icon == 0) {
            d = segment(p, vec2(-s, 0.0), vec2(s, 0.0));
        } else if (icon == 1) {
            d = box_outline(p, vec2(s));
        } else if (icon == 2) {
            float back = box_outline(p - vec2(s * 0.3, -s * 0.3), vec2(s * 0.7));
            float front = box_outline(p + vec2(s * 0.3, -s * 0.3), vec2(s * 0.7));
            vec2 fp = p + vec2(s * 0.3, -s * 0.3);
            bool inside_front = all(lessThan(abs(fp), vec2(s * 0.7 - t)));
            d = min(inside_front ? 1e3 : back, front);
        } else {
            d = min(segment(p, vec2(-s), vec2(s)), segment(p, vec2(-s, s), vec2(s, -s)));
        }
        a *= clamp(t + 0.5 - d, 0.0, 1.0);
    } else if (v_kind == 5) {
        vec2 c = v_params.xy;
        float d = abs(length(v_local - c) - v_params.z) - v_params.w * 0.5;
        // 弧は中心から見てマスの側の 4 分の 1 だけ。
        vec2 dir = v_local - c;
        vec2 toward = vec2(c.x > v_size.x * 0.5 ? -1.0 : 1.0, c.y > v_size.y * 0.5 ? -1.0 : 1.0);
        if (dir.x * toward.x < -0.5 || dir.y * toward.y < -0.5) {
            discard;
        }
        a *= clamp(0.5 - d, 0.0, 1.0);
    }
    frag = vec4(v_color.rgb * a, a);
}
"#;

/// アトラスの中のグリフ。
#[derive(Clone, Copy, Debug)]
struct AtlasGlyph {
    color: bool,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    left: i16,
    top: i16,
}

/// 棚に詰めていくだけの単純なアトラス。
struct Atlas {
    texture: glow::Texture,
    size: u32,
    max_size: u32,
    color: bool,
    x: u32,
    y: u32,
    row_h: u32,
}

impl Atlas {
    fn new(gl: &glow::Context, size: u32, max_size: u32, color: bool) -> Atlas {
        // SAFETY: 文脈は current。作ったテクスチャはこの構造体が持つ。
        let texture = unsafe { gl.create_texture().expect("テクスチャを作れない") };
        let mut atlas = Atlas { texture, size, max_size, color, x: 0, y: 0, row_h: 0 };
        atlas.allocate(gl);
        atlas
    }

    fn allocate(&mut self, gl: &glow::Context) {
        let (internal, format) =
            if self.color { (glow::RGBA8, glow::RGBA) } else { (glow::R8, glow::RED) };
        // SAFETY: 文脈は current。中身は空で確保する。
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                internal as i32,
                self.size as i32,
                self.size as i32,
                0,
                format,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            let filter = if self.color { glow::LINEAR } else { glow::NEAREST };
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filter as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filter as i32);
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
        }
        self.x = 0;
        self.y = 0;
        self.row_h = 0;
    }

    /// 場所を取る。いっぱいなら `None` (呼び出し側がアトラスを空にして作り直す)。
    fn reserve(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.size || h > self.size {
            return None;
        }
        if self.x + w > self.size {
            self.x = 0;
            self.y += self.row_h + 1;
            self.row_h = 0;
        }
        if self.y + h > self.size {
            return None;
        }
        let at = (self.x, self.y);
        self.x += w + 1;
        self.row_h = self.row_h.max(h);
        Some(at)
    }

    /// 大きくできるなら大きくする (中身は消える)。
    fn grow(&mut self, gl: &glow::Context) -> bool {
        if self.size >= self.max_size {
            self.allocate(gl);
            return false;
        }
        self.size *= 2;
        self.allocate(gl);
        true
    }

    fn upload(&self, gl: &glow::Context, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        let format = if self.color { glow::RGBA } else { glow::RED };
        // SAFETY: 文脈は current。data の大きさは w * h * チャンネル数。
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                x as i32,
                y as i32,
                w as i32,
                h as i32,
                format,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(data)),
            );
        }
    }
}

/// 窓の中の配置 (バッファのピクセル)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    /// 見出しの帯の高さ。帯を描かないなら 0。
    pub header: u32,
    pub padding: u32,
    pub scale: f32,
}

impl Layout {
    /// 文字を並べる範囲の左上。
    pub fn origin(&self) -> (f32, f32) {
        (self.padding as f32, (self.header + self.padding) as f32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

/// 見出しのボタンの位置 (x, y, 幅, 高さ)。描くときとクリックの判定で同じものを使う。
pub fn header_buttons(layout: &Layout) -> [(Button, [f32; 4]); 3] {
    let size = (24.0 * layout.scale).round();
    let gap = (10.0 * layout.scale).round();
    let y = ((layout.header as f32 - size) / 2.0).round();
    let right = layout.width as f32 - gap;
    let at = |i: f32| [right - size * (i + 1.0) - gap * i, y, size, size];
    [(Button::Close, at(0.0)), (Button::Maximize, at(1.0)), (Button::Minimize, at(2.0))]
}

pub struct Header<'a> {
    pub title: &'a str,
    pub focused: bool,
    pub hovered: Option<Button>,
    pub maximized: bool,
    /// 角を丸めるか (最大化やタイル表示では丸めない)。
    pub rounded: bool,
}

/// プラグインが付けるハイライト (見えている行で指定する)。
#[derive(Clone, Debug, PartialEq, Hash)]
pub struct Highlight {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub color: Rgb,
}

/// プラグインが出すオーバーレイの文字。
#[derive(Clone, Debug, PartialEq, Hash)]
pub struct Overlay {
    pub row: usize,
    pub col: usize,
    pub text: String,
    pub fg: Rgb,
    pub bg: Rgb,
}

pub struct Scene<'a> {
    pub term: &'a Terminal,
    pub scheme: &'a Scheme,
    pub layout: Layout,
    pub focused: bool,
    pub selection: Option<(Point, Point)>,
    pub highlights: &'a [Highlight],
    pub overlays: &'a [Overlay],
    /// IME の変換中の文字。
    pub preedit: Option<&'a str>,
    pub header: Option<Header<'a>>,
}

/// 1 フレームで変わった所。
#[derive(Clone, Copy, Debug)]
struct FrameDamage {
    full: bool,
    rows: Option<(usize, usize)>,
    header: bool,
}

fn union(a: Option<(usize, usize)>, b: (usize, usize)) -> (usize, usize) {
    a.map_or(b, |(s, e)| (s.min(b.0), e.max(b.1)))
}

/// 1 行分のインスタンス (行の左上が原点)。
struct RowInstances {
    back: Vec<Inst>,
    front: Vec<Inst>,
}

pub struct Renderer {
    program: glow::Program,
    u_viewport: Option<glow::UniformLocation>,
    u_mask_size: Option<glow::UniformLocation>,
    u_color_size: Option<glow::UniformLocation>,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    vbo_capacity: usize,
    mask: Atlas,
    color: Atlas,
    glyphs: HashMap<(usize, u16, u8), Option<AtlasGlyph>>,
    /// ASCII と書体の組み合わせは、ハッシュを引かずに配列で引く。
    ascii: Box<[[Option<Option<AtlasGlyph>>; 128]; 4]>,
    rows: HashMap<(bool, u64), RowInstances>,
    /// 前のフレームの、見えている行ごとの中身の印。変わった行を見つけるのに使う。
    row_marks: Vec<u64>,
    header_mark: u64,
    frame_mark: u64,
    /// 過去のフレームで変わった所 (新しいものが先頭)。buffer age の分だけ合わせて描く。
    history: Vec<FrameDamage>,
    palette_key: u64,
    scratch: Vec<Inst>,
}

fn rgba(rgb: Rgb, alpha: u8) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], alpha]
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// 塗りつぶした四角形。
fn solid(rect: [f32; 4], c: Rgb) -> Inst {
    Inst { rect, color: rgba(c, 255), kind: KIND_RECT, ..Inst::default() }
}

/// 色を指定していないセルの背景色 (画面全体の反転を効かせたあと)。
fn default_bg(term: &Terminal) -> Rgb {
    let index = if term.modes.reverse_video {
        hull_term::term::COLOR_FG
    } else {
        hull_term::term::COLOR_BG
    };
    term.palette.get(index)
}

/// カーソルを見せるか (アプリが隠していない、かつ一番下を見ている)。
fn show_cursor(term: &Terminal) -> bool {
    term.modes.show_cursor && term.display_offset() == 0
}

fn hash<T: Hash>(value: &T) -> u64 {
    let mut h = DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

impl Renderer {
    pub fn new(gl: &glow::Context) -> Result<Renderer, String> {
        // SAFETY: 文脈は current。作った GL の物はこの構造体が持つ。
        unsafe {
            let program = gl.create_program()?;
            for (kind, src) in
                [(glow::VERTEX_SHADER, VERTEX_SHADER), (glow::FRAGMENT_SHADER, FRAGMENT_SHADER)]
            {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, src);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return Err(format!("シェーダーのエラー: {}", gl.get_shader_info_log(shader)));
                }
                gl.attach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(format!("シェーダーのエラー: {}", gl.get_program_info_log(program)));
            }
            gl.use_program(Some(program));
            gl.uniform_1_i32(gl.get_uniform_location(program, "u_mask").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(program, "u_color").as_ref(), 1);

            let vao = gl.create_vertex_array()?;
            let vbo = gl.create_buffer()?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let stride = std::mem::size_of::<Inst>() as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 4, glow::FLOAT, false, stride, 0);
            gl.vertex_attrib_divisor(0, 1);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 4, glow::FLOAT, false, stride, 16);
            gl.vertex_attrib_divisor(1, 1);
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 4, glow::UNSIGNED_BYTE, true, stride, 32);
            gl.vertex_attrib_divisor(2, 1);
            gl.enable_vertex_attrib_array(3);
            gl.vertex_attrib_pointer_f32(3, 1, glow::FLOAT, false, stride, 36);
            gl.vertex_attrib_divisor(3, 1);

            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            gl.disable(glow::DEPTH_TEST);

            let mask = Atlas::new(gl, 1024, 4096, false);
            let color = Atlas::new(gl, 256, 2048, true);
            Ok(Renderer {
                program,
                u_viewport: gl.get_uniform_location(program, "u_viewport"),
                u_mask_size: gl.get_uniform_location(program, "u_mask_size"),
                u_color_size: gl.get_uniform_location(program, "u_color_size"),
                vao,
                vbo,
                vbo_capacity: 0,
                mask,
                color,
                glyphs: HashMap::new(),
                ascii: Box::new([[None; 128]; 4]),
                rows: HashMap::new(),
                row_marks: Vec::new(),
                header_mark: 0,
                frame_mark: 0,
                history: Vec::new(),
                palette_key: 0,
                scratch: Vec::new(),
            })
        }
    }

    /// 次のフレームで全体を描き直させる。
    pub fn invalidate(&mut self) {
        self.frame_mark = 0;
    }

    /// フォントが変わったときに、グリフと行の作り置きを捨てる。
    pub fn clear_glyphs(&mut self, gl: &glow::Context) {
        self.drop_caches();
        self.mask.allocate(gl);
        self.color.allocate(gl);
    }

    /// グリフと行の作り置きを捨てる (次のフレームで作り直す)。
    fn drop_caches(&mut self) {
        self.glyphs.clear();
        *self.ascii = [[None; 128]; 4];
        self.rows.clear();
        self.history.clear();
    }

    fn glyph(
        &mut self,
        gl: &glow::Context,
        fonts: &mut Fonts,
        ch: char,
        style: Style,
        cells: u8,
    ) -> Option<AtlasGlyph> {
        let style_index = style.bold as usize | (style.italic as usize) << 1;
        if (ch as u32) < 128
            && cells == 1
            && let Some(hit) = self.ascii[style_index][ch as usize]
        {
            return hit;
        }
        let (face, gid) = fonts.resolve(ch, style);
        let key = (face, gid, cells);
        let entry = match self.glyphs.get(&key) {
            Some(hit) => *hit,
            None => {
                let entry = self.rasterize(gl, fonts, face, gid, cells);
                self.glyphs.insert(key, entry);
                entry
            }
        };
        if (ch as u32) < 128 && cells == 1 {
            self.ascii[style_index][ch as usize] = Some(entry);
        }
        entry
    }

    fn rasterize(
        &mut self,
        gl: &glow::Context,
        fonts: &mut Fonts,
        face: usize,
        gid: u16,
        cells: u8,
    ) -> Option<AtlasGlyph> {
        let g = fonts.rasterize(face, gid, u32::from(cells))?;
        if g.width == 0 || g.height == 0 {
            return None;
        }
        let atlas = if g.color { &mut self.color } else { &mut self.mask };
        let (x, y) = match atlas.reserve(g.width, g.height) {
            Some(at) => at,
            None => {
                // いっぱいになったら大きくして、作り置きをすべて捨てる (次のフレームで作り直す)。
                atlas.grow(gl);
                let at = atlas.reserve(g.width, g.height)?;
                self.drop_caches();
                at
            }
        };
        let atlas = if g.color { &self.color } else { &self.mask };
        atlas.upload(gl, x, y, g.width, g.height, &g.data);
        Some(AtlasGlyph {
            color: g.color,
            x: x as u16,
            y: y as u16,
            w: g.width as u16,
            h: g.height as u16,
            left: g.left as i16,
            top: g.top as i16,
        })
    }

    fn resolve(term: &Terminal, color: Color, fg: bool) -> Rgb {
        match color {
            Color::Default => {
                if fg {
                    term.palette.get(hull_term::term::COLOR_FG)
                } else {
                    term.palette.get(hull_term::term::COLOR_BG)
                }
            }
            Color::Indexed(i) => term.palette.get(usize::from(i)),
            Color::Rgb(r, g, b) => [r, g, b],
        }
    }

    /// セルの文字色と背景色 (反転などを効かせたあと)。
    fn cell_colors(term: &Terminal, cell: &Cell) -> (Rgb, Rgb) {
        let mut fg = Self::resolve(term, cell.fg, true);
        let mut bg = Self::resolve(term, cell.bg, false);
        if cell.flags & flags::INVERSE != 0 {
            std::mem::swap(&mut fg, &mut bg);
        }
        if term.modes.reverse_video {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags & flags::DIM != 0 {
            fg = mix(fg, bg, 0.4);
        }
        if cell.flags & flags::HIDDEN != 0 {
            fg = bg;
        }
        (fg, bg)
    }

    fn build_row(
        &mut self,
        gl: &glow::Context,
        fonts: &mut Fonts,
        term: &Terminal,
        row: &Row,
        cols: usize,
    ) -> RowInstances {
        let m = fonts.metrics;
        let (cw, ch) = (m.cell_width as f32, m.cell_height as f32);
        let default_bg = default_bg(term);
        let light = (cw / 8.0).round().max(1.0).min(m.stroke as f32 * 2.0).max(1.0);
        let mut back = Vec::new();
        let mut front = Vec::new();
        let mut run: Option<(usize, usize, Rgb)> = None;
        let flush = |run: &mut Option<(usize, usize, Rgb)>, back: &mut Vec<Inst>| {
            if let Some((start, end, color)) = run.take() {
                back.push(solid([start as f32 * cw, 0.0, (end - start) as f32 * cw, ch], color));
            }
        };

        for col in 0..cols.min(row.len()) {
            let cell = row.cell(col);
            if cell.flags & flags::WIDE_SPACER != 0 {
                // 背景は左半分と同じ色で塗る。
                if let Some(r) = &mut run
                    && r.1 == col
                {
                    r.1 = col + 1;
                }
                continue;
            }
            let (fg, bg) = Self::cell_colors(term, &cell);
            let wide = cell.flags & flags::WIDE != 0;
            let span = if wide { 2 } else { 1 };
            if bg != default_bg {
                match &mut run {
                    Some((_, end, color)) if *end == col && *color == bg => *end = col + span,
                    _ => {
                        flush(&mut run, &mut back);
                        run = Some((col, col + span, bg));
                    }
                }
            } else {
                flush(&mut run, &mut back);
            }
            let x = col as f32 * cw;
            self.push_glyphs(gl, fonts, &mut front, &cell, row, col, x, 0.0, fg, light);

            let width = span as f32 * cw;
            let stroke = m.stroke as f32;
            let underline =
                |y: f32, front: &mut Vec<Inst>| front.push(solid([x, y, width, stroke], fg));
            if cell.flags & (flags::UNDERLINE | flags::CURLY_UNDERLINE) != 0 {
                underline(m.underline_position as f32, &mut front);
            }
            if cell.flags & flags::DOUBLE_UNDERLINE != 0 {
                let y = (m.underline_position as f32).min(ch - stroke * 3.0);
                underline(y, &mut front);
                underline(y + stroke * 2.0, &mut front);
            }
            if cell.flags & flags::STRIKE != 0 {
                underline(m.strikeout_position as f32, &mut front);
            }
        }
        flush(&mut run, &mut back);
        RowInstances { back, front }
    }

    /// 1 文字 (と結合文字) のグリフを置く。罫線は自分で描く。
    #[allow(clippy::too_many_arguments)]
    fn push_glyphs(
        &mut self,
        gl: &glow::Context,
        fonts: &mut Fonts,
        out: &mut Vec<Inst>,
        cell: &Cell,
        row: &Row,
        col: usize,
        x: f32,
        y: f32,
        fg: Rgb,
        light: f32,
    ) {
        let c = cell.ch;
        if c == ' ' && cell.flags & flags::COMBINING == 0 {
            return;
        }
        let m = fonts.metrics;
        let (cw, chh) = (m.cell_width as f32, m.cell_height as f32);
        if boxdraw::is_box_char(c) {
            for shape in boxdraw::shapes(c, cw, chh, light) {
                out.push(match shape {
                    Shape::Rect { x: sx, y: sy, w, h, alpha } => Inst {
                        rect: [x + sx, y + sy, w, h],
                        color: rgba(fg, (alpha * 255.0) as u8),
                        kind: KIND_RECT,
                        ..Inst::default()
                    },
                    Shape::Arc { cx, cy, radius, thickness, .. } => Inst {
                        rect: [x, y, cw, chh],
                        uv: [cx, cy, radius, thickness],
                        color: rgba(fg, 255),
                        kind: KIND_ARC,
                    },
                });
            }
            return;
        }
        let style =
            Style { bold: cell.flags & flags::BOLD != 0, italic: cell.flags & flags::ITALIC != 0 };
        let cells = if cell.flags & flags::WIDE != 0 { 2 } else { 1 };
        let mut push = |this: &mut Self, ch: char, cells: u8| {
            if let Some(g) = this.glyph(gl, fonts, ch, style, cells) {
                let gx = x + f32::from(g.left);
                let gy = y + m.baseline as f32 - f32::from(g.top);
                out.push(Inst {
                    rect: [gx, gy, f32::from(g.w), f32::from(g.h)],
                    uv: [f32::from(g.x), f32::from(g.y), f32::from(g.w), f32::from(g.h)],
                    color: rgba(fg, 255),
                    kind: if g.color { KIND_COLOR } else { KIND_MASK },
                });
            }
        };
        push(self, c, cells);
        if cell.flags & flags::COMBINING != 0 {
            let combining_cells = char_width(c).max(1) as u8;
            for &(cc, ch) in &row.combining {
                if cc as usize == col {
                    push(self, ch, if ch == c { cells } else { combining_cells });
                }
            }
        }
    }

    /// 文字列を置く (見出しやオーバーレイ用)。置いた幅 (マス) を返す。
    #[allow(clippy::too_many_arguments)]
    fn push_text(
        &mut self,
        gl: &glow::Context,
        fonts: &mut Fonts,
        out: &mut Vec<Inst>,
        text: &str,
        x: f32,
        y: f32,
        fg: Rgb,
        bold: bool,
    ) -> usize {
        let cw = fonts.metrics.cell_width as f32;
        let mut col = 0;
        let row = Row::default();
        for c in text.chars() {
            let w = char_width(c);
            if w == 0 {
                continue;
            }
            let flags = if bold { flags::BOLD } else { 0 } | if w == 2 { flags::WIDE } else { 0 };
            let cell = Cell { ch: c, flags, ..Cell::BLANK };
            self.push_glyphs(gl, fonts, out, &cell, &row, 0, x + col as f32 * cw, y, fg, 1.0);
            col += w;
        }
        col
    }

    /// 見えている行ごとの印 (中身、カーソル、選択、ハイライト、オーバーレイ)。前のフレームと比べて、変わった行を探す。
    fn row_marks(scene: &Scene, rows: usize, palette_key: u64) -> Vec<u64> {
        let term = scene.term;
        let (crow, ccol) = term.cursor();
        let show_cursor = show_cursor(term);
        (0..rows)
            .map(|r| {
                let mut h = DefaultHasher::new();
                (term.is_alt_screen(), term.visible_row(r).generation, palette_key).hash(&mut h);
                if show_cursor && r == crow {
                    (ccol, scene.focused, term.modes.cursor_shape as u8, scene.preedit)
                        .hash(&mut h);
                }
                if let Some((s, e)) = scene.selection {
                    let abs = term.visible_abs(r);
                    if s.line <= abs && abs <= e.line {
                        (s, e).hash(&mut h);
                    }
                }
                for hl in scene.highlights.iter().filter(|hl| hl.row == r) {
                    hl.hash(&mut h);
                }
                for ov in scene.overlays.iter().filter(|o| o.row == r) {
                    ov.hash(&mut h);
                }
                h.finish()
            })
            .collect()
    }

    /// 描く。窓に知らせる変わった範囲を返す (空なら全体)。何も変わっていなければ `None`。
    pub fn render(
        &mut self,
        gl: &glow::Context,
        fonts: &mut Fonts,
        scene: &Scene,
        buffer_age: u32,
    ) -> Option<Vec<[i32; 4]>> {
        let term = scene.term;
        let layout = scene.layout;
        let m = fonts.metrics;
        let (cw, chh) = (m.cell_width as f32, m.cell_height as f32);
        let rows = term.rows();
        let cols = term.cols();
        let (ox, oy) = layout.origin();

        // 色の表が変わったら、行の作り置きをすべて捨てる。
        let palette_key = hash(&(
            (0..259).map(|i| term.palette.get(i)).collect::<Vec<_>>(),
            term.modes.reverse_video,
            scene.scheme.dark,
        ));
        if palette_key != self.palette_key {
            self.rows.clear();
            self.palette_key = palette_key;
        }

        // どの行を描き直すか決める。
        let marks = Self::row_marks(scene, rows, palette_key);
        let frame_mark = hash(&(
            layout.width,
            layout.height,
            layout.header,
            layout.padding,
            cw as u32,
            chh as u32,
        ));
        let header_mark = scene.header.as_ref().map_or(0, |h| {
            hash(&(h.title, h.focused, h.hovered, h.maximized, h.rounded, scene.scheme.dark))
        });
        let full = frame_mark != self.frame_mark || marks.len() != self.row_marks.len();
        let mut band: Option<(usize, usize)> = None;
        if !full {
            for (r, (a, b)) in marks.iter().zip(&self.row_marks).enumerate() {
                if a != b {
                    band = Some(union(band, (r, r)));
                }
            }
        }
        let header_changed = full || header_mark != self.header_mark;
        if !full && band.is_none() && !header_changed {
            // 何も変わっていない。描かず、窓にも出さない。
            return None;
        }
        self.row_marks = marks;
        self.frame_mark = frame_mark;
        self.header_mark = header_mark;

        // buffer age の分だけ、前のフレームで変わった所も合わせて描く。
        let this = FrameDamage { full, rows: band, header: header_changed };
        self.history.insert(0, this);
        self.history.truncate(4);
        let age = buffer_age as usize;
        let mut redraw_all = full || age == 0 || age > self.history.len();
        let mut draw_band = band;
        let mut header_redraw = header_changed;
        if !redraw_all {
            for past in &self.history[1..age] {
                if past.full {
                    redraw_all = true;
                    break;
                }
                if let Some(rows) = past.rows {
                    draw_band = Some(union(draw_band, rows));
                }
                header_redraw |= past.header;
            }
        }
        header_redraw |= redraw_all;

        // 描く範囲 (行の帯) を、窓のピクセルで決める。
        let band_pixels = |(s, e): (usize, usize)| -> [i32; 4] {
            let y0 = if s == 0 { layout.header as f32 } else { oy + s as f32 * chh };
            let y1 = if e + 1 == rows { layout.height as f32 } else { oy + (e + 1) as f32 * chh };
            [0, y0 as i32, layout.width as i32, (y1 - y0) as i32]
        };
        // 描く行の帯と、窓の中の範囲。端末の部分を描かないときは帯が空 (1, 0)。
        let (band_rows, scissor): ((usize, usize), Option<[i32; 4]>) = if redraw_all {
            let top = layout.header as i32;
            (
                (0, rows.saturating_sub(1)),
                Some([0, top, layout.width as i32, layout.height as i32 - top]),
            )
        } else if let Some(b) = draw_band {
            (b, Some(band_pixels(b)))
        } else {
            ((1, 0), None)
        };

        let mut insts = std::mem::take(&mut self.scratch);
        insts.clear();
        let default_bg = default_bg(term);

        // 行のインスタンスを集める (作り置きがなければ作る)。
        let mut fronts: Vec<(f32, (bool, u64))> = Vec::new();
        let alt = term.is_alt_screen();
        let mut kept = HashMap::with_capacity(rows);
        for r in 0..rows {
            let row = term.visible_row(r);
            let key = (alt, row.generation);
            let entry = match self.rows.remove(&key) {
                Some(e) => e,
                None => self.build_row(gl, fonts, term, row, cols),
            };
            kept.insert(key, entry);
            if r >= band_rows.0 && r <= band_rows.1 {
                let y = oy + r as f32 * chh;
                for inst in &kept[&key].back {
                    let mut i = *inst;
                    i.rect[0] += ox;
                    i.rect[1] += y;
                    insts.push(i);
                }
                fronts.push((y, key));
            }
        }
        self.rows = kept;
        let in_band = |r: usize| r >= band_rows.0 && r <= band_rows.1;

        // 選択とハイライトは、背景の上、文字の下に塗る。
        if let Some((s, e)) = scene.selection {
            for r in (0..rows).filter(|&r| in_band(r)) {
                let abs = term.visible_abs(r);
                if abs < s.line || abs > e.line {
                    continue;
                }
                let start = if abs == s.line { s.col } else { 0 };
                let end = if abs == e.line { (e.col + 1).min(cols) } else { cols };
                if start < end {
                    insts.push(solid(
                        [
                            ox + start as f32 * cw,
                            oy + r as f32 * chh,
                            (end - start) as f32 * cw,
                            chh,
                        ],
                        scene.scheme.selection,
                    ));
                }
            }
        }
        for hl in scene.highlights.iter().filter(|h| in_band(h.row) && h.row < rows) {
            let (start, end) = (hl.start.min(cols), hl.end.min(cols));
            if start < end {
                insts.push(solid(
                    [
                        ox + start as f32 * cw,
                        oy + hl.row as f32 * chh,
                        (end - start) as f32 * cw,
                        chh,
                    ],
                    hl.color,
                ));
            }
        }

        for (y, key) in &fronts {
            for inst in &self.rows[key].front {
                let mut i = *inst;
                i.rect[0] += ox;
                i.rect[1] += y;
                insts.push(i);
            }
        }

        // カーソル。
        let (crow, ccol) = term.cursor();
        if show_cursor(term) && crow < rows && in_band(crow) {
            let cell = term.grid().row(crow).cell(ccol);
            let wide = if cell.flags & flags::WIDE != 0 { 2.0 } else { 1.0 };
            let (x, y) = (ox + ccol as f32 * cw, oy + crow as f32 * chh);
            let cursor_color = term.palette.get(hull_term::term::COLOR_CURSOR);
            let thin = (m.stroke as f32).max((layout.scale).round());
            let rect = |x: f32, y: f32, w: f32, h: f32, c: Rgb| solid([x, y, w, h], c);
            match (scene.focused, term.modes.cursor_shape) {
                (true, CursorShape::Block) if scene.preedit.is_none() => {
                    insts.push(rect(x, y, cw * wide, chh, cursor_color));
                    // カーソルの下の文字は背景色で描き直す。
                    if cell.ch != ' ' && cell.flags & flags::WIDE_SPACER == 0 {
                        let row = term.grid().row(crow);
                        let (_, bg) = Self::cell_colors(term, &cell);
                        let fg = if bg == cursor_color { default_bg } else { bg };
                        self.push_glyphs(gl, fonts, &mut insts, &cell, row, ccol, x, y, fg, 1.0);
                    }
                }
                (true, CursorShape::Bar) => insts.push(rect(x, y, thin, chh, cursor_color)),
                (true, CursorShape::Underline) => {
                    insts.push(rect(x, y + chh - thin, cw * wide, thin, cursor_color))
                }
                (false, _) | (true, CursorShape::Block) => {
                    let w = cw * wide;
                    insts.push(rect(x, y, w, thin, cursor_color));
                    insts.push(rect(x, y + chh - thin, w, thin, cursor_color));
                    insts.push(rect(x, y, thin, chh, cursor_color));
                    insts.push(rect(x + w - thin, y, thin, chh, cursor_color));
                }
            }
            // IME の変換中の文字は、カーソルの位置に下線つきで出す。
            if let Some(text) = scene.preedit.filter(|t| !t.is_empty()) {
                let width = hull_plugin::width(text);
                let fg = term.palette.get(hull_term::term::COLOR_FG);
                insts.push(rect(x, y, width as f32 * cw, chh, default_bg));
                self.push_text(gl, fonts, &mut insts, text, x, y, fg, false);
                insts.push(rect(x, y + m.underline_position as f32, width as f32 * cw, thin, fg));
            }
        }

        // プラグインのオーバーレイは、いちばん上に出す。
        for ov in scene.overlays.iter().filter(|o| in_band(o.row) && o.row < rows) {
            let (x, y) = (ox + ov.col as f32 * cw, oy + ov.row as f32 * chh);
            let width = hull_plugin::width(&ov.text);
            insts.push(solid([x, y, width as f32 * cw, chh], ov.bg));
            self.push_text(gl, fonts, &mut insts, &ov.text, x, y, ov.fg, true);
        }

        // 見出しの帯。
        let header_insts = if header_redraw && layout.header > 0 {
            scene.header.as_ref().map(|h| self.header(gl, fonts, scene, h))
        } else {
            None
        };

        // SAFETY: 文脈は current。描画に使う GL の物はこの構造体が持つ。
        unsafe {
            gl.viewport(0, 0, layout.width as i32, layout.height as i32);
            gl.use_program(Some(self.program));
            gl.uniform_2_f32(self.u_viewport.as_ref(), layout.width as f32, layout.height as f32);
            let mask_size = self.mask.size as f32;
            gl.uniform_2_f32(self.u_mask_size.as_ref(), mask_size, mask_size);
            let color_size = self.color.size as f32;
            gl.uniform_2_f32(self.u_color_size.as_ref(), color_size, color_size);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.mask.texture));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.color.texture));
            gl.bind_vertex_array(Some(self.vao));

            let bg = rgba(default_bg, 255).map(|v| f32::from(v) / 255.0);
            gl.enable(glow::SCISSOR_TEST);
            // 端末の部分。
            if let Some([x, y, w, h]) = scissor {
                gl.scissor(x, layout.height as i32 - y - h, w, h);
                gl.clear_color(bg[0], bg[1], bg[2], 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
                self.draw(gl, &insts);
            }

            // 見出しの部分。
            if let Some(h) = &header_insts {
                gl.scissor(
                    0,
                    layout.height as i32 - layout.header as i32,
                    layout.width as i32,
                    layout.header as i32,
                );
                gl.clear_color(0.0, 0.0, 0.0, 0.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
                self.draw(gl, h);
            }
            gl.disable(glow::SCISSOR_TEST);
        }
        self.scratch = insts;

        // 窓に知らせるのは、前に出したフレームから変わった所だけ。
        let mut damage = Vec::new();
        if full {
            return Some(damage);
        }
        if let Some(b) = band {
            damage.push(band_pixels(b));
        }
        if header_changed && layout.header > 0 {
            damage.push([0, 0, layout.width as i32, layout.header as i32]);
        }
        Some(damage)
    }

    fn draw(&mut self, gl: &glow::Context, insts: &[Inst]) {
        if insts.is_empty() {
            return;
        }
        let bytes = std::mem::size_of_val(insts);
        // SAFETY: Inst は repr(C) の素朴な値なので、そのままバイト列として送れる。
        unsafe {
            let data = std::slice::from_raw_parts(insts.as_ptr().cast::<u8>(), bytes);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            if bytes > self.vbo_capacity {
                self.vbo_capacity = bytes.next_power_of_two();
                gl.buffer_data_size(
                    glow::ARRAY_BUFFER,
                    self.vbo_capacity as i32,
                    glow::STREAM_DRAW,
                );
            }
            gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, data);
            gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, insts.len() as i32);
        }
    }

    fn header(
        &mut self,
        gl: &glow::Context,
        fonts: &mut Fonts,
        scene: &Scene,
        h: &Header,
    ) -> Vec<Inst> {
        let layout = scene.layout;
        let s = scene.scheme;
        let (bg, fg) = if h.focused {
            (s.header, s.header_text)
        } else {
            (s.header_backdrop, s.header_text_backdrop)
        };
        let mut out = Vec::new();
        let radius = if h.rounded { (12.0 * layout.scale).round() } else { 0.0 };
        out.push(Inst {
            rect: [0.0, 0.0, layout.width as f32, layout.header as f32],
            uv: [radius, 1.0, 0.0, 0.0],
            color: rgba(bg, 255),
            kind: KIND_ROUNDED,
        });
        // 題名は真ん中に。ボタンにかからないよう、長ければ切る。
        let cw = fonts.metrics.cell_width as f32;
        let buttons = header_buttons(&layout);
        let max_cols = ((layout.width as f32 - 2.0 * (layout.width as f32 - buttons[2].1[0])) / cw)
            .max(0.0) as usize;
        let mut title = String::new();
        let mut width = 0;
        for c in h.title.chars() {
            let w = char_width(c);
            if width + w > max_cols {
                if title.pop().is_some() {
                    title.push('…');
                }
                break;
            }
            title.push(c);
            width += w;
        }
        let x = ((layout.width as f32 - width as f32 * cw) / 2.0).round();
        let y = ((layout.header as f32 - fonts.metrics.cell_height as f32) / 2.0).round();
        self.push_text(gl, fonts, &mut out, &title, x, y, fg, true);

        for (button, [bx, by, bw, bh]) in buttons {
            if h.hovered == Some(button) {
                out.push(Inst {
                    rect: [bx, by, bw, bh],
                    uv: [bw / 2.0, 0.0, 0.0, 0.0],
                    color: rgba(s.button_hover, 255),
                    kind: KIND_ROUNDED,
                });
            }
            let icon = match button {
                Button::Minimize => 0.0,
                Button::Maximize if h.maximized => 2.0,
                Button::Maximize => 1.0,
                Button::Close => 3.0,
            };
            let pad = (bw * 0.33).round();
            out.push(Inst {
                rect: [bx + pad, by + pad, bw - pad * 2.0, bh - pad * 2.0],
                uv: [icon, (1.2 * layout.scale).max(1.0), 0.0, 0.0],
                color: rgba(fg, 255),
                kind: KIND_ICON,
            });
        }
        out
    }
}

//! フォントの検索とラスタライズ。
//!
//! 検索とフォールバックは fontconfig、ラスタライズは swash (Rust だけで完結し、FreeType を要らない)。
//! フォントのファイルは mmap で開くので、Noto CJK のような大きなファイルも触った分しかメモリを使わない。
//! リガチャはやらないので、シェーピングはせず、1 文字ずつ文字コードからグリフを引く。

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use fontconfig_sys as fc;
use fontconfig_sys::constants::*;
use memmap2::Mmap;
use swash::scale::{Render, ScaleContext, Source, StrikeWith, image::Content};
use swash::zeno::{Format, Transform};
use swash::{CacheKey, FontRef, NormalizedCoord};

/// 文字の書体 (太字と斜体の組み合わせ)。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    const ALL: [Style; 4] = [
        Style { bold: false, italic: false },
        Style { bold: true, italic: false },
        Style { bold: false, italic: true },
        Style { bold: true, italic: true },
    ];

    fn index(self) -> usize {
        self.bold as usize | (self.italic as usize) << 1
    }
}

struct Face {
    data: Arc<Mmap>,
    offset: u32,
    key: CacheKey,
    /// 太字の書体がないので、輪郭を太らせて作る。
    fake_bold: bool,
    /// 斜体の書体がないので、傾けて作る。
    fake_italic: bool,
    /// 絵文字のような色つきのグリフを持つ。
    color: bool,
    /// 可変フォントの名前つきインスタンス (太字など) の座標。
    coords: Vec<NormalizedCoord>,
}

impl Face {
    fn font(&self) -> FontRef<'_> {
        FontRef { data: &self.data, offset: self.offset, key: self.key }
    }
}

/// ラスタライズしたグリフ。
pub struct Glyph {
    pub width: u32,
    pub height: u32,
    /// 文字の原点 (マスの左端、ベースライン) からの位置。
    pub left: i32,
    pub top: i32,
    /// `color` なら RGBA (乗算済み)、そうでなければ 1 バイトの濃さ。
    pub data: Vec<u8>,
    pub color: bool,
}

/// マスの大きさと線の位置 (ピクセル)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub cell_width: u32,
    pub cell_height: u32,
    pub baseline: u32,
    pub underline_position: u32,
    pub stroke: u32,
    pub strikeout_position: u32,
}

/// fontconfig の設定。初期化 (15ms ほど) は別のスレッドで進め、要るときに待つ。
/// 窓を閉じるまで使うので、手放さない。
pub struct FontConfig {
    thread: Mutex<Option<JoinHandle<usize>>>,
    config: OnceLock<usize>,
}

impl FontConfig {
    pub fn start() -> Arc<FontConfig> {
        // SAFETY: fontconfig の初期化。失敗したら null が返る。
        let thread = std::thread::spawn(|| unsafe { fc::FcInitLoadConfigAndFonts() } as usize);
        Arc::new(FontConfig { thread: Mutex::new(Some(thread)), config: OnceLock::new() })
    }

    fn get(&self) -> Option<*mut fc::FcConfig> {
        let ptr = *self.config.get_or_init(|| {
            let thread = self.thread.lock().ok().and_then(|mut t| t.take());
            thread.and_then(|t| t.join().ok()).unwrap_or(0)
        });
        (ptr != 0).then_some(ptr as *mut fc::FcConfig)
    }
}

/// 前に fontconfig で探した結果。次の起動では fontconfig を待たずに最初のフレームを出せる。
/// フォントを足したり消したりすると fontconfig のキャッシュが変わるので、その時刻で古さを見分ける。
struct FontCache {
    path: PathBuf,
    stamp: String,
    entries: HashMap<String, Found>,
}

impl FontCache {
    fn stamp() -> String {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let dirs = [
            PathBuf::from("/var/cache/fontconfig"),
            home.join(".cache/fontconfig"),
            PathBuf::from("/etc/fonts/conf.d"),
            home.join(".config/fontconfig"),
            home.join(".local/share/fonts"),
        ];
        let newest = dirs
            .iter()
            .filter_map(|d| std::fs::metadata(d).ok()?.modified().ok())
            .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .max()
            .unwrap_or_default();
        format!("v1 {}", newest.as_nanos())
    }

    fn load() -> FontCache {
        let path = crate::theme::cache_dir().join("fonts.tsv");
        let stamp = Self::stamp();
        let mut entries = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut lines = text.lines();
            if lines.next() == Some(stamp.as_str()) {
                for line in lines {
                    let f: Vec<&str> = line.split('\t').collect();
                    if let [family, lang, style, path, index, weight, slant] = f[..] {
                        let parsed = (index.parse(), weight.parse(), slant.parse());
                        if let (Ok(index), Ok(weight), Ok(slant)) = parsed {
                            let found = Found { path: PathBuf::from(path), index, weight, slant };
                            entries.insert(format!("{family}\t{lang}\t{style}"), found);
                        }
                    }
                }
            }
        }
        FontCache { path, stamp, entries }
    }

    fn save(&self) {
        let mut text = format!("{}\n", self.stamp);
        for (key, f) in &self.entries {
            text.push_str(&format!(
                "{key}\t{}\t{}\t{}\t{}\n",
                f.path.display(),
                f.index,
                f.weight,
                f.slant
            ));
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.path, text);
    }
}

pub struct Fonts {
    fc: Arc<FontConfig>,
    family: String,
    lang: CString,
    size_px: f32,
    faces: Vec<Face>,
    /// 書体ごとの基本のフェイス (`faces` の番号)。
    primary: [usize; 4],
    /// ファイルごとの読み込み済みのフェイス。同じファイルを二重に開かない。
    loaded: HashMap<(PathBuf, u32, bool, bool), usize>,
    /// 文字と書体から、使うフェイスとグリフ。
    lookup: HashMap<(char, Style), (usize, u16)>,
    scaler: ScaleContext,
    pub metrics: Metrics,
}

/// fontconfig が見つけたフォント。
#[derive(Clone)]
struct Found {
    path: PathBuf,
    index: u32,
    weight: i32,
    slant: i32,
}

impl Fonts {
    /// `family` は fontconfig の名前 (例: "monospace"、"Ubuntu Sans Mono")。
    /// `size_px` は 1em のピクセル数。`lang` はフォールバックで優先する言語 (例: "ja")。
    pub fn new(
        family: &str,
        size_px: f32,
        lang: &str,
        fc: Arc<FontConfig>,
    ) -> Result<Fonts, String> {
        let mut fonts = Fonts {
            fc,
            family: family.to_string(),
            lang: CString::new(lang).unwrap_or_default(),
            size_px,
            faces: Vec::new(),
            primary: [0; 4],
            loaded: HashMap::new(),
            lookup: HashMap::new(),
            scaler: ScaleContext::new(),
            metrics: Metrics {
                cell_width: 1,
                cell_height: 1,
                baseline: 1,
                underline_position: 1,
                stroke: 1,
                strikeout_position: 1,
            },
        };
        let mut cache = FontCache::load();
        let mut missed = false;
        for style in Style::ALL {
            let key = format!("{family}\t{lang}\t{}", style.index());
            let cached = cache.entries.get(&key).filter(|f| f.path.exists()).cloned();
            let found = match cached {
                Some(found) => found,
                None => {
                    missed = true;
                    let found = fonts
                        .find(style, None)
                        .ok_or_else(|| format!("フォント \"{family}\" が見つかりません"))?;
                    cache.entries.insert(key, found.clone());
                    found
                }
            };
            let fake_bold = style.bold && found.weight < FC_WEIGHT_DEMIBOLD;
            let fake_italic = style.italic && found.slant == FC_SLANT_ROMAN;
            fonts.primary[style.index()] = fonts.load(found, fake_bold, fake_italic)?;
        }
        if missed {
            cache.save();
        }
        fonts.metrics = fonts.compute_metrics();
        Ok(fonts)
    }

    pub fn family(&self) -> &str {
        &self.family
    }

    /// 大きさだけを変える。どの文字にどのフォントを使うかは変わらないので、検索の結果は残す。
    pub fn set_size(&mut self, size_px: f32) {
        self.size_px = size_px;
        self.metrics = self.compute_metrics();
    }

    fn find(&self, style: Style, ch: Option<char>) -> Option<Found> {
        let config = self.fc.get()?;
        let family = CString::new(self.family.as_str()).ok()?;
        // SAFETY: fontconfig の API を、作ったパターンが生きているあいだだけ使う。
        unsafe {
            let pattern = fc::FcNameParse(family.as_ptr().cast());
            if pattern.is_null() {
                return None;
            }
            let weight = if style.bold { FC_WEIGHT_BOLD } else { FC_WEIGHT_REGULAR };
            let slant = if style.italic { FC_SLANT_ITALIC } else { FC_SLANT_ROMAN };
            fc::FcPatternAddInteger(pattern, FC_WEIGHT.as_ptr(), weight);
            fc::FcPatternAddInteger(pattern, FC_SLANT.as_ptr(), slant);
            fc::FcPatternAddDouble(pattern, FC_PIXEL_SIZE.as_ptr(), f64::from(self.size_px));
            if !self.lang.as_bytes().is_empty() {
                fc::FcPatternAddString(pattern, FC_LANG.as_ptr(), self.lang.as_ptr().cast());
            }
            let charset = ch.map(|ch| {
                let cs = fc::FcCharSetCreate();
                fc::FcCharSetAddChar(cs, ch as u32);
                fc::FcPatternAddCharSet(pattern, FC_CHARSET.as_ptr(), cs);
                cs
            });
            // 絵文字は、色つきのフォント (Noto Color Emoji) を優先する。
            if ch.is_some_and(is_emoji) {
                fc::FcPatternAddBool(pattern, FC_COLOR.as_ptr(), 1);
            }
            fc::FcConfigSubstitute(config, pattern, fc::FcMatchPattern);
            fc::FcDefaultSubstitute(pattern);
            let mut result = 0;
            let matched = fc::FcFontMatch(config, pattern, &mut result);
            fc::FcPatternDestroy(pattern);
            if let Some(cs) = charset {
                fc::FcCharSetDestroy(cs);
            }
            if matched.is_null() {
                return None;
            }
            let found = (|| {
                // フォールバックでは、選ばれたフォントが本当にその文字を持つかを確かめる。
                if let Some(ch) = ch {
                    let mut cs = std::ptr::null_mut();
                    if fc::FcPatternGetCharSet(matched, FC_CHARSET.as_ptr(), 0, &mut cs)
                        != fc::FcResultMatch
                        || fc::FcCharSetHasChar(cs, ch as u32) == 0
                    {
                        return None;
                    }
                }
                let mut file = std::ptr::null_mut();
                if fc::FcPatternGetString(matched, FC_FILE.as_ptr(), 0, &mut file)
                    != fc::FcResultMatch
                {
                    return None;
                }
                let path = PathBuf::from(CStr::from_ptr(file.cast()).to_str().ok()?);
                let mut index = 0;
                fc::FcPatternGetInteger(matched, FC_INDEX.as_ptr(), 0, &mut index);
                let mut weight = FC_WEIGHT_REGULAR;
                fc::FcPatternGetInteger(matched, FC_WEIGHT.as_ptr(), 0, &mut weight);
                let mut slant = FC_SLANT_ROMAN;
                fc::FcPatternGetInteger(matched, FC_SLANT.as_ptr(), 0, &mut slant);
                Some(Found { path, index: index as u32, weight, slant })
            })();
            fc::FcPatternDestroy(matched);
            found
        }
    }

    fn load(&mut self, found: Found, fake_bold: bool, fake_italic: bool) -> Result<usize, String> {
        // fontconfig の index は、下の 16 ビットがファイルの中のフォント、上が可変フォントのインスタンス。
        let key = (found.path.clone(), found.index, fake_bold, fake_italic);
        let face_index = (found.index & 0xffff) as usize;
        let instance = (found.index >> 16) as usize;
        if let Some(&i) = self.loaded.get(&key) {
            return Ok(i);
        }
        // 同じファイルを別の書体で開いていれば、mmap を使い回す。
        let data = match self.loaded.keys().find(|k| k.0 == found.path) {
            Some(k) => self.faces[self.loaded[k]].data.clone(),
            None => {
                let file = File::open(&found.path)
                    .map_err(|e| format!("{}: {e}", found.path.display()))?;
                // SAFETY: フォントのファイルを読み取り専用で写す。ほかから書き換えられることは想定しない。
                Arc::new(unsafe { Mmap::map(&file) }.map_err(|e| e.to_string())?)
            }
        };
        let font = FontRef::from_index(&data, face_index)
            .ok_or_else(|| format!("{} を読めません", found.path.display()))?;
        let (offset, key_) = (font.offset, font.key);
        let coords: Vec<NormalizedCoord> = match instance.checked_sub(1) {
            Some(i) => font
                .instances()
                .nth(i)
                .map(|inst| inst.normalized_coords().collect())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let scaler = self.scaler.builder(font).build();
        let color = scaler.has_color_bitmaps() || scaler.has_color_outlines();
        self.faces.push(Face { data, offset, key: key_, fake_bold, fake_italic, color, coords });
        let i = self.faces.len() - 1;
        self.loaded.insert(key, i);
        Ok(i)
    }

    fn compute_metrics(&mut self) -> Metrics {
        let face = &self.faces[self.primary[0]];
        let font = face.font();
        let m = font.metrics(&face.coords).scale(self.size_px);
        let glyphs = font.glyph_metrics(&face.coords).scale(self.size_px);
        let advance = glyphs.advance_width(font.charmap().map('M'));
        let cell_width = advance.round().max(1.0) as u32;
        let cell_height = (m.ascent + m.descent + m.leading).round().max(1.0) as u32;
        // 行の高さの余りを上下に分ける。
        let extra = cell_height as f32 - (m.ascent + m.descent);
        let baseline = (m.ascent + extra / 2.0).round().max(1.0) as u32;
        let stroke = m.stroke_size.round().max(1.0) as u32;
        let underline_position = (baseline as f32 - m.underline_offset)
            .round()
            .clamp(0.0, cell_height as f32 - 1.0) as u32;
        let strikeout =
            if m.strikeout_offset > 0.0 { m.strikeout_offset } else { m.x_height / 2.0 };
        let strikeout_position =
            (baseline as f32 - strikeout).round().clamp(0.0, cell_height as f32 - 1.0) as u32;
        Metrics {
            cell_width,
            cell_height,
            baseline,
            underline_position,
            stroke,
            strikeout_position,
        }
    }

    /// 文字を描くフェイスとグリフ。どのフォントも持たない文字は、基本のフェイスの .notdef。
    pub fn resolve(&mut self, ch: char, style: Style) -> (usize, u16) {
        if let Some(&hit) = self.lookup.get(&(ch, style)) {
            return hit;
        }
        let primary = self.primary[style.index()];
        let gid = self.faces[primary].font().charmap().map(ch);
        let hit = if gid != 0 {
            (primary, gid)
        } else {
            self.fallback(ch, style).unwrap_or((primary, 0))
        };
        self.lookup.insert((ch, style), hit);
        hit
    }

    fn fallback(&mut self, ch: char, style: Style) -> Option<(usize, u16)> {
        let found = self.find(style, Some(ch))?;
        let fake_bold = style.bold && found.weight < FC_WEIGHT_DEMIBOLD;
        let fake_italic = style.italic && found.slant == FC_SLANT_ROMAN;
        let face = self.load(found, fake_bold, fake_italic).ok()?;
        let gid = self.faces[face].font().charmap().map(ch);
        (gid != 0).then_some((face, gid))
    }

    /// グリフをラスタライズする。`cells` は文字の幅 (1 か 2)。色つきのグリフはその幅に収める。
    pub fn rasterize(&mut self, face: usize, glyph: u16, cells: u32) -> Option<Glyph> {
        let f = &self.faces[face];
        let font = FontRef { data: &f.data, offset: f.offset, key: f.key };
        let (fake_bold, fake_italic, color) = (f.fake_bold, f.fake_italic, f.color);
        let mut scaler = self
            .scaler
            .builder(font)
            .size(self.size_px)
            .hint(true)
            .normalized_coords(&f.coords)
            .build();
        let mut render = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ]);
        render.format(Format::Alpha);
        if fake_bold {
            render.embolden((self.size_px / 24.0).max(0.5));
        }
        if fake_italic {
            render.transform(Some(Transform::skew(
                swash::zeno::Angle::from_degrees(14.0),
                swash::zeno::Angle::ZERO,
            )));
        }
        let image = render.render(&mut scaler, glyph)?;
        let is_color = image.content == Content::Color;
        let mut out = Glyph {
            width: image.placement.width,
            height: image.placement.height,
            left: image.placement.left,
            top: image.placement.top,
            data: image.data,
            color: is_color,
        };
        if is_color || color && out.width > cells * self.metrics.cell_width {
            fit_color_glyph(&mut out, cells * self.metrics.cell_width, self.metrics);
        }
        Some(out)
    }
}

/// 絵文字として色つきで出したい文字 (おおまかな範囲)。
fn is_emoji(c: char) -> bool {
    matches!(c as u32, 0x1f000..=0x1faff | 0x2600..=0x27bf | 0x2b00..=0x2bff)
}

/// 絵文字の大きさをマスに合わせる。はみ出すときだけ縮め、縦は行の真ん中にそろえる。
fn fit_color_glyph(glyph: &mut Glyph, max_width: u32, m: Metrics) {
    let max_height = m.cell_height;
    if glyph.width == 0 || glyph.height == 0 {
        return;
    }
    let scale = (max_width as f32 / glyph.width as f32)
        .min(max_height as f32 / glyph.height as f32)
        .min(1.0);
    if scale < 1.0 {
        let w = ((glyph.width as f32 * scale).round() as u32).max(1);
        let h = ((glyph.height as f32 * scale).round() as u32).max(1);
        glyph.data = downscale_rgba(&glyph.data, glyph.width, glyph.height, w, h);
        glyph.width = w;
        glyph.height = h;
    }
    glyph.left = ((max_width - glyph.width.min(max_width)) / 2) as i32;
    let top_in_cell = (max_height.saturating_sub(glyph.height)) / 2;
    glyph.top = m.baseline as i32 - top_in_cell as i32;
}

/// 面積平均で縮める (RGBA)。
fn downscale_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        let y0 = y * sh / dh;
        let y1 = ((y + 1) * sh / dh).max(y0 + 1).min(sh);
        for x in 0..dw {
            let x0 = x * sw / dw;
            let x1 = ((x + 1) * sw / dw).max(x0 + 1).min(sw);
            let mut acc = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * sw + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += u32::from(src[i + c]);
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / n) as u8;
            }
        }
    }
    out
}

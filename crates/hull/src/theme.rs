//! GNOME への追従と配色。
//!
//! 明暗 (`color-scheme`) と等幅フォント (`monospace-font-name`) を gsettings で読み、
//! `gsettings monitor` で変更を見張る。D-Bus のライブラリを本体に入れないため、コマンドを使う。

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Command, Stdio};

use hull_term::Rgb;

use crate::config::{Config, SchemeConfig, parse_rgb};

const SCHEMA: &str = "org.gnome.desktop.interface";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Desktop {
    pub dark: Option<bool>,
    /// GNOME の等幅フォント (名前, ポイント)。
    pub monospace: Option<(String, f32)>,
}

/// 起動を速くするための覚え書きを置く場所 (`~/.cache/hull`)。
pub fn cache_dir() -> std::path::PathBuf {
    crate::config::xdg_dir("XDG_CACHE_HOME", ".cache").join("hull")
}

impl Desktop {
    /// 前の起動で読んだ GNOME の設定。起動のときに gsettings を待たないために使い、あとで確かめる。
    pub fn load_cached() -> Option<Desktop> {
        let text = std::fs::read_to_string(cache_dir().join("desktop.tsv")).ok()?;
        let mut d = Desktop::default();
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f[..] {
                ["dark", v] => d.dark = Some(v == "1"),
                ["monospace", name, size] => {
                    d.monospace = Some((name.to_string(), size.parse().ok()?))
                }
                _ => {}
            }
        }
        Some(d)
    }

    pub fn save_cached(&self) {
        let mut text = String::new();
        if let Some(dark) = self.dark {
            text.push_str(&format!("dark\t{}\n", if dark { 1 } else { 0 }));
        }
        if let Some((name, size)) = &self.monospace {
            text.push_str(&format!("monospace\t{name}\t{size}\n"));
        }
        let dir = cache_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("desktop.tsv"), text);
    }
}

/// gsettings の値 (`'prefer-dark'` のように引用符つき) をほどく。
fn unquote(s: &str) -> &str {
    s.trim().trim_matches('\'')
}

fn parse_scheme(value: &str) -> Option<bool> {
    match unquote(value) {
        "prefer-dark" => Some(true),
        "default" | "prefer-light" => Some(false),
        _ => None,
    }
}

/// "Ubuntu Sans Mono 13" を名前と大きさに分ける。
fn parse_font(value: &str) -> Option<(String, f32)> {
    let s = unquote(value);
    let (name, size) = s.rsplit_once(' ')?;
    let size: f32 = size.parse().ok()?;
    (size > 0.0 && !name.is_empty()).then(|| (name.to_string(), size))
}

/// GNOME の設定を読む。2 つのコマンドを同時に走らせ、起動を待つ時間を短くする。
pub fn query_desktop() -> Desktop {
    let spawn = |key: &str| {
        Command::new("gsettings")
            .args(["get", SCHEMA, key])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    };
    let scheme = spawn("color-scheme");
    let font = spawn("monospace-font-name");
    let read = |child: Option<Child>| -> Option<String> {
        let out = child?.wait_with_output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    Desktop {
        dark: read(scheme).as_deref().and_then(parse_scheme),
        monospace: read(font).as_deref().and_then(parse_font),
    }
}

/// 変更を見張るコマンドを起動する。
pub fn monitor() -> Option<(Child, BufReader<ChildStdout>)> {
    let mut child = Command::new("gsettings")
        .args(["monitor", SCHEMA])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    Some((child, BufReader::new(stdout)))
}

/// `gsettings monitor` の 1 行 ("color-scheme: 'prefer-dark'") を読み、`desktop` に反映する。
/// 変わったら `true`。
pub fn apply_monitor_line(desktop: &mut Desktop, line: &str) -> bool {
    let Some((key, value)) = line.split_once(':') else { return false };
    let before = desktop.clone();
    match key.trim() {
        "color-scheme" => desktop.dark = parse_scheme(value),
        "monospace-font-name" => desktop.monospace = parse_font(value),
        _ => {}
    }
    *desktop != before
}

/// 見張りのコマンドから読めるだけ行を読む。
pub fn read_lines(reader: &mut BufReader<ChildStdout>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    // パイプはノンブロッキングにしてあるので、読めなくなったら止まる。
    while let Ok(n) = reader.read_line(&mut line) {
        if n == 0 {
            break;
        }
        lines.push(std::mem::take(&mut line));
    }
    lines
}

/// 画面の配色。
#[derive(Clone, Debug, PartialEq)]
pub struct Scheme {
    pub dark: bool,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub palette: [Rgb; 16],
    /// 見出しの帯の色 (前にあるとき、後ろにあるとき) と文字の色。
    pub header: Rgb,
    pub header_backdrop: Rgb,
    pub header_text: Rgb,
    pub header_text_backdrop: Rgb,
    pub button_hover: Rgb,
}

const PALETTE: [Rgb; 16] = [
    [0x17, 0x14, 0x21],
    [0xc0, 0x1c, 0x28],
    [0x26, 0xa2, 0x69],
    [0xa2, 0x73, 0x4c],
    [0x12, 0x48, 0x8b],
    [0xa3, 0x47, 0xba],
    [0x2a, 0xa1, 0xb3],
    [0xd0, 0xcf, 0xcc],
    [0x5e, 0x5c, 0x64],
    [0xf6, 0x61, 0x51],
    [0x33, 0xd1, 0x7a],
    [0xe9, 0xad, 0x0c],
    [0x2a, 0x7b, 0xde],
    [0xc0, 0x61, 0xcb],
    [0x33, 0xc7, 0xde],
    [0xff, 0xff, 0xff],
];

impl Scheme {
    /// libadwaita の色に合わせた既定の配色。
    fn builtin(dark: bool) -> Scheme {
        if dark {
            Scheme {
                dark,
                foreground: [0xff, 0xff, 0xff],
                background: [0x1e, 0x1e, 0x1e],
                cursor: [0xff, 0xff, 0xff],
                selection: [0x2d, 0x4f, 0x7c],
                palette: PALETTE,
                header: [0x30, 0x30, 0x30],
                header_backdrop: [0x24, 0x24, 0x24],
                header_text: [0xff, 0xff, 0xff],
                header_text_backdrop: [0x91, 0x91, 0x91],
                button_hover: [0x45, 0x45, 0x45],
            }
        } else {
            let mut palette = PALETTE;
            // 白地では白の文字が見えないので、白を灰色に寄せる。
            palette[7] = [0x9a, 0x99, 0x96];
            palette[15] = [0x5e, 0x5c, 0x64];
            Scheme {
                dark,
                foreground: [0x1e, 0x1e, 0x1e],
                background: [0xff, 0xff, 0xff],
                cursor: [0x1e, 0x1e, 0x1e],
                selection: [0xb4, 0xd5, 0xfe],
                palette,
                header: [0xeb, 0xeb, 0xeb],
                header_backdrop: [0xfa, 0xfa, 0xfa],
                header_text: [0x2e, 0x2e, 0x2e],
                header_text_backdrop: [0x92, 0x92, 0x92],
                button_hover: [0xd6, 0xd6, 0xd6],
            }
        }
    }

    fn apply(&mut self, c: &SchemeConfig) {
        let set = |slot: &mut Rgb, v: &Option<String>| {
            if let Some(rgb) = v.as_deref().and_then(parse_rgb) {
                *slot = rgb;
            }
        };
        set(&mut self.foreground, &c.foreground);
        set(&mut self.background, &c.background);
        set(&mut self.cursor, &c.cursor);
        set(&mut self.selection, &c.selection);
        for (slot, s) in self.palette.iter_mut().zip(&c.palette) {
            if let Some(rgb) = parse_rgb(s) {
                *slot = rgb;
            }
        }
    }

    pub fn resolve(config: &Config, desktop: &Desktop) -> Scheme {
        use crate::config::ThemeChoice::*;
        let dark = match config.window.theme {
            Dark => true,
            Light => false,
            Auto => desktop.dark.unwrap_or(false),
        };
        let mut scheme = Scheme::builtin(dark);
        scheme.apply(if dark { &config.colors.dark } else { &config.colors.light });
        scheme
    }
}

/// フォントの名前と大きさ (ポイント)。設定、GNOME、既定値の順に決める。
pub fn font_choice(config: &Config, desktop: &Desktop) -> (String, f32) {
    let gnome = desktop.monospace.clone();
    let family = config
        .font
        .family
        .clone()
        .or_else(|| gnome.as_ref().map(|g| g.0.clone()))
        .unwrap_or_else(|| "monospace".into());
    let size = config.font.size.or(gnome.map(|g| g.1)).unwrap_or(11.0).clamp(4.0, 200.0);
    (family, size)
}

/// フォールバックで優先する言語 ("ja_JP.UTF-8" なら "ja")。
pub fn font_lang(config: &Config) -> String {
    if let Some(lang) = &config.font.lang {
        return lang.clone();
    }
    for var in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            let code: String = v.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
            if !code.is_empty() && code != "C" {
                return code;
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gsettings_の値を読む() {
        assert_eq!(parse_scheme("'prefer-dark'\n"), Some(true));
        assert_eq!(parse_scheme("'default'"), Some(false));
        assert_eq!(parse_font("'Ubuntu Sans Mono 13'\n"), Some(("Ubuntu Sans Mono".into(), 13.0)));
        assert_eq!(parse_font("'Mono'"), None);
    }

    #[test]
    fn 見張りの行を反映する() {
        let mut d = Desktop::default();
        assert!(apply_monitor_line(&mut d, "color-scheme: 'prefer-dark'\n"));
        assert_eq!(d.dark, Some(true));
        assert!(!apply_monitor_line(&mut d, "color-scheme: 'prefer-dark'\n"));
        assert!(!apply_monitor_line(&mut d, "clock-format: '24h'\n"));
    }

    #[test]
    fn 設定が_gnome_より優先する() {
        let desktop =
            Desktop { dark: Some(true), monospace: Some(("Ubuntu Sans Mono".into(), 13.0)) };
        let config = Config::parse("[font]\nsize = 9\n[window]\ntheme = \"light\"\n").unwrap();
        assert_eq!(font_choice(&config, &desktop), ("Ubuntu Sans Mono".into(), 9.0));
        assert!(!Scheme::resolve(&config, &desktop).dark);
        assert!(Scheme::resolve(&Config::default(), &desktop).dark);
    }
}

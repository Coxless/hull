//! 設定。`~/.config/hull/hull.toml` のファイル 1 つで、保存するとそのまま反映する。
//!
//! 書いていない項目は既定値になる。フォントと明暗は、書かなければ GNOME の設定に従う。

use std::path::PathBuf;

use serde::Deserialize;

use hull_term::Rgb;
use hull_term::input::Mods;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub scrollback: ScrollbackConfig,
    pub shell: ShellConfig,
    pub colors: ColorsConfig,
    pub bell: BellConfig,
    #[serde(rename = "keybind")]
    pub keybinds: Vec<KeybindConfig>,
    #[serde(rename = "plugin")]
    pub plugins: Vec<PluginConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font: FontConfig::default(),
            window: WindowConfig::default(),
            scrollback: ScrollbackConfig::default(),
            shell: ShellConfig::default(),
            colors: ColorsConfig::default(),
            bell: BellConfig::default(),
            keybinds: Vec::new(),
            plugins: default_plugins(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct FontConfig {
    /// fontconfig の名前。省略すると GNOME の等幅フォント (なければ "monospace")。
    pub family: Option<String>,
    /// ポイント。省略すると GNOME の等幅フォントの大きさ (なければ 11)。
    pub size: Option<f32>,
    /// フォールバックで優先する言語。省略すると $LANG から決める (ja_JP なら "ja")。
    pub lang: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    /// GNOME の明暗の設定に従う。
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WindowConfig {
    /// 文字のまわりの余白 (論理ピクセル)。
    pub padding: u32,
    /// 起動時の大きさ (列と行)。
    pub columns: u16,
    pub lines: u16,
    pub theme: ThemeChoice,
    /// GNOME のようにサーバーが枠を描かない環境で、hull が見出しの帯を描く。
    pub decorations: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig {
            padding: 4,
            columns: 100,
            lines: 30,
            theme: ThemeChoice::Auto,
            decorations: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ScrollbackConfig {
    pub lines: usize,
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        // alacritty の既定値に合わせる (メモリの比較の条件をそろえるため)。
        ScrollbackConfig { lines: 10_000 }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ShellConfig {
    /// 省略するとログインシェル。
    pub program: Option<String>,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ColorsConfig {
    pub dark: SchemeConfig,
    pub light: SchemeConfig,
}

/// 明るい・暗いそれぞれの色。書いた色だけが既定値を上書きする。
#[derive(Clone, Debug, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SchemeConfig {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor: Option<String>,
    pub selection: Option<String>,
    /// 0〜15 の色。
    pub palette: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct BellConfig {
    /// 窓が前にないときにベルが鳴ったら、窓に注意を向けさせる。
    pub attention: bool,
}

impl Default for BellConfig {
    fn default() -> Self {
        BellConfig { attention: true }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeybindConfig {
    /// 例: "ctrl+shift+c"。
    pub keys: String,
    /// 本体のアクションの名前か、"plugin:<プラグイン>/<名前>"。"none" で既定の割り当てを消す。
    pub action: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// 同梱のプラグインを置く場所。
pub const PLUGIN_DIR: &str = "/usr/lib/hull/plugins";

/// 設定ファイルがないときに使うプラグイン (同梱の 3 つ)。
/// 設定ファイルに `[[plugin]]` を 1 つでも書くと、書いたものだけになる。
fn default_plugins() -> Vec<PluginConfig> {
    ["search", "url-hints", "claude-status"]
        .iter()
        .map(|name| PluginConfig {
            name: name.to_string(),
            command: format!("{PLUGIN_DIR}/hull-{name}"),
            args: Vec::new(),
        })
        .filter(|p| std::path::Path::new(&p.command).exists())
        .collect()
}

pub fn config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("hull").join("hull.toml")
}

/// XDG のディレクトリ (`var` が絶対パスでなければ `$HOME/fallback`)。
pub fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback)
    })
}

impl Config {
    /// 設定を読む。ファイルがなければ既定値。読めなければ、理由と既定値を返す。
    pub fn load(path: &std::path::Path) -> (Config, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match Config::parse(&text) {
                Ok(config) => (config, None),
                Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
            Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
        }
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        for bind in &config.keybinds {
            KeyCombo::parse(&bind.keys)?;
            Action::parse(&bind.action)?;
        }
        Ok(config)
    }
}

/// 色の文字列 ("#rrggbb")。
pub fn parse_rgb(s: &str) -> Option<Rgb> {
    hull_term::term::parse_color(s.as_bytes())
}

// ---- キーバインド ----

/// 割り当てられる操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Copy,
    Paste,
    PastePrimary,
    ScrollPageUp,
    ScrollPageDown,
    ScrollLineUp,
    ScrollLineDown,
    ScrollTop,
    ScrollBottom,
    FontBigger,
    FontSmaller,
    FontReset,
    /// 前のプロンプトへ (OSC 133 の印を使う)。
    PromptPrevious,
    PromptNext,
    /// プラグインにキーバインドの発火を知らせる。
    Plugin {
        plugin: String,
        name: String,
    },
    /// 既定の割り当てを消す。
    None,
}

impl Action {
    pub fn parse(s: &str) -> Result<Action, String> {
        Ok(match s {
            "copy" => Action::Copy,
            "paste" => Action::Paste,
            "paste-primary" => Action::PastePrimary,
            "scroll-page-up" => Action::ScrollPageUp,
            "scroll-page-down" => Action::ScrollPageDown,
            "scroll-line-up" => Action::ScrollLineUp,
            "scroll-line-down" => Action::ScrollLineDown,
            "scroll-top" => Action::ScrollTop,
            "scroll-bottom" => Action::ScrollBottom,
            "font-bigger" => Action::FontBigger,
            "font-smaller" => Action::FontSmaller,
            "font-reset" => Action::FontReset,
            "prompt-previous" => Action::PromptPrevious,
            "prompt-next" => Action::PromptNext,
            "none" => Action::None,
            s => {
                let rest =
                    s.strip_prefix("plugin:").ok_or_else(|| format!("知らないアクション: {s}"))?;
                let (plugin, name) = rest.split_once('/').unwrap_or((rest, rest));
                if plugin.is_empty() || name.is_empty() {
                    return Err(format!("プラグインの名前がありません: {s}"));
                }
                Action::Plugin { plugin: plugin.into(), name: name.into() }
            }
        })
    }
}

/// キーの組み合わせ。文字キーは小文字で持つ。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyCombo {
    pub mods: Mods,
    pub key: String,
}

impl KeyCombo {
    pub fn parse(s: &str) -> Result<KeyCombo, String> {
        let mut mods = Mods::default();
        let mut key = None;
        // "ctrl++" は "ctrl+plus" と同じ。
        let s = match s.strip_suffix("++") {
            Some(head) => format!("{head}+plus"),
            None => s.to_string(),
        };
        let s = s.as_str();
        for part in s.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "shift" => mods.shift = true,
                "alt" => mods.alt = true,
                "super" | "logo" => mods.logo = true,
                "" => return Err(format!("キーの書き方が正しくありません: {s}")),
                k => {
                    if key.is_some() {
                        return Err(format!("キーが 2 つあります: {s}"));
                    }
                    key = Some(k.to_string());
                }
            }
        }
        let key = key.ok_or_else(|| format!("キーがありません: {s}"))?;
        Ok(KeyCombo { mods, key })
    }
}

/// 既定のキーバインド。設定ファイルの `[[keybind]]` が同じキーを上書きする。
pub fn default_keybinds() -> Vec<(KeyCombo, Action)> {
    let binds = [
        ("ctrl+shift+c", "copy"),
        ("ctrl+shift+v", "paste"),
        ("shift+insert", "paste-primary"),
        ("shift+pageup", "scroll-page-up"),
        ("shift+pagedown", "scroll-page-down"),
        ("ctrl+shift+up", "scroll-line-up"),
        ("ctrl+shift+down", "scroll-line-down"),
        ("shift+home", "scroll-top"),
        ("shift+end", "scroll-bottom"),
        ("ctrl+plus", "font-bigger"),
        ("ctrl+equal", "font-bigger"),
        ("ctrl+shift+plus", "font-bigger"),
        ("ctrl+minus", "font-smaller"),
        ("ctrl+0", "font-reset"),
        ("ctrl+shift+z", "prompt-previous"),
        ("ctrl+shift+x", "prompt-next"),
        ("ctrl+shift+f", "plugin:search/search"),
        ("ctrl+shift+u", "plugin:url-hints/open"),
        ("ctrl+shift+y", "plugin:url-hints/copy"),
    ];
    binds
        .iter()
        .map(|(k, a)| (KeyCombo::parse(k).expect("既定値"), Action::parse(a).expect("既定値")))
        .collect()
}

/// 既定と設定を合わせたキーバインド。
pub fn keybinds(config: &Config) -> Vec<(KeyCombo, Action)> {
    let mut binds = default_keybinds();
    for bind in &config.keybinds {
        let (Ok(combo), Ok(action)) = (KeyCombo::parse(&bind.keys), Action::parse(&bind.action))
        else {
            continue;
        };
        binds.retain(|(k, _)| *k != combo);
        if action != Action::None {
            binds.push((combo, action));
        }
    }
    binds
}

/// `hull --print-config` で出す、既定値を書いた設定ファイル。
pub const EXAMPLE: &str = include_str!("../hull.toml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 例の設定ファイルを読める() {
        let config = Config::parse(EXAMPLE).unwrap();
        assert_eq!(config.scrollback.lines, 10_000);
        assert_eq!(config.plugins.len(), 3);
    }

    #[test]
    fn 書いていない項目は既定値() {
        let config = Config::parse("[font]\nsize = 13\n").unwrap();
        assert_eq!(config.font.size, Some(13.0));
        assert_eq!(config.window.padding, 4);
    }

    #[test]
    fn 知らない項目はエラー() {
        assert!(Config::parse("[font]\nsise = 13\n").is_err());
        assert!(Config::parse("[[keybind]]\nkeys = \"ctrl+q\"\naction = \"quit\"\n").is_err());
    }

    #[test]
    fn キーバインドの上書きと削除() {
        let config = Config::parse(
            "[[keybind]]\nkeys = \"ctrl+shift+c\"\naction = \"none\"\n\
             [[keybind]]\nkeys = \"ctrl+shift+g\"\naction = \"plugin:search\"\n",
        )
        .unwrap();
        let binds = keybinds(&config);
        let copy = KeyCombo::parse("ctrl+shift+c").unwrap();
        assert!(!binds.iter().any(|(k, _)| *k == copy));
        let g = KeyCombo::parse("ctrl+shift+g").unwrap();
        let action = &binds.iter().find(|(k, _)| *k == g).unwrap().1;
        assert_eq!(*action, Action::Plugin { plugin: "search".into(), name: "search".into() });
    }

    #[test]
    fn キーの組み合わせ() {
        let combo = KeyCombo::parse("Ctrl+Shift+F").unwrap();
        assert!(combo.mods.ctrl && combo.mods.shift && !combo.mods.alt);
        assert_eq!(combo.key, "f");
        assert_eq!(KeyCombo::parse("ctrl++").unwrap().key, "plus");
        assert!(KeyCombo::parse("ctrl+a+b").is_err());
    }
}

//! Cấu hình người dùng đọc từ `~/.config/termul/config.toml` (theo OS).
//! Thiếu file hoặc lỗi parse → dùng mặc định.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Color;
use serde::Deserialize;

use crate::app::{SUGGEST_FETCH, SUGGEST_MAX_VISIBLE, TAB_MIN_W};

pub(crate) const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 10_000_000;

/// Một tổ hợp phím đã phân giải: (modifier, phím).
pub(crate) type KeyBind = (KeyModifiers, KeyCode);

/// Bộ phím tắt (đã phân giải sẵn để so khớp nhanh mỗi keystroke).
#[derive(Clone, Copy)]
pub(crate) struct Keymap {
    pub(crate) prefix: KeyBind,       // vào prefix mode (mặc định Ctrl+`)
    pub(crate) quit: KeyBind,         // thoát nhanh (mặc định Ctrl+Q)
    pub(crate) split_right: char,     // '%'
    pub(crate) split_down: char,      // '"'
    pub(crate) close_pane: char,      // 'x'
    pub(crate) tab_new: char,         // 'c'
    pub(crate) tab_next: char,        // 'n'
    pub(crate) tab_prev: char,        // 'p'
    pub(crate) tab_rename: char,      // ','
    pub(crate) tab_close: char,       // '&'
    pub(crate) palette: char,         // 'r'
    pub(crate) quit_action: char,     // 'q' (trong prefix mode)
}

/// Cấu hình đã phân giải (dùng trực tiếp trong App).
pub(crate) struct Config {
    pub(crate) bg: Color,     // nền popup/tabbar + tab active
    pub(crate) accent: Color, // viền + highlight chọn
    pub(crate) green: Color,  // dòng cùng cwd trong history
    pub(crate) blue: Color,   // viền pane đang focus
    pub(crate) red: Color,    // popup xác nhận (hành động phá huỷ)
    pub(crate) yellow: Color, // chỉ báo cuộn ▲▼ / cảnh báo
    pub(crate) tab_min_width: u16,
    pub(crate) suggest_max_visible: usize,
    pub(crate) suggest_fetch: usize,
    pub(crate) mention_exclude: Vec<String>,
    pub(crate) scrollback_limit_bytes: usize,
    pub(crate) keys: Keymap,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bg: Color::Rgb(40, 42, 54),        // #282a36
            accent: Color::Rgb(203, 166, 247), // #cba6f7 mauve
            green: Color::Rgb(166, 227, 161),  // #a6e3a1
            blue: Color::Rgb(137, 180, 250),   // #89b4fa
            red: Color::Rgb(243, 139, 168),    // #f38ba8
            yellow: Color::Rgb(249, 226, 175), // #f9e2af
            tab_min_width: TAB_MIN_W,
            suggest_max_visible: SUGGEST_MAX_VISIBLE,
            suggest_fetch: SUGGEST_FETCH,
            mention_exclude: Vec::new(),
            scrollback_limit_bytes: DEFAULT_SCROLLBACK_LIMIT_BYTES,
            keys: Keymap {
                prefix: (KeyModifiers::CONTROL, KeyCode::Char('`')),
                quit: (KeyModifiers::CONTROL, KeyCode::Char('q')),
                split_right: 'd',
                split_down: 's',
                close_pane: 'x',
                tab_new: 'c',
                tab_next: 'n',
                tab_prev: 'p',
                tab_rename: ',',
                tab_close: 'w',
                palette: 'r',
                quit_action: 'q',
            },
        }
    }
}

impl Config {
    /// Nạp config từ file; mọi lỗi (không có file, parse hỏng) → mặc định.
    pub(crate) fn load() -> Config {
        let Some(path) = config_path() else {
            return Config::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Config::default();
        };
        match toml::from_str::<FileConfig>(&text) {
            Ok(fc) => fc.resolve(),
            Err(_) => Config::default(),
        }
    }
}

/// Đường dẫn file config: `<config_dir>/termul/config.toml`.
fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("termul").join("config.toml"))
}

// ── Sơ đồ file TOML (mọi trường tuỳ chọn) ───────────────────────────

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileConfig {
    appearance: FileAppearance,
    behavior: FileBehavior,
    mention: FileMention,
    advanced: FileAdvanced,
    keys: FileKeys,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileAppearance {
    bg: Option<String>,
    accent: Option<String>,
    green: Option<String>,
    blue: Option<String>,
    red: Option<String>,
    yellow: Option<String>,
    tab_min_width: Option<u16>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileBehavior {
    suggest_max_visible: Option<usize>,
    suggest_fetch: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileMention {
    exclude: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileAdvanced {
    scrollback_limit_bytes: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileKeys {
    prefix: Option<String>,
    quit: Option<String>,
    split_right: Option<String>,
    split_down: Option<String>,
    close_pane: Option<String>,
    tab_new: Option<String>,
    tab_next: Option<String>,
    tab_prev: Option<String>,
    tab_rename: Option<String>,
    tab_close: Option<String>,
    palette: Option<String>,
    quit_action: Option<String>,
}

impl FileConfig {
    /// Trộn giá trị file lên trên mặc định (giá trị lỗi → giữ mặc định).
    fn resolve(self) -> Config {
        let d = Config::default();
        let a = self.appearance;
        let b = self.behavior;
        let m = self.mention;
        let advanced = self.advanced;
        let k = self.keys;
        Config {
            bg: a.bg.and_then(|s| parse_color(&s)).unwrap_or(d.bg),
            accent: a.accent.and_then(|s| parse_color(&s)).unwrap_or(d.accent),
            green: a.green.and_then(|s| parse_color(&s)).unwrap_or(d.green),
            blue: a.blue.and_then(|s| parse_color(&s)).unwrap_or(d.blue),
            red: a.red.and_then(|s| parse_color(&s)).unwrap_or(d.red),
            yellow: a.yellow.and_then(|s| parse_color(&s)).unwrap_or(d.yellow),
            tab_min_width: a.tab_min_width.unwrap_or(d.tab_min_width),
            suggest_max_visible: b.suggest_max_visible.unwrap_or(d.suggest_max_visible),
            suggest_fetch: b.suggest_fetch.unwrap_or(d.suggest_fetch),
            mention_exclude: m.exclude,
            scrollback_limit_bytes: advanced
                .scrollback_limit_bytes
                .unwrap_or(d.scrollback_limit_bytes),
            keys: Keymap {
                prefix: k.prefix.and_then(|s| parse_bind(&s)).unwrap_or(d.keys.prefix),
                quit: k.quit.and_then(|s| parse_bind(&s)).unwrap_or(d.keys.quit),
                split_right: first_char(k.split_right).unwrap_or(d.keys.split_right),
                split_down: first_char(k.split_down).unwrap_or(d.keys.split_down),
                close_pane: first_char(k.close_pane).unwrap_or(d.keys.close_pane),
                tab_new: first_char(k.tab_new).unwrap_or(d.keys.tab_new),
                tab_next: first_char(k.tab_next).unwrap_or(d.keys.tab_next),
                tab_prev: first_char(k.tab_prev).unwrap_or(d.keys.tab_prev),
                tab_rename: first_char(k.tab_rename).unwrap_or(d.keys.tab_rename),
                tab_close: first_char(k.tab_close).unwrap_or(d.keys.tab_close),
                palette: first_char(k.palette).unwrap_or(d.keys.palette),
                quit_action: first_char(k.quit_action).unwrap_or(d.keys.quit_action),
            },
        }
    }
}

/// Ký tự đầu tiên của chuỗi (cho các phím lệnh 1 ký tự trong prefix).
fn first_char(s: Option<String>) -> Option<char> {
    s.and_then(|s| s.chars().next())
}

/// Parse màu hex `#rrggbb` (hoặc `rrggbb`) thành `Color::Rgb`.
fn parse_color(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Parse tổ hợp phím như `ctrl+\``, `alt+x`, hay một ký tự đơn `q`.
fn parse_bind(s: &str) -> Option<KeyBind> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    let (mod_parts, key_part) = parts.split_at(parts.len() - 1);
    let mut mods = KeyModifiers::NONE;
    for m in mod_parts {
        match m.to_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" | "option" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    let code = parse_code(key_part[0])?;
    Some((mods, code))
}

fn parse_code(s: &str) -> Option<KeyCode> {
    let mut chars = s.chars();
    let (first, second) = (chars.next()?, chars.next());
    if second.is_none() {
        return Some(KeyCode::Char(first));
    }
    match s.to_lowercase().as_str() {
        "enter" => Some(KeyCode::Enter),
        "tab" => Some(KeyCode::Tab),
        "esc" | "escape" => Some(KeyCode::Esc),
        "space" => Some(KeyCode::Char(' ')),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        _ => None,
    }
}

/// So khớp một sự kiện phím với một `KeyBind`.
pub(crate) fn key_matches(code: KeyCode, mods: KeyModifiers, bind: KeyBind) -> bool {
    code == bind.1 && mods == bind.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_color_hex() {
        assert_eq!(parse_color("#cba6f7"), Some(Color::Rgb(203, 166, 247)));
        assert_eq!(parse_color("00d7ff"), Some(Color::Rgb(0, 215, 255)));
        assert_eq!(parse_color("#xyz"), None);
    }

    #[test]
    fn parses_keybind() {
        assert_eq!(
            parse_bind("ctrl+`"),
            Some((KeyModifiers::CONTROL, KeyCode::Char('`')))
        );
        assert_eq!(parse_bind("q"), Some((KeyModifiers::NONE, KeyCode::Char('q'))));
        assert_eq!(
            parse_bind("alt+enter"),
            Some((KeyModifiers::ALT, KeyCode::Enter))
        );
        assert_eq!(parse_bind("bogus+b"), None);
    }

    #[test]
    fn default_prefix_is_ctrl_backtick() {
        assert_eq!(
            Config::default().keys.prefix,
            (KeyModifiers::CONTROL, KeyCode::Char('`'))
        );
    }

    #[test]
    fn file_overrides_merge_onto_defaults() {
        let toml = r##"
            [appearance]
            accent = "#ff0000"
            green = "#00ff00"
            [behavior]
            suggest_max_visible = 5
            [mention]
            exclude = ["target/", "*.log"]
            [advanced]
            scrollback_limit_bytes = 20000000
            [keys]
            prefix = "ctrl+a"
        "##;
        let fc: FileConfig = toml::from_str(toml).unwrap();
        let cfg = fc.resolve();
        assert_eq!(cfg.accent, Color::Rgb(255, 0, 0));
        assert_eq!(cfg.green, Color::Rgb(0, 255, 0));
        assert_eq!(cfg.suggest_max_visible, 5);
        assert_eq!(cfg.mention_exclude, ["target/", "*.log"]);
        assert_eq!(cfg.scrollback_limit_bytes, 20_000_000);
        assert_eq!(cfg.keys.prefix, (KeyModifiers::CONTROL, KeyCode::Char('a')));
        // trường không khai báo → giữ mặc định
        assert_eq!(cfg.blue, Color::Rgb(137, 180, 250));
        assert_eq!(cfg.bg, Color::Rgb(40, 42, 54));
        assert_eq!(cfg.suggest_fetch, SUGGEST_FETCH);
        assert_eq!(cfg.keys.close_pane, 'x');
    }

    #[test]
    fn scrollback_limit_defaults_to_ten_megabytes() {
        assert_eq!(
            Config::default().scrollback_limit_bytes,
            DEFAULT_SCROLLBACK_LIMIT_BYTES
        );
    }
}

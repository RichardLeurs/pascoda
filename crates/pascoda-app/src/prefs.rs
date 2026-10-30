//! Editor preferences: the font size and how many spaces the Tab key inserts. Kept in
//! `~/Library/Application Support/Pascoda/preferences` as `key=value` lines. The current values are
//! also kept in statics, so the editor's layout reads them without a handle to the app.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering};

/// The line height the editor had before the font size was a preference, at the default size.
const BASE_LINE_H: f32 = 18.0;
const BASE_FONT: f32 = 13.0;
pub const FONT_SIZES: std::ops::RangeInclusive<f32> = 10.0..=20.0;
pub const TAB_WIDTHS: [u8; 3] = [2, 4, 8];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prefs {
    pub font_size: f32,
    pub tab_width: u8,
    pub line_numbers: bool,
    /// Long lines wrap onto more rows instead of scrolling sideways.
    pub word_wrap: bool,
    /// Index into `theme::EDITOR_FONTS`.
    pub font: usize,
    /// The editor background per appearance (`[dark]`); `None` keeps the palette's.
    pub background: [Option<[u8; 3]>; 2],
    /// Colour overrides for the code tokens: `[dark][token]`, in the order of `highlight::TOKENS`.
    /// `None` keeps the palette's colour.
    pub colors: [[Option<[u8; 3]>; TOKEN_COUNT]; 2],
}

/// The number of code token colours a preference can override.
pub const TOKEN_COUNT: usize = 8;

impl Default for Prefs {
    fn default() -> Self {
        Self { font_size: BASE_FONT, tab_width: 4, line_numbers: true, word_wrap: false, font: 0, background: [None; 2], colors: [[None; TOKEN_COUNT]; 2] }
    }
}

impl Prefs {
    /// The values with the size rounded to a whole point and kept in range, and unlisted tab widths
    /// put right.
    pub fn checked(self) -> Self {
        Self {
            font_size: self.font_size.round().clamp(*FONT_SIZES.start(), *FONT_SIZES.end()),
            tab_width: if TAB_WIDTHS.contains(&self.tab_width) { self.tab_width } else { Prefs::default().tab_width },
            line_numbers: self.line_numbers,
            word_wrap: self.word_wrap,
            font: self.font.min(crate::theme::EDITOR_FONTS.len() - 1),
            background: self.background,
            colors: self.colors,
        }
    }

    /// The editor line height for this font size: 18 pt at the default size, in proportion above
    /// and below it.
    pub fn line_height(&self) -> f32 {
        (self.font_size * BASE_LINE_H / BASE_FONT).round()
    }

    /// The `key=value` text of the preferences file.
    pub fn to_text(&self) -> String {
        let mut text = format!(
            "font_size={}\ntab_width={}\nline_numbers={}\nword_wrap={}\n",
            self.font_size, self.tab_width, self.line_numbers, self.word_wrap
        );
        text.push_str(&format!("font={}\n", crate::theme::EDITOR_FONTS[self.font].0));
        for (dark, bg) in self.background.iter().enumerate() {
            if let Some([r, g, b]) = bg {
                let mode = if dark == 1 { "dark" } else { "light" };
                text.push_str(&format!("background.{mode}=#{r:02x}{g:02x}{b:02x}\n"));
            }
        }
        for (dark, row) in self.colors.iter().enumerate() {
            for (token, color) in row.iter().enumerate() {
                if let Some([r, g, b]) = color {
                    let key = TOKEN_KEYS[token];
                    let mode = if dark == 1 { "dark" } else { "light" };
                    text.push_str(&format!("color.{mode}.{key}=#{r:02x}{g:02x}{b:02x}\n"));
                }
            }
        }
        text
    }

    /// Reads the file text. Unknown lines and bad values are ignored, so the defaults stand.
    pub fn parse(text: &str) -> Self {
        let mut p = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "font_size" => {
                    if let Ok(v) = value.parse::<f32>() {
                        p.font_size = v;
                    }
                }
                "tab_width" => {
                    if let Ok(v) = value.parse::<u8>() {
                        p.tab_width = v;
                    }
                }
                "line_numbers" => {
                    if let Ok(v) = value.parse::<bool>() {
                        p.line_numbers = v;
                    }
                }
                "font" => {
                    if let Some(i) = crate::theme::EDITOR_FONTS.iter().position(|(n, _)| *n == value) {
                        p.font = i;
                    }
                }
                "background.light" => p.background[0] = parse_hex(value),
                "background.dark" => p.background[1] = parse_hex(value),
                "word_wrap" => {
                    if let Ok(v) = value.parse::<bool>() {
                        p.word_wrap = v;
                    }
                }
                other => {
                    if let Some(rest) = other.strip_prefix("color.") {
                        if let Some((mode, token)) = rest.split_once('.') {
                            let dark = match mode {
                                "light" => Some(0),
                                "dark" => Some(1),
                                _ => None,
                            };
                            let index = TOKEN_KEYS.iter().position(|k| *k == token);
                            if let (Some(dark), Some(index), Some(rgb)) = (dark, index, parse_hex(value)) {
                                p.colors[dark][index] = Some(rgb);
                            }
                        }
                    }
                }
            }
        }
        p.checked()
    }

    /// Sets or clears (`None`) the editor background for one appearance.
    pub fn set_background(&mut self, dark: bool, color: Option<[u8; 3]>) {
        self.background[dark as usize] = color;
    }

    /// Sets or clears (`None`) one token colour for one appearance.
    pub fn set_color(&mut self, dark: bool, token: usize, color: Option<[u8; 3]>) {
        self.colors[dark as usize][token] = color;
    }

    fn file() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/Pascoda/preferences"))
    }

    /// The saved preferences, or the defaults when there are none.
    pub fn load() -> Self {
        Self::file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(f) = Self::file() {
            let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
            let _ = std::fs::write(f, self.to_text());
        }
    }

    /// Makes these the values the editor uses.
    pub fn apply(&self) {
        FONT_BITS.store(self.font_size.to_bits(), Ordering::Relaxed);
        TAB_WIDTH.store(self.tab_width, Ordering::Relaxed);
        LINE_NUMBERS.store(self.line_numbers, Ordering::Relaxed);
        WORD_WRAP.store(self.word_wrap, Ordering::Relaxed);
        FONT.store(self.font, Ordering::Relaxed);
        *BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()) = self.background;
        *CODE_COLORS.lock().unwrap_or_else(|e| e.into_inner()) = self.colors;
    }
}

// Zero until `apply` runs: `font_size` then gives the default.
static FONT_BITS: AtomicU32 = AtomicU32::new(0);
static TAB_WIDTH: AtomicU8 = AtomicU8::new(4);
static LINE_NUMBERS: AtomicBool = AtomicBool::new(true);
static WORD_WRAP: AtomicBool = AtomicBool::new(false);
static FONT: AtomicUsize = AtomicUsize::new(0);

static BACKGROUND: Mutex<[Option<[u8; 3]>; 2]> = Mutex::new([None; 2]);

/// The editor background the user chose for this appearance, if any.
pub fn editor_background(dark: bool) -> Option<[u8; 3]> {
    BACKGROUND.lock().unwrap_or_else(|e| e.into_inner())[dark as usize]
}

/// Index of the editor font (into `theme::EDITOR_FONTS`) in use.
pub fn font_index() -> usize {
    FONT.load(Ordering::Relaxed)
}

/// Whether long lines wrap in the editor.
pub fn word_wrap() -> bool {
    WORD_WRAP.load(Ordering::Relaxed)
}
static CODE_COLORS: Mutex<[[Option<[u8; 3]>; TOKEN_COUNT]; 2]> = Mutex::new([[None; TOKEN_COUNT]; 2]);

/// The token colour the user chose for this appearance, if any.
pub fn code_color(dark: bool, token: usize) -> Option<[u8; 3]> {
    CODE_COLORS.lock().unwrap_or_else(|e| e.into_inner())[dark as usize][token]
}

/// The file keys of the code token colours, in `highlight::TOKENS` order.
pub const TOKEN_KEYS: [&str; TOKEN_COUNT] =
    ["text", "keyword", "string", "comment", "number", "user_type", "builtin_type", "call"];

fn parse_hex(value: &str) -> Option<[u8; 3]> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// The current editor font size (13 until the preferences say otherwise).
pub fn font_size() -> f32 {
    let v = f32::from_bits(FONT_BITS.load(Ordering::Relaxed));
    if v == 0.0 { BASE_FONT } else { v }
}

/// The current editor line height.
pub fn line_height() -> f32 {
    Prefs { font_size: font_size(), ..Prefs::default() }.line_height()
}

/// Whether the editor's gutter shows line numbers (breakpoints and problem marks show either way).
pub fn line_numbers() -> bool {
    LINE_NUMBERS.load(Ordering::Relaxed)
}

/// The number of spaces the Tab key inserts.
pub fn tab_width() -> u8 {
    TAB_WIDTH.load(Ordering::Relaxed)
}

/// Replaces the characters `start..end` of `text` with spaces up to the next tab stop of the
/// editor's tab width (or `width` spaces when the selection is empty, from the caret). Returns the new
/// text and the caret, which ends after the spaces.
pub fn insert_tab(text: &str, start: usize, end: usize, width: u8) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let (start, end) = (start.min(chars.len()), end.min(chars.len()));
    let (start, end) = (start.min(end), start.max(end));
    // The column of the caret: characters since the line start.
    let line_start = chars[..start].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1);
    let column = start - line_start;
    let width = width.max(1) as usize;
    let spaces = width - column % width;
    let mut out: String = chars[..start].iter().collect();
    out.extend(std::iter::repeat_n(' ', spaces));
    out.extend(&chars[end..]);
    (out, start + spaces)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `apply` changes process-wide values. Tests that apply take it for writing; tests that draw
    /// the editor take it for reading, so they never see a half-applied change.
    pub static APPLY: std::sync::RwLock<()> = std::sync::RwLock::new(());

    #[test]
    fn the_line_height_follows_the_font_size() {
        assert_eq!(Prefs::default().line_height(), 18.0);
        assert_eq!(Prefs { font_size: 16.0, ..Prefs::default() }.line_height(), 22.0);
        assert_eq!(Prefs { font_size: 10.0, ..Prefs::default() }.line_height(), 14.0);
    }

    #[test]
    fn preferences_round_trip_and_bad_values_fall_back() {
        let p = Prefs { font_size: 15.0, tab_width: 8, line_numbers: false, word_wrap: true, ..Prefs::default() };
        assert_eq!(Prefs::parse(&p.to_text()), p);
        assert_eq!(Prefs::parse("font_size=99\ntab_width=3\n"), Prefs { font_size: 20.0, ..Prefs::default() });
        assert!(!Prefs::parse("line_numbers=false\n").line_numbers);
        assert!(Prefs::parse("word_wrap=true\n").word_wrap);
        assert!(!Prefs::parse("word_wrap=sometimes\n").word_wrap);
        assert_eq!(Prefs::parse("font=Menlo\n").font, 1);
        assert_eq!(Prefs::parse("font=Comic Sans\n").font, 0, "an unknown font keeps the default");
        assert!(Prefs::parse("line_numbers=maybe\n").line_numbers, "a bad value keeps the default");
        assert_eq!(Prefs::parse("font_size=13.4\n").font_size, 13.0, "whole points");
        assert_eq!(Prefs::parse("font_size=abc\nunknown=1\n"), Prefs::default());
        assert_eq!(Prefs::parse(""), Prefs::default());
    }

    #[test]
    fn token_colours_are_saved_per_appearance_and_applied() {
        let _guard = APPLY.write().unwrap_or_else(|e| e.into_inner());
        let mut p = Prefs::default();
        p.set_color(false, 1, Some([0x12, 0x34, 0xAB]));
        p.set_color(true, 2, Some([0, 255, 16]));
        let text = p.to_text();
        assert!(text.contains("color.light.keyword=#1234ab\n"), "{text}");
        assert!(text.contains("color.dark.string=#00ff10\n"), "{text}");
        assert_eq!(Prefs::parse(&text), p);
        // Bad colours are ignored; the keyword keeps its default.
        let bad = Prefs::parse("color.light.keyword=blue\ncolor.purple.keyword=#000000\ncolor.light.nope=#000000\n");
        assert_eq!(bad, Prefs::default());
        // Applying makes them what highlighting reads.
        p.apply();
        assert_eq!(code_color(false, 1), Some([0x12, 0x34, 0xAB]));
        assert_eq!(code_color(true, 1), None);
        Prefs::default().apply();
        assert_eq!(code_color(false, 1), None);
    }

    #[test]
    fn the_font_and_background_are_saved_and_applied() {
        let _guard = APPLY.write().unwrap_or_else(|e| e.into_inner());
        let mut p = Prefs { font: 2, ..Prefs::default() };
        p.set_background(true, Some([0x10, 0x20, 0x30]));
        let text = p.to_text();
        assert!(text.contains("font=Courier\n"), "{text}");
        assert!(text.contains("background.dark=#102030\n"), "{text}");
        assert_eq!(Prefs::parse(&text), p);
        assert_eq!(Prefs::parse("background.light=#zzzzzz\n").background, [None; 2]);
        p.apply();
        assert_eq!(font_index(), 2);
        assert_eq!(editor_background(true), Some([0x10, 0x20, 0x30]));
        assert_eq!(editor_background(false), None);
        Prefs::default().apply();
        assert_eq!(font_index(), 0);
        assert_eq!(editor_background(true), None);
    }

    #[test]
    fn applied_values_are_what_the_editor_reads() {
        let _guard = APPLY.write().unwrap_or_else(|e| e.into_inner());
        Prefs { font_size: 16.0, tab_width: 2, line_numbers: false, ..Prefs::default() }.apply();
        assert_eq!(font_size(), 16.0);
        assert_eq!(line_height(), 22.0);
        assert_eq!(tab_width(), 2);
        assert!(!line_numbers());
        Prefs::default().apply();
        assert!(line_numbers());
        assert!(!word_wrap());
        assert_eq!(font_index(), 0);
        assert_eq!(font_size(), 13.0);
        assert_eq!(line_height(), 18.0);
    }

    #[test]
    fn tab_fills_to_the_next_stop() {
        // Column 0 with width 4: four spaces; column 2: two spaces.
        assert_eq!(insert_tab("", 0, 0, 4), ("    ".to_string(), 4));
        assert_eq!(insert_tab("ab", 2, 2, 4), ("ab  ".to_string(), 4));
        // Columns count from the line start, not the text start.
        assert_eq!(insert_tab("x\nabc", 5, 5, 4), ("x\nabc ".to_string(), 6));
        // A selection is replaced by the spaces, from the selection's start column (1 here).
        assert_eq!(insert_tab("abcdef", 1, 3, 4), ("a   def".to_string(), 4));
    }
}

//! Text measurement and segmentation.
//!
//! Milestone 1 ships a deterministic metrics provider so layout and golden
//! tests do not depend on installed fonts. The real implementation plugs
//! `parley`/`swash` behind the same [`Metrics`] trait, so layout code does not
//! change.
//!
//! Line breaking works on [`Token`] values: Latin words stay whole, wide
//! (CJK) characters may break individually, whitespace is collapsible at line
//! ends, and explicit newlines force a break.

#![forbid(unsafe_code)]

/// Formatting information needed to measure a glyph.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TextStyle<'a> {
    pub size_pt: f64,
    pub bold: bool,
    pub italic: bool,
    pub family: Option<&'a str>,
    pub east_asian: Option<&'a str>,
}

impl Default for TextStyle<'_> {
    fn default() -> Self {
        Self {
            size_pt: 10.5,
            bold: false,
            italic: false,
            family: None,
            east_asian: None,
        }
    }
}

impl<'a> TextStyle<'a> {
    pub fn new(size_pt: f64, bold: bool) -> Self {
        Self {
            size_pt,
            bold,
            ..Self::default()
        }
    }
}

/// A source of horizontal advances and vertical metrics.
pub trait Metrics {
    fn advance(&self, ch: char, style: TextStyle<'_>) -> f64;
    fn line_height(&self, style: TextStyle<'_>) -> f64;
    fn ascent(&self, style: TextStyle<'_>) -> f64;

    /// Identifies this metrics implementation for layout caches.
    fn cache_key(&self) -> u64 {
        0
    }

    /// Width of a whole token. The default measures one character at a time;
    /// shaping backends override it to include kerning and ligatures.
    fn advance_text(&self, text: &str, style: TextStyle<'_>) -> f64 {
        text.chars().map(|ch| self.advance(ch, style)).sum()
    }
}

/// A piece of text produced by [`tokenize`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// A Latin word or a run of characters that must not break internally.
    Word(String),
    /// A single wide character that may break on either side.
    Char(char),
    /// One collapsible space.
    Space,
    /// A tab; expands to four spaces.
    Tab,
    /// An explicit line break.
    Break,
}

impl Token {
    pub fn text_len(&self) -> usize {
        match self {
            Token::Word(word) => word.chars().count(),
            Token::Char(_) | Token::Space | Token::Tab | Token::Break => 1,
        }
    }
}

/// Split a string into break-opportunity tokens.
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();

    let flush = |word: &mut String, tokens: &mut Vec<Token>| {
        if !word.is_empty() {
            tokens.push(Token::Word(std::mem::take(word)));
        }
    };

    for ch in text.chars() {
        match ch {
            '\n' => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Break);
            }
            '\r' => {}
            ' ' => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Space);
            }
            '\t' => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Tab);
            }
            ch if is_wide(ch) => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Char(ch));
            }
            ch => word.push(ch),
        }
    }
    flush(&mut word, &mut tokens);
    tokens
}

/// True for characters that occupy a full em and may break on either side.
pub fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD)
}

/// Punctuation that may not start a line (避头).
pub fn is_closing_punct(ch: char) -> bool {
    matches!(
        ch,
        '。' | '，'
            | '、'
            | '；'
            | '：'
            | '？'
            | '！'
            | '）'
            | '】'
            | '》'
            | '」'
            | '』'
            | '〉'
            | '”'
            | '’'
            | '…'
            | '·'
            | ','
            | '.'
            | ';'
            | ':'
            | '?'
            | '!'
            | ')'
            | ']'
            | '}'
    )
}

/// Punctuation that may not end a line (避尾).
pub fn is_opening_punct(ch: char) -> bool {
    matches!(
        ch,
        '（' | '【'
            | '《'
            | '「'
            | '『'
            | '〈'
            | '“'
            | '‘'
            | '('
            | '['
            | '{'
    )
}

/// Deterministic placeholder metrics.
///
/// Wide characters are exactly one em; Latin letters are roughly half an em.
/// These values are documented and intentionally simple so layout snapshots
/// are reproducible on every machine.
#[derive(Copy, Clone, Debug, Default)]
pub struct DeterministicMetrics;

impl DeterministicMetrics {
    fn em(ch: char) -> f64 {
        if is_wide(ch) {
            1.0
        } else {
            match ch {
                ' ' => 0.26,
                '\t' => 0.26 * 4.0,
                '\n' | '\r' => 0.0,
                'm' | 'w' | 'M' | 'W' => 0.82,
                'i' | 'l' | 'j' | 't' | 'f' | 'r' => 0.34,
                c if c.is_ascii_uppercase() => 0.62,
                c if c.is_ascii_digit() => 0.55,
                c if c.is_ascii_punctuation() => 0.32,
                _ => 0.52,
            }
        }
    }
}

impl Metrics for DeterministicMetrics {
    fn advance(&self, ch: char, style: TextStyle<'_>) -> f64 {
        let bold_factor = if style.bold { 1.02 } else { 1.0 };
        Self::em(ch) * style.size_pt * bold_factor
    }

    fn line_height(&self, style: TextStyle<'_>) -> f64 {
        style.size_pt * 1.3
    }

    fn ascent(&self, style: TextStyle<'_>) -> f64 {
        style.size_pt * 0.88
    }
}

impl<T: Metrics + ?Sized> Metrics for &T {
    fn advance(&self, ch: char, style: TextStyle<'_>) -> f64 {
        (**self).advance(ch, style)
    }

    fn line_height(&self, style: TextStyle<'_>) -> f64 {
        (**self).line_height(style)
    }

    fn ascent(&self, style: TextStyle<'_>) -> f64 {
        (**self).ascent(style)
    }

    fn advance_text(&self, text: &str, style: TextStyle<'_>) -> f64 {
        (**self).advance_text(text, style)
    }
}


/// Metrics backed by installed system fonts.
///
/// Faces are selected with `fontdb` (family, weight, italic) and advances come
/// from `ttf-parser`. Shaping (ligatures, kerning, complex scripts) is not
/// applied yet, so this is a per-character measurement layer; the layout model
/// is unchanged.
#[derive(Debug)]
pub struct SystemMetrics {
    database: fontdb::Database,
    cache: std::cell::RefCell<
        std::collections::HashMap<(fontdb::ID, char), Option<FaceMetrics>>,
    >,
}

#[derive(Copy, Clone, Debug)]
struct FaceMetrics {
    advance_em: f64,
    ascent_em: f64,
    descent_em: f64,
}

impl SystemMetrics {
    pub fn new() -> Self {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        Self {
            database,
            cache: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    pub fn has_fonts(&self) -> bool {
        self.database.faces().next().is_some()
    }

    fn face_metrics(&self, id: fontdb::ID, ch: char) -> Option<FaceMetrics> {
        if let Some(value) = self.cache.borrow().get(&(id, ch)) {
            return *value;
        }
        let value = self
            .database
            .with_face_data(id, |data, index| {
                let face = ttf_parser::Face::parse(data, index).ok()?;
                let glyph = face.glyph_index(ch)?;
                let units = f64::from(face.units_per_em());
                if units <= 0.0 {
                    return None;
                }
                Some(FaceMetrics {
                    advance_em: f64::from(face.glyph_hor_advance(glyph)?) / units,
                    ascent_em: f64::from(face.ascender()) / units,
                    descent_em: f64::from(face.descender()) / units,
                })
            })
            .flatten();
        self.cache.borrow_mut().insert((id, ch), value);
        value
    }

    fn face_for(&self, ch: char, style: TextStyle<'_>) -> Option<fontdb::ID> {
        let weight = if style.bold {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        };
        let slant = if style.italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        };

        let mut families: Vec<fontdb::Family<'_>> = Vec::new();
        if is_wide(ch) {
            if let Some(name) = style.east_asian {
                families.push(fontdb::Family::Name(name));
            }
        }
        if let Some(name) = style.family {
            families.push(fontdb::Family::Name(name));
        }
        if is_wide(ch) {
            for name in [
                "Microsoft YaHei",
                "SimSun",
                "Noto Sans CJK SC",
                "Source Han Sans SC",
                "PingFang SC",
            ] {
                families.push(fontdb::Family::Name(name));
            }
        }
        for name in ["Segoe UI", "Arial", "Noto Sans", "DejaVu Sans"] {
            families.push(fontdb::Family::Name(name));
        }
        families.push(fontdb::Family::SansSerif);

        for family in families {
            let list = [family];
            let query = fontdb::Query {
                families: &list,
                weight,
                stretch: fontdb::Stretch::Normal,
                style: slant,
            };
            if let Some(id) = self.database.query(&query) {
                if self.face_metrics(id, ch).is_some() {
                    return Some(id);
                }
            }
        }
        None
    }

    fn metrics_for(&self, ch: char, style: TextStyle<'_>) -> Option<FaceMetrics> {
        let id = self.face_for(ch, style)?;
        self.face_metrics(id, ch)
    }

    fn shape_text(&self, text: &str, style: TextStyle<'_>) -> Option<f64> {
        let first = text.chars().next()?;
        let id = self.face_for(first, style)?;
        self.database
            .with_face_data(id, |data, index| {
                let face = rustybuzz::Face::from_slice(data, index)?;
                let mut buffer = rustybuzz::UnicodeBuffer::new();
                buffer.push_str(text);
                buffer.set_direction(rustybuzz::Direction::LeftToRight);
                let glyphs = rustybuzz::shape(&face, &[], buffer);
                if glyphs.glyph_infos().iter().any(|info| info.glyph_id == 0) {
                    return None;
                }
                let units = f64::from(face.units_per_em());
                if units <= 0.0 {
                    return None;
                }
                let advance: i32 = glyphs
                    .glyph_positions()
                    .iter()
                    .map(|position| position.x_advance)
                    .sum();
                Some(f64::from(advance) / units * style.size_pt)
            })
            .flatten()
    }
}

impl Default for SystemMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics for SystemMetrics {
    fn cache_key(&self) -> u64 {
        1
    }

    fn advance(&self, ch: char, style: TextStyle<'_>) -> f64 {
        if ch == '\n' || ch == '\r' {
            0.0
        } else if ch == '\t' {
            4.0 * self.advance(' ', style)
        } else {
            self.metrics_for(ch, style)
                .map(|metrics| metrics.advance_em * style.size_pt)
                .unwrap_or_else(|| {
                    style.size_pt * if is_wide(ch) { 1.0 } else { 0.52 }
                })
        }
    }

    fn advance_text(&self, text: &str, style: TextStyle<'_>) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        if let Some(advance) = self.shape_text(text, style) {
            return advance;
        }
        text.chars().map(|ch| self.advance(ch, style)).sum()
    }

    fn line_height(&self, style: TextStyle<'_>) -> f64 {
        self.metrics_for('M', style)
            .or_else(|| self.metrics_for('中', style))
            .map(|metrics| {
                ((metrics.ascent_em - metrics.descent_em).max(0.8)) * style.size_pt
            })
            .unwrap_or(style.size_pt * 1.3)
    }

    fn ascent(&self, style: TextStyle<'_>) -> f64 {
        self.metrics_for('M', style)
            .or_else(|| self.metrics_for('中', style))
            .map(|metrics| metrics.ascent_em.max(0.6) * style.size_pt)
            .unwrap_or(style.size_pt * 0.88)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_mixed_script() {
        let tokens = tokenize("Hello世界, ok\nnext");
        assert_eq!(
            tokens,
            vec![
                Token::Word("Hello".into()),
                Token::Char('世'),
                Token::Char('界'),
                Token::Word(",".into()),
                Token::Space,
                Token::Word("ok".into()),
                Token::Break,
                Token::Word("next".into()),
            ]
        );
    }

    #[test]
    fn wide_detection() {
        assert!(is_wide('中'));
        assert!(is_wide('。'));
        assert!(!is_wide('A'));
    }

    #[test]
    fn metrics_are_deterministic() {
        let m = DeterministicMetrics;
        let style = TextStyle::default();
        assert_eq!(m.advance('中', style), 10.5);
        assert!((m.advance('a', style) - 5.46).abs() < 1e-9);
        assert!((m.line_height(style) - 13.65).abs() < 1e-9);
    }

    #[test]
    fn system_metrics_measure_when_fonts_exist() {
        let metrics = SystemMetrics::new();
        if !metrics.has_fonts() {
            return;
        }
        let style = TextStyle::default();
        assert!(metrics.advance('A', style) > 0.0);
        assert!(metrics.advance('中', style) > 0.0);
        assert!(metrics.line_height(style) > 0.0);
    }


    #[test]
    fn system_shaping_matches_single_char() {
        let metrics = SystemMetrics::new();
        if !metrics.has_fonts() {
            return;
        }
        let style = TextStyle::default();
        let single = metrics.advance('A', style);
        let text = metrics.advance_text("A", style);
        assert!(single > 0.0);
        assert!((single - text).abs() < 1e-6);
        assert!(metrics.advance_text("Hello", style) > 0.0);
    }

}

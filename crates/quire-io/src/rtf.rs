//! A conservative RTF reader/writer covering paragraphs, runs and basic
//! character/paragraph formatting.

use encoding_rs::{Encoding, GB18030, UTF_8, WINDOWS_1252};
use quire_base::{ParaId, Result};
use quire_model::{Align, CharProps, Color, Document, Paragraph};

#[derive(Clone, Debug)]
enum Token {
    Start,
    End,
    Control(String, Option<i32>),
    Hex(u8),
    Text(Vec<u8>),
}

fn tokenize(bytes: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                tokens.push(Token::Start);
                index += 1;
            }
            b'}' => {
                tokens.push(Token::End);
                index += 1;
            }
            b'\\' => {
                index += 1;
                if index >= bytes.len() {
                    break;
                }
                let next = bytes[index];
                if next == b'\'' {
                    if index + 2 < bytes.len() {
                        let text = String::from_utf8_lossy(&bytes[index + 1..index + 3]).to_string();
                        if let Ok(value) = u8::from_str_radix(&text, 16) {
                            tokens.push(Token::Hex(value));
                        }
                        index += 3;
                    } else {
                        index += 1;
                    }
                } else if next.is_ascii_alphabetic() {
                    let start = index;
                    while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                        index += 1;
                    }
                    let word = String::from_utf8_lossy(&bytes[start..index]).to_string();
                    let mut sign = 1i32;
                    if index < bytes.len() && bytes[index] == b'-' {
                        sign = -1;
                        index += 1;
                    }
                    let number_start = index;
                    while index < bytes.len() && bytes[index].is_ascii_digit() {
                        index += 1;
                    }
                    let number = if number_start < index {
                        std::str::from_utf8(&bytes[number_start..index])
                            .ok()
                            .and_then(|text| text.parse::<i32>().ok())
                            .map(|value| value * sign)
                    } else {
                        None
                    };
                    if index < bytes.len() && bytes[index] == b' ' {
                        index += 1;
                    }
                    tokens.push(Token::Control(word, number));
                } else {
                    tokens.push(Token::Control((next as char).to_string(), None));
                    index += 1;
                }
            }
            _ => {
                let start = index;
                while index < bytes.len()
                    && bytes[index] != b'{'
                    && bytes[index] != b'}'
                    && bytes[index] != b'\\'
                {
                    index += 1;
                }
                let mut chunk = bytes[start..index].to_vec();
                chunk.retain(|byte| *byte != b'\n' && *byte != b'\r');
                if !chunk.is_empty() {
                    tokens.push(Token::Text(chunk));
                }
            }
        }
    }
    tokens
}

#[derive(Clone, Copy)]
struct State {
    bold: bool,
    italic: bool,
    underline: bool,
    size_pt: f64,
    color: Color,
    align: Align,
    indent_left_pt: f64,
    indent_first_pt: f64,
    space_before_pt: f64,
    space_after_pt: f64,
    uc: usize,
    fallback: usize,
    encoding: &'static Encoding,
}

impl Default for State {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            underline: false,
            size_pt: quire_model::DEFAULT_FONT_SIZE_PT,
            color: Color::BLACK,
            align: Align::Left,
            indent_left_pt: 0.0,
            indent_first_pt: 0.0,
            space_before_pt: 0.0,
            space_after_pt: 0.0,
            uc: 1,
            fallback: 0,
            encoding: WINDOWS_1252,
        }
    }
}

fn decode(bytes: &[u8], encoding: &'static Encoding) -> String {
    encoding.decode_without_bom_handling(bytes).0.into_owned()
}

/// Read an RTF file.
pub fn read(bytes: &[u8]) -> Result<Document> {
    let tokens = tokenize(bytes);
    let mut state = State::default();
    let mut stack: Vec<State> = Vec::new();
    let mut depth = 0usize;
    let mut skip_depth: Option<usize> = None;
    let mut pending: Vec<u8> = Vec::new();
    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut paragraph = Paragraph::new(ParaId::new(0));
    let mut colors: Vec<Color> = Vec::new();
    let mut color_depth: Option<usize> = None;
    let mut current_color = Color::BLACK;

    let props = |state: &State| CharProps {
        bold: state.bold,
        italic: state.italic,
        underline: state.underline,
        size_pt: state.size_pt,
        color: state.color,
        ..CharProps::default()
    };

    let flush_pending = |pending: &mut Vec<u8>, state: &State, paragraph: &mut Paragraph| {
        if pending.is_empty() {
            return;
        }
        let text = decode(pending, state.encoding);
        pending.clear();
        let mut remaining = state.fallback;
        for ch in text.chars() {
            if remaining > 0 {
                remaining -= 1;
                continue;
            }
            paragraph.push_text(ch.to_string(), props(state));
        }
    };

    let flush_paragraph = |paragraph: &mut Paragraph, paragraphs: &mut Vec<Paragraph>| {
        let mut next = Paragraph::new(ParaId::new(0));
        std::mem::swap(paragraph, &mut next);
        paragraphs.push(next);
    };

    for token in tokens {
        match token {
            Token::Start => {
                stack.push(state);
                depth += 1;
            }
            Token::End => {
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                if color_depth == Some(depth) {
                    color_depth = None;
                }
                depth = depth.saturating_sub(1);
                if let Some(previous) = stack.pop() {
                    state = previous;
                }
            }
            Token::Text(bytes) => {
                if skip_depth.is_some() {
                    continue;
                }
                if color_depth.is_some() {
                    for byte in bytes {
                        if byte == b';' {
                            colors.push(current_color);
                            current_color = Color::BLACK;
                        }
                    }
                    continue;
                }
                flush_pending(&mut pending, &state, &mut paragraph);
                let text = decode(&bytes, state.encoding);
                let mut remaining = state.fallback;
                for ch in text.chars() {
                    if remaining > 0 {
                        remaining -= 1;
                        continue;
                    }
                    paragraph.push_text(ch.to_string(), props(&state));
                }
            }
            Token::Hex(byte) => {
                if skip_depth.is_some() {
                    continue;
                }
                if state.fallback > 0 {
                    state.fallback -= 1;
                } else {
                    pending.push(byte);
                }
            }
            Token::Control(word, number) => {
                if skip_depth.is_some() {
                    continue;
                }
                match word.as_str() {
                    "fonttbl" | "stylesheet" | "info" | "pict" | "object" | "header"
                    | "footer" | "footnote" | "annotation" | "field" | "fldinst"
                    | "generator" | "listtable" | "listoverridetable" | "revtbl"
                    | "themedata" | "colorschememapping" | "datastore" | "xmlnstbl" => {
                        skip_depth = Some(depth);
                    }
                    "colortbl" => color_depth = Some(depth),
                    "*" => skip_depth = Some(depth),
                    _ => {
                        flush_pending(&mut pending, &state, &mut paragraph);
                        match word.as_str() {
                            "par" => {
                                paragraph.props.align = state.align;
                                paragraph.props.indent_left_pt = state.indent_left_pt;
                                paragraph.props.indent_first_pt = state.indent_first_pt;
                                paragraph.props.space_before_pt = state.space_before_pt;
                                paragraph.props.space_after_pt = state.space_after_pt;
                                flush_paragraph(&mut paragraph, &mut paragraphs);
                            }
                            "line" => {
                                paragraph.push_text("\n", props(&state));
                            }
                            "tab" => {
                                paragraph.push_text("\t", props(&state));
                            }
                            "pard" => {
                                state.align = Align::Left;
                                state.indent_left_pt = 0.0;
                                state.indent_first_pt = 0.0;
                                state.space_before_pt = 0.0;
                                state.space_after_pt = 0.0;
                            }
                            "ql" => state.align = Align::Left,
                            "qc" => state.align = Align::Center,
                            "qr" => state.align = Align::Right,
                            "qj" => state.align = Align::Justify,
                            "b" => state.bold = number != Some(0),
                            "i" => state.italic = number != Some(0),
                            "ul" => state.underline = number != Some(0),
                            "ulnone" => state.underline = false,
                            "fs" => {
                                if let Some(value) = number {
                                    if value > 0 {
                                        state.size_pt = f64::from(value) / 2.0;
                                    }
                                }
                            }
                            "cf" => {
                                if let Some(value) = number {
                                    if value > 0 {
                                        state.color = colors
                                            .get(value as usize - 1)
                                            .copied()
                                            .unwrap_or(Color::BLACK);
                                    } else {
                                        state.color = Color::BLACK;
                                    }
                                }
                            }
                            "li" => {
                                if let Some(value) = number {
                                    state.indent_left_pt = f64::from(value) / 20.0;
                                }
                            }
                            "fi" => {
                                if let Some(value) = number {
                                    state.indent_first_pt = f64::from(value) / 20.0;
                                }
                            }
                            "sb" => {
                                if let Some(value) = number {
                                    state.space_before_pt = f64::from(value) / 20.0;
                                }
                            }
                            "sa" => {
                                if let Some(value) = number {
                                    state.space_after_pt = f64::from(value) / 20.0;
                                }
                            }
                            "ansicpg" => {
                                state.encoding = match number {
                                    Some(936) => GB18030,
                                    Some(65001) => UTF_8,
                                    _ => WINDOWS_1252,
                                };
                            }
                            "uc" => {
                                if let Some(value) = number {
                                    state.uc = value.max(0) as usize;
                                    state.fallback = 0;
                                }
                            }
                            "u" => {
                                if let Some(value) = number {
                                    let value = if value < 0 {
                                        (value + 65536) as u32
                                    } else {
                                        value as u32
                                    };
                                    if let Some(ch) = char::from_u32(value) {
                                        paragraph.push_text(ch.to_string(), props(&state));
                                        state.fallback = state.uc;
                                    }
                                }
                            }
                            "red" => {
                                if let Some(value) = number {
                                    current_color.r = value.clamp(0, 255) as u8;
                                }
                            }
                            "green" => {
                                if let Some(value) = number {
                                    current_color.g = value.clamp(0, 255) as u8;
                                }
                            }
                            "blue" => {
                                if let Some(value) = number {
                                    current_color.b = value.clamp(0, 255) as u8;
                                }
                            }
                            "~" => paragraph.push_text("\u{00A0}", props(&state)),
                            "_" => paragraph.push_text("-", props(&state)),
                            "-" => {}
                            "\\" | "{" | "}" => {
                                paragraph.push_text(word, props(&state));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    flush_pending(&mut pending, &state, &mut paragraph);
    if !paragraph.is_empty() || paragraphs.is_empty() {
        paragraph.props.align = state.align;
        paragraph.props.indent_left_pt = state.indent_left_pt;
        paragraph.props.indent_first_pt = state.indent_first_pt;
        paragraph.props.space_before_pt = state.space_before_pt;
        paragraph.props.space_after_pt = state.space_after_pt;
        paragraphs.push(paragraph);
    }
    let mut document = Document::new();
    document.clear_sections();
    let mut section = quire_model::Section::default();
    for paragraph in paragraphs {
        section.push_paragraph_block(paragraph);
    }
    document.push_section(section);
    document.renumber();
    document.validate()?;
    Ok(document)
}

fn escape_rtf(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\n' => out.push_str("\\line "),
            ch if (ch as u32) < 128 => out.push(ch),
            ch => {
                let value = ch as i32;
                let value = if value > 32767 { value - 65536 } else { value };
                out.push_str(&format!("\\u{value}?"));
            }
        }
    }
}

/// Write a minimal RTF document.
pub fn write(document: &Document) -> Vec<u8> {
    let mut out = String::from(
        "{\\rtf1\\ansi\\ansicpg936\\deff0{\\fonttbl{\\f0\\fnil\\fcharset134 Microsoft YaHei;}}\n",
    );
    for paragraph in document.paragraphs() {
        out.push_str("\\pard");
        match paragraph.props.align {
            Align::Left => out.push_str("\\ql"),
            Align::Center => out.push_str("\\qc"),
            Align::Right => out.push_str("\\qr"),
            Align::Justify => out.push_str("\\qj"),
        }
        out.push(' ');
        for run in &paragraph.runs {
            out.push('{');
            if run.props.bold {
                out.push_str("\\b");
            }
            if run.props.italic {
                out.push_str("\\i");
            }
            if run.props.underline {
                out.push_str("\\ul");
            }
            out.push_str(&format!("\\fs{} ", (run.props.size_pt * 2.0).round() as i32));
            escape_rtf(&run.text, &mut out);
            out.push('}');
        }
        out.push_str("\\par\n");
    }
    out.push('}');
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_paragraphs_and_unicode() {
        let doc = read(b"{\\rtf1\\ansi Hello\\par \\u20013?\\par}").unwrap();
        assert_eq!(doc.paragraph_count(), 2);
        assert_eq!(doc.paragraph(0, 0).unwrap().text(), "Hello");
        assert_eq!(doc.paragraph(0, 1).unwrap().text(), "中");
    }

    #[test]
    fn reads_bold_runs() {
        let doc = read(b"{\\rtf1\\ansi {\\b Bold} normal\\par}").unwrap();
        let paragraph = doc.paragraph(0, 0).unwrap();
        assert!(paragraph.runs.iter().any(|run| run.props.bold));
    }

    #[test]
    fn round_trips_text() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Hello 世界"));
        let written = write(&doc);
        let read_back = read(&written).unwrap();
        assert_eq!(read_back.paragraph(0, 0).unwrap().text(), "Hello 世界");
    }
}

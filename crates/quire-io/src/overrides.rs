//! Property overrides.
//!
//! OOXML formatting is layered: document defaults, paragraph style, character
//! style, then direct formatting. The reader collects each layer as
//! [`CharOverride`] / [`ParaOverride`] (every field optional) and applies them
//! in order once the whole element has been seen. This avoids depending on the
//! child element order inside `w:rPr` / `w:pPr`.

use quick_xml::events::BytesStart;
use quire_model::{Align, CharProps, Color, ParaProps};

use crate::xml::{attribute, half_points_to_pt, parse_bool, twips_to_pt};

/// Optional character properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CharOverride {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub bold_complex: Option<bool>,
    pub italic_complex: Option<bool>,
    pub underline: Option<bool>,
    pub size_pt: Option<f64>,
    pub size_complex_pt: Option<f64>,
    pub color: Option<Color>,
    pub font_family: Option<String>,
    pub font_family_east_asian: Option<String>,
    pub language: Option<String>,
    pub language_east_asian: Option<String>,
}

impl CharOverride {
    pub fn apply(&self, base: &CharProps) -> CharProps {
        let mut props = base.clone();
        if let Some(bold) = self.bold {
            props.bold = bold;
        }
        if let Some(italic) = self.italic {
            props.italic = italic;
        }
        if let Some(underline) = self.underline {
            props.underline = underline;
        }
        if let Some(size_pt) = self.size_pt {
            props.size_pt = size_pt;
        }
        if let Some(color) = self.color {
            props.color = color;
        }
        if let Some(size) = self.size_complex_pt {
            props.size_complex_pt = Some(size);
        }
        if let Some(font) = &self.font_family {
            props.font_family = Some(font.clone());
        }
        if let Some(font) = &self.font_family_east_asian {
            props.font_family_east_asian = Some(font.clone());
        }
        if let Some(bold) = self.bold_complex {
            props.bold_complex = bold;
        }
        if let Some(italic) = self.italic_complex {
            props.italic_complex = italic;
        }
        if let Some(language) = &self.language {
            props.language = Some(language.clone());
        }
        if let Some(language) = &self.language_east_asian {
            props.language_east_asian = Some(language.clone());
        }
        props
    }
}

/// Optional paragraph properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParaOverride {
    pub align: Option<Align>,
    pub indent_left_pt: Option<f64>,
    pub indent_right_pt: Option<f64>,
    pub indent_first_pt: Option<f64>,
    pub space_before_pt: Option<f64>,
    pub space_after_pt: Option<f64>,
    pub line_spacing: Option<f64>,
}

impl ParaOverride {
    pub fn apply(&self, base: &ParaProps) -> ParaProps {
        let mut props = base.clone();
        if let Some(align) = self.align {
            props.align = align;
        }
        if let Some(value) = self.indent_left_pt {
            props.indent_left_pt = value;
        }
        if let Some(value) = self.indent_right_pt {
            props.indent_right_pt = value;
        }
        if let Some(value) = self.indent_first_pt {
            props.indent_first_pt = value;
        }
        if let Some(value) = self.space_before_pt {
            props.space_before_pt = value;
        }
        if let Some(value) = self.space_after_pt {
            props.space_after_pt = value;
        }
        if let Some(value) = self.line_spacing {
            props.line_spacing = value;
        }
        props
    }
}

/// Apply one child of `w:rPr`.
pub fn apply_char_element(name: &[u8], element: &BytesStart<'_>, out: &mut CharOverride) {
    match name {
        b"b" => out.bold = Some(toggle_value(element)),
        b"i" => out.italic = Some(toggle_value(element)),
        b"bCs" => out.bold_complex = Some(toggle_value(element)),
        b"iCs" => out.italic_complex = Some(toggle_value(element)),
        b"u" => out.underline = Some(toggle_value(element)),
        b"sz" => {
            if let Some(value) = attribute(element, b"val") {
                if let Some(size) = half_points_to_pt(&value) {
                    out.size_pt = Some(size);
                }
            }
        }
        b"szCs" => {
            if let Some(value) = attribute(element, b"val") {
                if let Some(size) = half_points_to_pt(&value) {
                    out.size_complex_pt = Some(size);
                }
            }
        }
        b"color" => {
            if let Some(value) = attribute(element, b"val") {
                if let Some(color) = Color::from_hex(&value) {
                    out.color = Some(color);
                }
            }
        }
        b"rFonts" => {
            if let Some(value) = attribute(element, b"ascii").or_else(|| attribute(element, b"hAnsi")) {
                out.font_family = Some(value);
            }
            if let Some(value) = attribute(element, b"eastAsia") {
                out.font_family_east_asian = Some(value);
            }
        }
        b"lang" => {
            if let Some(value) = attribute(element, b"val") {
                out.language = Some(value);
            }
            if let Some(value) = attribute(element, b"eastAsia") {
                out.language_east_asian = Some(value);
            }
        }
        _ => {}
    }
}

/// Apply one child of `w:pPr`.
pub fn apply_para_element(name: &[u8], element: &BytesStart<'_>, out: &mut ParaOverride) {
    match name {
        b"jc" => {
            if let Some(value) = attribute(element, b"val") {
                out.align = Some(match value.as_str() {
                    "center" => Align::Center,
                    "right" | "end" => Align::Right,
                    "both" | "distribute" | "thaiDistribute" => Align::Justify,
                    _ => Align::Left,
                });
            }
        }
        b"ind" => {
            if let Some(value) =
                attribute(element, b"left").or_else(|| attribute(element, b"start"))
            {
                if let Some(pt) = twips_to_pt(&value) {
                    out.indent_left_pt = Some(pt);
                }
            }
            if let Some(value) =
                attribute(element, b"right").or_else(|| attribute(element, b"end"))
            {
                if let Some(pt) = twips_to_pt(&value) {
                    out.indent_right_pt = Some(pt);
                }
            }
            if let Some(value) = attribute(element, b"firstLine") {
                if let Some(pt) = twips_to_pt(&value) {
                    out.indent_first_pt = Some(pt);
                }
            }
            if let Some(value) = attribute(element, b"hanging") {
                if let Some(pt) = twips_to_pt(&value) {
                    out.indent_first_pt = Some(-pt);
                }
            }
        }
        b"spacing" => {
            if let Some(value) = attribute(element, b"before") {
                if let Some(pt) = twips_to_pt(&value) {
                    out.space_before_pt = Some(pt.max(0.0));
                }
            }
            if let Some(value) = attribute(element, b"after") {
                if let Some(pt) = twips_to_pt(&value) {
                    out.space_after_pt = Some(pt.max(0.0));
                }
            }
            if let Some(value) = attribute(element, b"line") {
                if let Ok(raw) = value.trim().parse::<f64>() {
                    let rule = attribute(element, b"lineRule").unwrap_or_else(|| "auto".into());
                    if rule == "auto" && raw > 0.0 {
                        out.line_spacing = Some((raw / 240.0).clamp(0.5, 5.0));
                    }
                }
            }
        }
        _ => {}
    }
}

fn toggle_value(element: &BytesStart<'_>) -> bool {
    attribute(element, b"val")
        .map(|value| parse_bool(&value))
        .unwrap_or(true)
}

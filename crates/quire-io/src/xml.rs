//! Namespace-agnostic XML helpers for OOXML parts.
//!
//! OOXML producers may use any prefix (`w:`, `w14:`, ...), so the reader
//! compares *local* names only. This is intentionally narrower than full
//! namespace resolution: the documents we accept are WordprocessingML parts,
//! and rejecting alternate prefixes would be wrong.

use quick_xml::events::BytesStart;

pub fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|byte| *byte == b':') {
        Some(index) => &name[index + 1..],
        None => name,
    }
}

pub fn attribute(element: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    element
        .attributes()
        .with_checks(false)
        .flatten()
        .find(|attr| local_name(attr.key.as_ref()) == name)
        .and_then(|attr| attr.unescape_value().ok())
        .map(|value| value.into_owned())
}

pub fn twips_to_pt(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<i32>()
        .ok()
        .map(|twips| f64::from(twips) / 20.0)
}

pub fn emu_to_pt(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .map(|emu| emu as f64 / 12_700.0)
}

pub fn half_points_to_pt(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| *value > 0.0)
        .map(|half_points| half_points / 2.0)
}

pub fn parse_bool(value: &str) -> bool {
    !matches!(value, "0" | "false" | "off" | "none")
}

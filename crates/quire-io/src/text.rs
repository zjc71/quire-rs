//! Plain text reading and writing.

use encoding_rs::{Encoding, GB18030, UTF_16BE, UTF_16LE, UTF_8};
use quire_base::{QuireError, Result};
use quire_model::Document;

/// Decode a text file, handling BOMs, UTF-8 and GB18030 fallback.
pub fn decode(bytes: &[u8]) -> Result<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return decode_with(UTF_8, rest);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_with(UTF_16LE, rest);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_with(UTF_16BE, rest);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_string());
    }
    decode_with(GB18030, bytes)
}

fn decode_with(encoding: &'static Encoding, bytes: &[u8]) -> Result<String> {
    let (text, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        return Err(QuireError::format("text is neither UTF-8 nor GB18030"));
    }
    Ok(text.into_owned())
}

/// Read plain text into a paragraph-per-line document.
pub fn read(bytes: &[u8]) -> Result<Document> {
    let text = decode(bytes)?;
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    Ok(Document::from_text(normalized.trim_end_matches('\n')))
}

/// Write a document as UTF-8 text.
pub fn write(document: &Document) -> Vec<u8> {
    let mut text = document.plain_text();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_utf8_and_bom() {
        let doc = read("第一行\nsecond".as_bytes()).unwrap();
        assert_eq!(doc.paragraph_count(), 2);
        let doc = read(&[0xEF, 0xBB, 0xBF, b'a', b'\n', b'b']).unwrap();
        assert_eq!(doc.paragraph(0, 0).unwrap().text(), "a");
    }

    #[test]
    fn falls_back_to_gb18030() {
        // "中文" in GB18030
        let bytes = [0xD6, 0xD0, 0xCE, 0xC4];
        let doc = read(&bytes).unwrap();
        assert_eq!(doc.paragraph(0, 0).unwrap().text(), "中文");
    }
}

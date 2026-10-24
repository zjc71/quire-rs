//! Small XML helpers shared by the format and paint crates.

/// Escape text for XML character data and attribute values.
///
/// Control characters that XML 1.0 cannot represent are dropped.
pub fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(ch),
            ch if (ch as u32) < 0x20 => {}
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_special_characters() {
        assert_eq!(
            escape_xml("a<b>&\"'"),
            "a&lt;b&gt;&amp;&quot;&apos;"
        );
    }

    #[test]
    fn drops_control_characters() {
        assert_eq!(escape_xml("a\u{0001}b"), "ab");
    }
}

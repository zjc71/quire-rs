//! Open Packaging Conventions helpers: relationships and part names.

use std::collections::BTreeMap;

use quire_base::{QuireError, Result};
use quick_xml::events::Event;
use quick_xml::Reader;

/// One relationship from a `.rels` part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relationship {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub external: bool,
}

impl Relationship {
    pub fn is_image(&self) -> bool {
        !self.external && self.kind.ends_with("/image")
    }
}

/// Parse a `.rels` part.
pub fn parse_relationships(xml: &str) -> Result<BTreeMap<String, Relationship>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut out = BTreeMap::new();

    loop {
        match reader
            .read_event()
            .map_err(|err| QuireError::format(format!("relationship xml error: {err}")))?
        {
            Event::Start(element) | Event::Empty(element) => {
                if crate::xml::local_name(element.name().as_ref()) != b"Relationship" {
                    continue;
                }
                let Some(id) = crate::xml::attribute(&element, b"Id") else {
                    continue;
                };
                let kind = crate::xml::attribute(&element, b"Type").unwrap_or_default();
                let target = crate::xml::attribute(&element, b"Target").unwrap_or_default();
                let external =
                    crate::xml::attribute(&element, b"TargetMode").is_some_and(|mode| mode == "External");
                if !id.is_empty() && !target.is_empty() {
                    out.insert(
                        id.clone(),
                        Relationship {
                            id,
                            kind,
                            target,
                            external,
                        },
                    );
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(out)
}

/// Resolve a relationship target against a base directory such as `word`.
///
/// Returns `None` for external targets, absolute URLs, or paths that would
/// escape the package root. `..` is resolved lexically.
pub fn normalize_target(base_dir: &str, target: &str) -> Option<String> {
    if target.contains("://") || target.starts_with("http:") || target.starts_with("https:") {
        return None;
    }
    let target = target.replace('\\', "/");
    let mut parts: Vec<String> = Vec::new();
    if !target.starts_with('/') {
        for part in base_dir.split('/').filter(|part| !part.is_empty()) {
            parts.push(part.to_string());
        }
    }
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other.to_string()),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Best-effort MIME type from a part extension.
pub fn mime_for_part(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "emf" => "image/emf",
        "wmf" => "image/wmf",
        _ => "application/octet-stream",
    }
}

/// File extension for a MIME type used when writing media parts.
pub fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        "image/emf" => "emf",
        "image/wmf" => "wmf",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relationships() {
        let xml = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
            <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
            <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.test" TargetMode="External"/>
        </Relationships>"#;
        let rels = parse_relationships(xml).unwrap();
        assert_eq!(rels.len(), 2);
        assert!(rels["rId1"].is_image());
        assert!(rels["rId2"].external);
    }

    #[test]
    fn normalizes_targets() {
        assert_eq!(
            normalize_target("word", "media/image1.png").as_deref(),
            Some("word/media/image1.png")
        );
        assert_eq!(
            normalize_target("word", "../media/x.png").as_deref(),
            Some("media/x.png")
        );
        assert_eq!(normalize_target("word", "/word/styles.xml").as_deref(), Some("word/styles.xml"));
        assert_eq!(normalize_target("word", "https://example.test/x"), None);
        assert_eq!(normalize_target("word", "../../../etc/passwd"), None);
    }

    #[test]
    fn maps_mime_types() {
        assert_eq!(mime_for_part("word/media/a.PNG"), "image/png");
        assert_eq!(extension_for_mime("image/jpeg"), "jpg");
    }
}

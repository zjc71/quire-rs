//! `word/styles.xml` parsing and style resolution.
//!
//! Styles are resolved to effective properties at read time. Layout therefore
//! never walks `basedOn` chains, and the writer can emit concrete properties
//! even if the original style graph used inheritance.

use std::collections::{BTreeMap, BTreeSet};

use quick_xml::events::Event;
use quick_xml::Reader;
use quire_base::{QuireError, Result};
use quire_model::{CharProps, ParaProps, Style, StyleKind, StyleSheet};

use crate::overrides::{apply_char_element, apply_para_element, CharOverride, ParaOverride};
use crate::xml::{attribute, local_name};

/// Document-level default formatting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleDefaults {
    pub para: ParaProps,
    pub char: CharProps,
    pub paragraph_id: Option<String>,
}

#[derive(Clone, Debug)]
struct RawStyle {
    id: String,
    name: String,
    kind: StyleKind,
    based_on: Option<String>,
    is_default: bool,
    para: ParaOverride,
    char: CharOverride,
}

#[derive(Clone, Debug, Default)]
struct Resolved {
    para: Option<ParaProps>,
    char: Option<CharProps>,
}

/// Parse `word/styles.xml`.
pub fn parse_styles(xml: &str) -> Result<(StyleSheet, StyleDefaults)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;

    let mut defaults = StyleDefaults::default();
    let mut default_char = CharOverride::default();
    let mut default_para = ParaOverride::default();
    let mut raws: BTreeMap<String, RawStyle> = BTreeMap::new();
    let mut current: Option<RawStyle> = None;

    let mut in_doc_defaults = false;
    let mut in_default_char = false;
    let mut in_default_para = false;
    let mut in_style = false;
    let mut in_style_char = false;
    let mut in_style_para = false;

    loop {
        let event = reader
            .read_event()
            .map_err(|err| QuireError::format(format!("styles xml error: {err}")))?;

        match event {
            Event::Start(element) | Event::Empty(element) => {
                let name = local_name(element.name().as_ref()).to_vec();
                match name.as_slice() {
                    b"docDefaults" => in_doc_defaults = true,
                    b"rPrDefault" => in_default_char = true,
                    b"pPrDefault" => in_default_para = true,
                    b"style" => {
                        if let Some(id) = attribute(&element, b"styleId") {
                            let kind = match attribute(&element, b"type").as_deref() {
                                Some("paragraph") => StyleKind::Paragraph,
                                Some("character") => StyleKind::Character,
                                _ => StyleKind::Other,
                            };
                            let is_default = attribute(&element, b"default")
                                .is_some_and(|value| value == "1" || value == "true");
                            current = Some(RawStyle {
                                id: id.clone(),
                                name: id,
                                kind,
                                based_on: None,
                                is_default,
                                para: ParaOverride::default(),
                                char: CharOverride::default(),
                            });
                            in_style = true;
                        }
                    }
                    b"name" => {
                        if let Some(style) = current.as_mut() {
                            if let Some(value) = attribute(&element, b"val") {
                                style.name = value;
                            }
                        }
                    }
                    b"basedOn" => {
                        if let Some(style) = current.as_mut() {
                            if let Some(value) = attribute(&element, b"val") {
                                style.based_on = Some(value);
                            }
                        }
                    }
                    b"rPr" => {
                        if in_style {
                            in_style_char = true;
                        }
                    }
                    b"pPr" => {
                        if in_style {
                            in_style_para = true;
                        }
                    }
                    _ => {
                        if in_default_char && in_doc_defaults {
                            apply_char_element(&name, &element, &mut default_char);
                        } else if in_default_para && in_doc_defaults {
                            apply_para_element(&name, &element, &mut default_para);
                        } else if in_style_char {
                            if let Some(style) = current.as_mut() {
                                apply_char_element(&name, &element, &mut style.char);
                            }
                        } else if in_style_para {
                            if let Some(style) = current.as_mut() {
                                apply_para_element(&name, &element, &mut style.para);
                            }
                        }
                    }
                }
            }
            Event::End(element) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                match name {
                    b"docDefaults" => in_doc_defaults = false,
                    b"rPrDefault" => in_default_char = false,
                    b"pPrDefault" => in_default_para = false,
                    b"rPr" => in_style_char = false,
                    b"pPr" => in_style_para = false,
                    b"style" => {
                        if let Some(style) = current.take() {
                            raws.insert(style.id.clone(), style);
                        }
                        in_style = false;
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    defaults.char = default_char.apply(&CharProps::default());
    defaults.para = default_para.apply(&ParaProps::default());

    // Choose the default paragraph style before resolving so defaults can
    // reference it.
    let default_paragraph = raws
        .values()
        .find(|style| style.is_default && style.kind == StyleKind::Paragraph)
        .map(|style| style.id.clone())
        .or_else(|| raws.contains_key("Normal").then(|| "Normal".to_string()));

    let mut resolver = Resolver {
        raws: &raws,
        defaults: &defaults,
        cache: BTreeMap::new(),
        visiting: BTreeSet::new(),
    };

    let mut sheet = StyleSheet::new();
    let ids: Vec<String> = raws.keys().cloned().collect();
    for id in ids {
        let style = resolver.resolve(&id);
        let raw = &raws[&id];
        let mut out = Style::new(raw.id.clone(), raw.name.clone(), raw.kind);
        out.para = style.para;
        out.char = style.char;
        sheet.insert(out);
    }

    if let Some(id) = &default_paragraph {
        if let Some(char_props) = sheet.char_props(id) {
            defaults.char = char_props.clone();
        }
    }
    defaults.paragraph_id = default_paragraph;

    Ok((sheet, defaults))
}

struct Resolver<'a> {
    raws: &'a BTreeMap<String, RawStyle>,
    defaults: &'a StyleDefaults,
    cache: BTreeMap<String, Resolved>,
    visiting: BTreeSet<String>,
}

impl Resolver<'_> {
    fn resolve(&mut self, id: &str) -> Resolved {
        if let Some(cached) = self.cache.get(id) {
            return cached.clone();
        }
        if !self.visiting.insert(id.to_string()) {
            // Cycle or self reference: fall back to defaults instead of
            // recursing forever.
            return Resolved::default();
        }

        let Some(raw) = self.raws.get(id) else {
            self.visiting.remove(id);
            return Resolved::default();
        };

        let parent = raw
            .based_on
            .as_deref()
            .map(|parent| self.resolve(parent))
            .unwrap_or_default();

        let resolved = match raw.kind {
            StyleKind::Paragraph => {
                let base_para = parent
                    .para
                    .clone()
                    .unwrap_or_else(|| self.defaults.para.clone());
                let base_char = parent
                    .char
                    .clone()
                    .unwrap_or_else(|| self.defaults.char.clone());
                Resolved {
                    para: Some(raw.para.apply(&base_para)),
                    char: Some(raw.char.apply(&base_char)),
                }
            }
            StyleKind::Character => Resolved {
                para: None,
                char: Some(raw.char.apply(
                    &parent
                        .char
                        .clone()
                        .unwrap_or_else(|| self.defaults.char.clone()),
                )),
            },
            StyleKind::Other => Resolved::default(),
        };

        self.visiting.remove(id);
        self.cache.insert(id.to_string(), resolved.clone());
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:docDefaults>
    <w:rPrDefault><w:rPr><w:sz w:val="20"/></w:rPr></w:rPrDefault>
  </w:docDefaults>
  <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
    <w:name w:val="Normal"/>
    <w:rPr><w:b/></w:rPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="Body">
    <w:name w:val="Body Text"/>
    <w:basedOn w:val="Normal"/>
    <w:pPr><w:jc w:val="center"/><w:ind w:left="480"/></w:pPr>
    <w:rPr><w:i/></w:rPr>
  </w:style>
  <w:style w:type="character" w:styleId="Strong">
    <w:name w:val="Strong"/>
    <w:rPr><w:b/></w:rPr>
  </w:style>
</w:styles>"#;

    #[test]
    fn resolves_based_on_chain() {
        let (sheet, defaults) = parse_styles(STYLES).unwrap();
        assert_eq!(defaults.paragraph_id.as_deref(), Some("Normal"));

        let body = sheet.get("Body").unwrap();
        let para = body.para.as_ref().unwrap();
        assert_eq!(para.align, quire_model::Align::Center);
        assert!((para.indent_left_pt - 24.0).abs() < 1e-9);

        let char = body.char.as_ref().unwrap();
        assert!(char.bold, "inherited from Normal");
        assert!(char.italic, "direct on Body");
        assert!((char.size_pt - 10.0).abs() < 1e-9);
    }

    #[test]
    fn character_style_has_no_paragraph_props() {
        let (sheet, _) = parse_styles(STYLES).unwrap();
        let strong = sheet.get("Strong").unwrap();
        assert!(strong.para.is_none());
        assert!(strong.char.as_ref().unwrap().bold);
    }

    #[test]
    fn cycles_do_not_recurse_forever() {
        let xml = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
          <w:style w:type="paragraph" w:styleId="A"><w:basedOn w:val="B"/></w:style>
          <w:style w:type="paragraph" w:styleId="B"><w:basedOn w:val="A"/></w:style>
        </w:styles>"#;
        let (sheet, _) = parse_styles(xml).unwrap();
        assert!(sheet.get("A").is_some());
        assert!(sheet.get("B").is_some());
    }
}
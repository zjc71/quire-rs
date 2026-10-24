//! `word/numbering.xml` parsing.

use std::collections::BTreeMap;

use quick_xml::events::Event;
use quick_xml::Reader;
use quire_base::{QuireError, Result};
use quire_model::{
    AbstractNumbering, NumberFormat, NumberSuffix, Numbering, NumberingInstance, NumberingLevel,
};

use crate::xml::{attribute, local_name, twips_to_pt};

/// Parse the numbering part into the model.
pub fn parse_numbering(xml: &str) -> Result<Numbering> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;

    let mut numbering = Numbering::default();
    let mut abstract_numbering: Option<AbstractNumbering> = None;
    let mut level: Option<NumberingLevel> = None;
    let mut instance: Option<NumberingInstance> = None;
    let mut in_level_props = false;
    let mut override_level: Option<u8> = None;

    loop {
        let event = reader
            .read_event()
            .map_err(|err| QuireError::format(format!("numbering xml error: {err}")))?;

        match event {
            Event::Start(element) | Event::Empty(element) => {
                let name = local_name(element.name().as_ref()).to_vec();
                match name.as_slice() {
                    b"abstractNum" => {
                        if let Some(id) = attribute(&element, b"abstractNumId")
                            .and_then(|value| value.parse::<u32>().ok())
                        {
                            abstract_numbering = Some(AbstractNumbering {
                                id,
                                levels: BTreeMap::new(),
                            });
                        }
                    }
                    b"lvl" => {
                        let index = attribute(&element, b"ilvl")
                            .and_then(|value| value.parse::<u8>().ok())
                            .unwrap_or(0);
                        let left = 36.0 * f64::from(index + 1);
                        level = Some(NumberingLevel {
                            level: index,
                            indent_left_pt: left,
                            hanging_pt: 18.0,
                            text: format!("%{}.", index + 1),
                            ..NumberingLevel::default()
                        });
                    }
                    b"start" => {
                        if let (Some(value), Some(level)) = (
                            attribute(&element, b"val").and_then(|value| value.parse::<u32>().ok()),
                            level.as_mut(),
                        ) {
                            level.start = value;
                        }
                    }
                    b"numFmt" => {
                        if let (Some(value), Some(level)) =
                            (attribute(&element, b"val"), level.as_mut())
                        {
                            level.format = parse_format(&value);
                        }
                    }
                    b"lvlText" => {
                        if let (Some(value), Some(level)) =
                            (attribute(&element, b"val"), level.as_mut())
                        {
                            level.text = value;
                        }
                    }
                    b"suff" => {
                        if let (Some(value), Some(level)) =
                            (attribute(&element, b"val"), level.as_mut())
                        {
                            level.suffix = match value.as_str() {
                                "space" => NumberSuffix::Space,
                                "nothing" => NumberSuffix::Nothing,
                                _ => NumberSuffix::Tab,
                            };
                        }
                    }
                    b"pPr" => in_level_props = true,
                    b"ind" if in_level_props => {
                        if let Some(level) = level.as_mut() {
                            if let Some(value) = attribute(&element, b"left") {
                                if let Some(pt) = twips_to_pt(&value) {
                                    level.indent_left_pt = pt.max(0.0);
                                }
                            }
                            if let Some(value) = attribute(&element, b"hanging") {
                                if let Some(pt) = twips_to_pt(&value) {
                                    level.hanging_pt = pt.max(0.0);
                                }
                            }
                            if let Some(value) = attribute(&element, b"firstLine") {
                                if let Some(pt) = twips_to_pt(&value) {
                                    level.hanging_pt = (-pt).max(0.0);
                                }
                            }
                        }
                    }
                    b"num" => {
                        if let Some(id) =
                            attribute(&element, b"numId").and_then(|value| value.parse::<u32>().ok())
                        {
                            instance = Some(NumberingInstance {
                                id,
                                abstract_id: 0,
                                start_overrides: BTreeMap::new(),
                            });
                        }
                    }
                    b"lvlOverride" => {
                        override_level = attribute(&element, b"ilvl")
                            .and_then(|value| value.parse::<u8>().ok());
                    }
                    b"startOverride" => {
                        if let (Some(value), Some(level), Some(instance)) = (
                            attribute(&element, b"val").and_then(|value| value.parse::<u32>().ok()),
                            override_level,
                            instance.as_mut(),
                        ) {
                            instance.start_overrides.insert(level, value);
                        }
                    }
                    b"abstractNumId" => {
                        if let (Some(value), Some(instance)) = (
                            attribute(&element, b"val").and_then(|value| value.parse::<u32>().ok()),
                            instance.as_mut(),
                        ) {
                            instance.abstract_id = value;
                        }
                    }
                    _ => {}
                }
            }
            Event::End(element) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                match name {
                    b"pPr" => in_level_props = false,
                    b"lvlOverride" => override_level = None,
                    b"lvl" => {
                        if let (Some(level), Some(abstract_numbering)) =
                            (level.take(), abstract_numbering.as_mut())
                        {
                            abstract_numbering.levels.insert(level.level, level);
                        }
                    }
                    b"abstractNum" => {
                        if let Some(abstract_numbering) = abstract_numbering.take() {
                            numbering
                                .abstracts
                                .insert(abstract_numbering.id, abstract_numbering);
                        }
                    }
                    b"num" => {
                        if let Some(instance) = instance.take() {
                            numbering.instances.insert(instance.id, instance);
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(numbering)
}

fn parse_format(value: &str) -> NumberFormat {
    match value {
        "decimal" => NumberFormat::Decimal,
        "lowerLetter" => NumberFormat::LowerLetter,
        "upperLetter" => NumberFormat::UpperLetter,
        "lowerRoman" => NumberFormat::LowerRoman,
        "upperRoman" => NumberFormat::UpperRoman,
        "bullet" => NumberFormat::Bullet,
        "none" => NumberFormat::None,
        _ => NumberFormat::Decimal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NUMBERING: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:suff w:val="space"/><w:lvlText w:val="%1."/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl>
    <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%2)"/></w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;

    #[test]
    fn parses_levels_and_instances() {
        let numbering = parse_numbering(NUMBERING).unwrap();
        assert_eq!(numbering.instances.len(), 1);
        let instance = numbering.instances.get(&1).unwrap();
        assert_eq!(instance.abstract_id, 0);
        assert_eq!(numbering.start(1, 0), Some(1));
        let level = numbering.level(1, 0).unwrap();
        assert!((level.indent_left_pt - 36.0).abs() < 1e-9);
        assert!((level.hanging_pt - 18.0).abs() < 1e-9);
        assert_eq!(level.suffix, NumberSuffix::Space);
        assert_eq!(numbering.level(1, 1).unwrap().format, NumberFormat::LowerLetter);
    }
}

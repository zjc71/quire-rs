//! OpenDocument Text (`.odt`) import.
//!
//! This reader maps paragraphs, spans and basic automatic-style formatting.
//! Tables, frames, fields and notes are currently flattened or ignored.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::Reader;
use quire_base::{ParaId, QuireError, Result};
use quire_model::{Align, CharProps, Document, ParaProps, Paragraph, Section};

use crate::xml::{attribute, local_name};

fn read_part(bytes: &[u8], name: &str) -> Result<Option<String>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| QuireError::format(format!("cannot open odt: {error}")))?;
    let Ok(mut part) = archive.by_name(name) else {
        return Ok(None);
    };
    let mut text = String::new();
    part.read_to_string(&mut text)?;
    Ok(Some(text))
}

fn length_pt(value: &str) -> Option<f64> {
    let value = value.trim();
    let (number, unit) = value
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != '-')
        .map(|index| value.split_at(index))
        .unwrap_or((value, ""));
    let number = number.parse::<f64>().ok()?;
    Some(match unit.trim() {
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        "in" => number * 72.0,
        "pt" | "" => number,
        "pc" => number * 12.0,
        _ => return None,
    })
}

/// Read an ODT package.
pub fn read(bytes: &[u8]) -> Result<Document> {
    let content = read_part(bytes, "content.xml")?
        .ok_or_else(|| QuireError::format("odt has no content.xml"))?;
    parse_content(&content)
}

fn apply_char_style(props: &mut CharProps, element: &quick_xml::events::BytesStart<'_>) {
    if let Some(value) = attribute(element, b"font-weight") {
        props.bold = value == "bold" || value.parse::<u32>().map(|v| v >= 600).unwrap_or(false);
    }
    if let Some(value) = attribute(element, b"font-style") {
        props.italic = value == "italic" || value == "oblique";
    }
    if let Some(value) = attribute(element, b"text-underline-style") {
        props.underline = value != "none";
    }
    if let Some(value) = attribute(element, b"font-size") {
        if let Some(size) = length_pt(&value) {
            props.size_pt = size;
        }
    }
    if let Some(value) = attribute(element, b"color") {
        let value = value.trim_start_matches('#');
        if let Some(color) = quire_model::Color::from_hex(value) {
            props.color = color;
        }
    }
}

fn apply_para_style(props: &mut ParaProps, element: &quick_xml::events::BytesStart<'_>) {
    if let Some(value) = attribute(element, b"text-align") {
        props.align = match value.as_str() {
            "center" => Align::Center,
            "right" | "end" => Align::Right,
            "justify" => Align::Justify,
            _ => Align::Left,
        };
    }
    if let Some(value) = attribute(element, b"margin-left") {
        if let Some(size) = length_pt(&value) {
            props.indent_left_pt = size;
        }
    }
    if let Some(value) = attribute(element, b"text-indent") {
        if let Some(size) = length_pt(&value) {
            props.indent_first_pt = size;
        }
    }
    if let Some(value) = attribute(element, b"margin-top") {
        if let Some(size) = length_pt(&value) {
            props.space_before_pt = size;
        }
    }
    if let Some(value) = attribute(element, b"margin-bottom") {
        if let Some(size) = length_pt(&value) {
            props.space_after_pt = size;
        }
    }
    if let Some(value) = attribute(element, b"line-height") {
        let value = value.trim();
        if let Some(percent) = value.strip_suffix('%') {
            if let Ok(percent) = percent.trim().parse::<f64>() {
                if percent > 0.0 {
                    props.line_spacing = percent / 100.0;
                }
            }
        }
    }
}

fn parse_content(xml: &str) -> Result<Document> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;

    let mut char_styles: BTreeMap<String, CharProps> = BTreeMap::new();
    let mut para_styles: BTreeMap<String, ParaProps> = BTreeMap::new();
    let mut in_automatic_styles = false;
    let mut current_style: Option<String> = None;
    let mut paragraph: Option<Paragraph> = None;
    let mut char_stack: Vec<CharProps> = Vec::new();
    let mut current_char = CharProps::default();
    let mut paragraphs: Vec<Paragraph> = Vec::new();

    loop {
        let event = reader
            .read_event()
            .map_err(|error| QuireError::format(format!("odt xml error: {error}")))?;
        let is_empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(element) | Event::Empty(element) => {
                let name = local_name(element.name().as_ref()).to_vec();
                match name.as_slice() {
                    b"automatic-styles" => in_automatic_styles = true,
                    b"style" if in_automatic_styles => {
                        let id = attribute(&element, b"name").unwrap_or_default();
                        current_style = Some(id);
                    }
                    b"text-properties" if current_style.is_some() => {
                        if let Some(id) = &current_style {
                            let mut props = CharProps::default();
                            apply_char_style(&mut props, &element);
                            char_styles.insert(id.clone(), props);
                        }
                    }
                    b"paragraph-properties" if current_style.is_some() => {
                        if let Some(id) = &current_style {
                            let mut props = ParaProps::default();
                            apply_para_style(&mut props, &element);
                            para_styles.insert(id.clone(), props);
                        }
                    }
                    b"p" | b"h" => {
                        if !in_automatic_styles {
                            let style = attribute(&element, b"style-name");
                            let mut props = ParaProps::default();
                            if let Some(style) = &style {
                                if let Some(style_props) = para_styles.get(style) {
                                    props = style_props.clone();
                                }
                            }
                            let mut paragraph_value = Paragraph::new(ParaId::new(0));
                            paragraph_value.props = props;
                            if is_empty {
                                paragraphs.push(paragraph_value);
                            } else {
                                paragraph = Some(paragraph_value);
                            }
                        }
                    }
                    b"span" if !in_automatic_styles => {
                        char_stack.push(current_char.clone());
                        if let Some(style) = attribute(&element, b"style-name") {
                            if let Some(props) = char_styles.get(&style) {
                                current_char = props.clone();
                            }
                        }
                    }
                    b"tab" if !in_automatic_styles => {
                        if let Some(paragraph) = paragraph.as_mut() {
                            paragraph.push_text("\t", current_char.clone());
                        }
                    }
                    b"line-break" if !in_automatic_styles => {
                        if let Some(paragraph) = paragraph.as_mut() {
                            paragraph.push_text("\n", current_char.clone());
                        }
                    }
                    b"s" if !in_automatic_styles => {
                        let count = attribute(&element, b"c")
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(1);
                        if let Some(paragraph) = paragraph.as_mut() {
                            paragraph.push_text(" ".repeat(count), current_char.clone());
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(text) => {
                if in_automatic_styles || paragraph.is_none() {
                    continue;
                }
                let value = text
                    .unescape()
                    .map_err(|error| QuireError::format(format!("odt text error: {error}")))?;
                if let Some(paragraph) = paragraph.as_mut() {
                    paragraph.push_text(value.as_ref(), current_char.clone());
                }
            }
            Event::End(element) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                match name {
                    b"automatic-styles" => in_automatic_styles = false,
                    b"style" => current_style = None,
                    b"span" => {
                        if let Some(previous) = char_stack.pop() {
                            current_char = previous;
                        }
                    }
                    b"p" | b"h" => {
                        if let Some(paragraph) = paragraph.take() {
                            paragraphs.push(paragraph);
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let mut document = Document::new();
    document.clear_sections();
    let mut section = Section::default();
    for paragraph in paragraphs {
        section.push_paragraph_block(paragraph);
    }
    document.push_section(section);
    document.renumber();
    document.validate()?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn package(content: &str) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("mimetype", options).unwrap();
            zip.write_all(b"application/vnd.oasis.opendocument.text").unwrap();
            zip.start_file("content.xml", options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn reads_paragraphs_and_text_styles() {
        let content = r#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
<office:automatic-styles>
 <style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold" fo:font-size="14pt"/></style:style>
 <style:style style:name="P1" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>
</office:automatic-styles>
<office:body><office:text>
<text:p text:style-name="P1">Hello <text:span text:style-name="T1">World</text:span></text:p>
<text:p>Second</text:p>
</office:text></office:body></office:document-content>"#;
        let doc = read(&package(content)).unwrap();
        assert_eq!(doc.paragraph_count(), 2);
        assert_eq!(doc.paragraph(0, 0).unwrap().text(), "Hello World");
        assert_eq!(doc.paragraph(0, 0).unwrap().props.align, Align::Center);
        assert!(doc.paragraph(0, 0).unwrap().runs.iter().any(|run| run.props.bold));
    }
}

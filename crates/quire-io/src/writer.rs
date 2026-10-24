//! WordprocessingML writer.
//!
//! Every section is written: non-final sections place their `w:sectPr` in the
//! last paragraph (the section break), the final one at body level. The writer
//! emits explicit properties (styles are resolved before they reach the model),
//! so a consumer that ignores `styles.xml` still sees the correct formatting.

use std::collections::BTreeMap;
use std::io::{Seek, Write};

use quire_base::xml::escape_xml;
use quire_base::{ImageId, QuireError, Result};
use quire_model::{
    Align, Block, Border, CellBorders, CharProps, Color, Document, ImageBlock, NumberFormat,
    NumberSuffix, PackageRelationship, ParaProps, Paragraph, Section, Style, StyleKind, Table,
    TableBorders, TableCell, VMerge,
};

use crate::opc::extension_for_mime;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PIC_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

struct Media {
    zip_path: String,
    bytes: Vec<u8>,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum StoryKind {
    Header,
    Footer,
}

struct StorySpec<'a> {
    section: usize,
    kind: StoryKind,
    variant: &'static str,
    part: String,
    root: &'static str,
    blocks: &'a [Block],
    relationship_id: String,
}

/// Write a document as a `.docx` package.
///
/// Refuses when the document contains content this milestone cannot write back,
/// so a successful save never silently drops data.
pub fn write<W: Write + Seek>(writer: W, document: &Document) -> Result<()> {
    write_package(writer, document, false)
}

/// Write even when unsupported content is present. Diagnostic use only.
pub fn write_force<W: Write + Seek>(writer: W, document: &Document) -> Result<()> {
    write_package(writer, document, true)
}

fn write_package<W: Write + Seek>(
    writer: W,
    document: &Document,
    allow_lossy: bool,
) -> Result<()> {
    document.validate()?;
    if !allow_lossy && !document.fidelity().is_lossless() {
        return Err(QuireError::unsupported(format!(
            "document uses content that cannot be written back yet: {}",
            document.fidelity().summary()
        )));
    }

    let sections = document.sections();
    let kept_relationships: Vec<PackageRelationship> = document
        .package()
        .document_relationships
        .iter()
        .filter(|relationship| !owned_relationship(&relationship.kind))
        .cloned()
        .collect();
    let base_id = kept_relationships
        .iter()
        .filter_map(|relationship| relationship_number(&relationship.id))
        .max()
        .unwrap_or(0)
        + 1;
    let mut specs: Vec<StorySpec<'_>> = Vec::new();
    let mut header_parts = 0usize;
    let mut footer_parts = 0usize;

    for (section_index, section) in sections.iter().enumerate() {
        let headers = [
            ("default", section.headers.default.as_ref()),
            ("first", section.headers.first.as_ref()),
            ("even", section.headers.even.as_ref()),
        ];
        for (variant, header) in headers {
            if let Some(header) = header {
                header_parts += 1;
                let part = format!("header{header_parts}.xml");
                let relationship_id = format!("rId{}", base_id + 3 + specs.len() as u32);
                specs.push(StorySpec {
                    section: section_index,
                    kind: StoryKind::Header,
                    variant,
                    part,
                    root: "hdr",
                    blocks: &header.blocks,
                    relationship_id,
                });
            }
        }

        let footers = [
            ("default", section.footers.default.as_ref()),
            ("first", section.footers.first.as_ref()),
            ("even", section.footers.even.as_ref()),
        ];
        for (variant, footer) in footers {
            if let Some(footer) = footer {
                footer_parts += 1;
                let part = format!("footer{footer_parts}.xml");
                let relationship_id = format!("rId{}", base_id + 3 + specs.len() as u32);
                specs.push(StorySpec {
                    section: section_index,
                    kind: StoryKind::Footer,
                    variant,
                    part,
                    root: "ftr",
                    blocks: &footer.blocks,
                    relationship_id,
                });
            }
        }
    }

    let mut media: Vec<Media> = Vec::new();
    let mut media_targets: BTreeMap<ImageId, String> = BTreeMap::new();
    let mut stories: Vec<&[Block]> = sections
        .iter()
        .map(|section| section.blocks.as_slice())
        .collect();
    stories.extend(specs.iter().map(|spec| spec.blocks));
    for blocks in stories {
        let mut image_blocks: Vec<&ImageBlock> = Vec::new();
        collect_images(blocks, &mut image_blocks);
        for image in image_blocks {
            if media_targets.contains_key(&image.id) {
                continue;
            }
            let index = media.len() + 1;
            let extension = extension_for_mime(&image.mime);
            let target = format!("media/image{index}.{extension}");
            media_targets.insert(image.id, target.clone());
            media.push(Media {
                zip_path: format!("word/{target}"),
                bytes: image.data.to_vec(),
            });
        }
    }

    let mut body_images: BTreeMap<ImageId, String> = BTreeMap::new();
    let mut body_image_start = base_id + 3 + specs.len() as u32;
    for section in sections {
        let before = body_images.len();
        assign_image_relationships(&section.blocks, body_image_start, &mut body_images);
        body_image_start += (body_images.len() - before) as u32;
    }

    let mut spec_images: Vec<BTreeMap<ImageId, String>> = Vec::with_capacity(specs.len());
    for spec in &specs {
        let mut map = BTreeMap::new();
        assign_image_relationships(spec.blocks, 3, &mut map);
        spec_images.push(map);
    }

    let mut zip = zip::ZipWriter::new(writer);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("[Content_Types].xml", options)
        .map_err(zip_error)?;
    zip.write_all(content_types(document, &media, &specs).as_bytes())?;

    zip.start_file("_rels/.rels", options).map_err(zip_error)?;
    zip.write_all(package_rels(document).as_bytes())?;

    zip.start_file("word/_rels/document.xml.rels", options)
        .map_err(zip_error)?;
    zip.write_all(
        document_rels(
            &kept_relationships,
            base_id,
            &specs,
            &body_images,
            &media_targets,
        )
        .as_bytes(),
    )?;

    zip.start_file("word/document.xml", options).map_err(zip_error)?;
    zip.write_all(document_xml(document, &body_images, &specs).as_bytes())?;

    zip.start_file("word/styles.xml", options).map_err(zip_error)?;
    zip.write_all(styles_xml(document).as_bytes())?;

    zip.start_file("word/numbering.xml", options)
        .map_err(zip_error)?;
    zip.write_all(numbering_xml(document).as_bytes())?;

    zip.start_file("word/settings.xml", options)
        .map_err(zip_error)?;
    zip.write_all(settings_xml(document).as_bytes())?;

    for (spec, images) in specs.iter().zip(&spec_images) {
        zip.start_file(format!("word/{}", spec.part), options)
            .map_err(zip_error)?;
        zip.write_all(part_xml(spec, images).as_bytes())?;

        zip.start_file(format!("word/_rels/{}.rels", spec.part), options)
            .map_err(zip_error)?;
        zip.write_all(part_rels(images, &media_targets).as_bytes())?;
    }

    for item in &media {
        zip.start_file(&item.zip_path, options).map_err(zip_error)?;
        zip.write_all(&item.bytes)?;
    }

    for (name, bytes) in &document.package().parts {
        zip.start_file(name.as_str(), options).map_err(zip_error)?;
        zip.write_all(bytes)?;
    }

    zip.finish().map_err(zip_error)?;
    Ok(())
}

fn assign_image_relationships(
    blocks: &[Block],
    start: u32,
    out: &mut BTreeMap<ImageId, String>,
) {
    let mut images: Vec<&ImageBlock> = Vec::new();
    collect_images(blocks, &mut images);
    let mut seen: Vec<ImageId> = Vec::new();
    for image in images {
        if seen.contains(&image.id) || out.contains_key(&image.id) {
            continue;
        }
        seen.push(image.id);
        out.insert(
            image.id,
            format!("rId{}", start + seen.len() as u32 - 1),
        );
    }
}

/// Build a `.docx` package in memory.
pub fn bytes(document: &Document) -> Result<Vec<u8>> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    write(&mut buffer, document)?;
    Ok(buffer.into_inner())
}

/// Build a `.docx` package in memory even when unsupported content is present.
pub fn bytes_force(document: &Document) -> Result<Vec<u8>> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    write_force(&mut buffer, document)?;
    Ok(buffer.into_inner())
}

fn zip_error(error: zip::result::ZipError) -> QuireError {
    QuireError::format(format!("cannot write package: {error}"))
}

fn content_types(document: &Document, media: &[Media], specs: &[StorySpec<'_>]) -> String {
    let mut extensions: Vec<&str> = media
        .iter()
        .map(|item| item.zip_path.rsplit('.').next().unwrap_or("bin"))
        .collect();
    extensions.sort_unstable();
    extensions.dedup();

    let mut defaults = document.package().content_types.defaults.clone();
    defaults.insert(
        "rels".to_string(),
        "application/vnd.openxmlformats-package.relationships+xml".to_string(),
    );
    defaults.insert("xml".to_string(), "application/xml".to_string());
    for extension in &extensions {
        defaults.insert(
            (*extension).to_string(),
            mime_for_extension(extension).to_string(),
        );
    }

    let mut overrides = document.package().content_types.overrides.clone();
    overrides.retain(|part, _| !owned_part(part));
    overrides.insert(
        "/word/document.xml".to_string(),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            .to_string(),
    );
    overrides.insert(
        "/word/styles.xml".to_string(),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml".to_string(),
    );
    overrides.insert(
        "/word/numbering.xml".to_string(),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml".to_string(),
    );
    overrides.insert(
        "/word/settings.xml".to_string(),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml".to_string(),
    );
    for spec in specs {
        let content_type = match spec.kind {
            StoryKind::Header => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"
            }
            StoryKind::Footer => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"
            }
        };
        overrides.insert(
            format!("/word/{}", spec.part),
            content_type.to_string(),
        );
    }

    let mut xml = String::from(XML_DECL);
    xml.push_str(
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
    );
    for (extension, content_type) in &defaults {
        xml.push_str(&format!(
            "<Default Extension=\"{}\" ContentType=\"{}\"/>",
            escape_xml(extension),
            escape_xml(content_type)
        ));
    }
    for (part, content_type) in &overrides {
        xml.push_str(&format!(
            "<Override PartName=\"{}\" ContentType=\"{}\"/>",
            escape_xml(part),
            escape_xml(content_type)
        ));
    }
    xml.push_str("</Types>");
    xml
}

fn mime_for_extension(extension: &str) -> &'static str {
    match extension {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "emf" => "image/emf",
        "wmf" => "image/wmf",
        _ => "application/octet-stream",
    }
}

fn package_rels(document: &Document) -> String {
    let kept: Vec<&PackageRelationship> = document
        .package()
        .package_relationships
        .iter()
        .filter(|relationship| !relationship.kind.ends_with("/officeDocument"))
        .collect();
    let document_id = kept
        .iter()
        .filter_map(|relationship| relationship_number(&relationship.id))
        .max()
        .unwrap_or(0)
        + 1;

    let mut xml = String::from(XML_DECL);
    xml.push_str(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for relationship in kept {
        xml.push_str(&format!(
            "<Relationship Id=\"{}\" Type=\"{}\" Target=\"{}\"{}/>",
            escape_xml(&relationship.id),
            escape_xml(&relationship.kind),
            escape_xml(&relationship.target),
            if relationship.external {
                " TargetMode=\"External\""
            } else {
                ""
            }
        ));
    }
    xml.push_str(&format!(
        "<Relationship Id=\"rId{document_id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>"
    ));
    xml.push_str("</Relationships>");
    xml
}

fn document_rels(
    kept: &[PackageRelationship],
    base_id: u32,
    specs: &[StorySpec<'_>],
    body_images: &BTreeMap<ImageId, String>,
    media_targets: &BTreeMap<ImageId, String>,
) -> String {
    let mut xml = String::from(XML_DECL);
    xml.push_str(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for relationship in kept {
        xml.push_str(&format!(
            "<Relationship Id=\"{}\" Type=\"{}\" Target=\"{}\"{}/>",
            escape_xml(&relationship.id),
            escape_xml(&relationship.kind),
            escape_xml(&relationship.target),
            if relationship.external {
                " TargetMode=\"External\""
            } else {
                ""
            }
        ));
    }
    xml.push_str(&format!(
        "<Relationship Id=\"rId{base_id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>"
    ));
    xml.push_str(&format!(
        "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>",
        base_id + 1
    ));
    xml.push_str(&format!(
        "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings\" Target=\"settings.xml\"/>",
        base_id + 2
    ));
    for spec in specs {
        let relationship_type = match spec.kind {
            StoryKind::Header => {
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header"
            }
            StoryKind::Footer => {
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer"
            }
        };
        xml.push_str(&format!(
            "<Relationship Id=\"{}\" Type=\"{}\" Target=\"{}\"/>",
            escape_xml(&spec.relationship_id),
            relationship_type,
            escape_xml(&spec.part)
        ));
    }
    for (id, relationship_id) in body_images {
        let Some(target) = media_targets.get(id) else {
            continue;
        };
        xml.push_str(&format!(
            "<Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"{}\"/>",
            escape_xml(relationship_id),
            escape_xml(target)
        ));
    }
    xml.push_str("</Relationships>");
    xml
}


fn owned_relationship(kind: &str) -> bool {
    kind.ends_with("/styles")
        || kind.ends_with("/numbering")
        || kind.ends_with("/settings")
        || kind.ends_with("/header")
        || kind.ends_with("/footer")
        || kind.ends_with("/image")
        || kind.ends_with("/officeDocument")
}

fn relationship_number(id: &str) -> Option<u32> {
    id.strip_prefix("rId")
        .and_then(|value| value.parse::<u32>().ok())
}

fn owned_part(part: &str) -> bool {
    matches!(
        part,
        "/word/document.xml"
            | "/word/styles.xml"
            | "/word/numbering.xml"
            | "/word/settings.xml"
    ) || part.starts_with("/word/header")
        || part.starts_with("/word/footer")
}

fn part_rels(images: &BTreeMap<ImageId, String>, media_targets: &BTreeMap<ImageId, String>) -> String {
    let mut xml = String::from(XML_DECL);
    xml.push_str(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    xml.push_str(
        "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>",
    );
    xml.push_str(
        "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>",
    );
    for (id, relationship_id) in images {
        let Some(target) = media_targets.get(id) else {
            continue;
        };
        xml.push_str(&format!(
            "<Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"{}\"/>",
            escape_xml(relationship_id),
            escape_xml(target)
        ));
    }
    xml.push_str("</Relationships>");
    xml
}

fn document_xml(
    document: &Document,
    images: &BTreeMap<ImageId, String>,
    specs: &[StorySpec<'_>],
) -> String {
    let sections = document.sections();
    let mut xml = String::from(XML_DECL);
    xml.push_str(&format!(
        "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:wp=\"{WP_NS}\" xmlns:a=\"{A_NS}\" xmlns:pic=\"{PIC_NS}\"><w:body>"
    ));

    let mut picture_id = 0u32;
    let last = sections.len().saturating_sub(1);
    for (index, section) in sections.iter().enumerate() {
        let sect = section_properties(section, index, specs);
        if index == last {
            write_blocks(&mut xml, &section.blocks, images, &mut picture_id);
            xml.push_str(&sect);
        } else {
            write_section_blocks(&mut xml, &section.blocks, images, &mut picture_id, &sect);
        }
    }

    xml.push_str("</w:body></w:document>");
    xml
}

fn section_properties(section: &Section, index: usize, specs: &[StorySpec<'_>]) -> String {
    let props = section.props;
    let mut xml = String::from("<w:sectPr>");
    for spec in specs.iter().filter(|spec| spec.section == index) {
        let tag = match spec.kind {
            StoryKind::Header => "headerReference",
            StoryKind::Footer => "footerReference",
        };
        xml.push_str(&format!(
            "<w:{tag} w:type=\"{}\" r:id=\"{}\"/>",
            spec.variant,
            escape_xml(&spec.relationship_id)
        ));
    }
    xml.push_str(&format!(
        "<w:pgSz w:w=\"{}\" w:h=\"{}\"/>",
        (props.page.width_pt * 20.0).round() as i64,
        (props.page.height_pt * 20.0).round() as i64
    ));
    xml.push_str(&format!(
        "<w:pgMar w:top=\"{}\" w:right=\"{}\" w:bottom=\"{}\" w:left=\"{}\"/>",
        (props.margins.top_pt * 20.0).round() as i64,
        (props.margins.right_pt * 20.0).round() as i64,
        (props.margins.bottom_pt * 20.0).round() as i64,
        (props.margins.left_pt * 20.0).round() as i64
    ));
    if section.title_page {
        xml.push_str("<w:titlePg/>");
    }
    xml.push_str("</w:sectPr>");
    xml
}

fn write_section_blocks(
    xml: &mut String,
    blocks: &[Block],
    images: &BTreeMap<ImageId, String>,
    picture_id: &mut u32,
    sect: &str,
) {
    let count = blocks.len();
    let ends_with_paragraph = matches!(blocks.last(), Some(Block::Paragraph(_)));
    for (index, block) in blocks.iter().enumerate() {
        match block {
            Block::Paragraph(paragraph) => {
                let trailing = if ends_with_paragraph && index + 1 == count {
                    Some(sect)
                } else {
                    None
                };
                write_paragraph_with_sect(xml, paragraph, trailing);
            }
            Block::Image(image) => {
                *picture_id += 1;
                write_image(xml, image, images.get(&image.id), *picture_id);
            }
            Block::Table(table) => write_table(xml, table, images, picture_id),
        }
    }
    if !ends_with_paragraph {
        xml.push_str("<w:p><w:pPr>");
        xml.push_str(sect);
        xml.push_str("</w:pPr></w:p>");
    }
}

fn settings_xml(document: &Document) -> String {
    let mut xml = String::from(XML_DECL);
    xml.push_str(&format!("<w:settings xmlns:w=\"{W_NS}\">"));
    if document.even_and_odd_headers() {
        xml.push_str("<w:evenAndOddHeaders/>");
    }
    xml.push_str("</w:settings>");
    xml
}

fn part_xml(spec: &StorySpec<'_>, images: &BTreeMap<ImageId, String>) -> String {
    let mut xml = String::from(XML_DECL);
    xml.push_str(&format!(
        "<w:{} xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:wp=\"{WP_NS}\" xmlns:a=\"{A_NS}\" xmlns:pic=\"{PIC_NS}\">",
        spec.root
    ));
    let mut picture_id = 0u32;
    write_blocks(&mut xml, spec.blocks, images, &mut picture_id);
    xml.push_str(&format!("</w:{}>", spec.root));
    xml
}

fn write_blocks(
    xml: &mut String,
    blocks: &[Block],
    images: &BTreeMap<ImageId, String>,
    picture_id: &mut u32,
) {
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => write_paragraph(xml, paragraph),
            Block::Image(image) => {
                *picture_id += 1;
                write_image(xml, image, images.get(&image.id), *picture_id);
            }
            Block::Table(table) => write_table(xml, table, images, picture_id),
        }
    }
}

fn write_paragraph(xml: &mut String, paragraph: &Paragraph) {
    write_paragraph_with_sect(xml, paragraph, None);
}

fn write_paragraph_with_sect(xml: &mut String, paragraph: &Paragraph, sect: Option<&str>) {
    xml.push_str("<w:p>");

    let mut paragraph_props = String::new();
    if let Some(style) = &paragraph.style {
        paragraph_props.push_str(&format!(
            "<w:pStyle w:val=\"{}\"/>",
            escape_xml(style)
        ));
    }
    if let Some(numbering) = &paragraph.numbering {
        paragraph_props.push_str(&format!(
            "<w:numPr><w:ilvl w:val=\"{}\"/><w:numId w:val=\"{}\"/></w:numPr>",
            numbering.level, numbering.num_id
        ));
    }
    write_para_props(&mut paragraph_props, &paragraph.props);
    if let Some(sect) = sect {
        paragraph_props.push_str(sect);
    }
    if !paragraph_props.is_empty() {
        xml.push_str("<w:pPr>");
        xml.push_str(&paragraph_props);
        xml.push_str("</w:pPr>");
    }

    for run in &paragraph.runs {
        if let Some(kind) = run.field {
            xml.push_str(&format!(
                "<w:fldSimple w:instr=\"{}\">",
                kind.instruction()
            ));
            write_run(xml, run);
            xml.push_str("</w:fldSimple>");
        } else {
            write_run(xml, run);
        }
    }

    xml.push_str("</w:p>");
}

fn write_run(xml: &mut String, run: &quire_model::Run) {
    xml.push_str("<w:r>");
    let mut run_props = String::new();
    if let Some(style) = &run.style {
        run_props.push_str(&format!("<w:rStyle w:val=\"{}\"/>", escape_xml(style)));
    }
    write_char_props(&mut run_props, &run.props);
    if !run_props.is_empty() {
        xml.push_str("<w:rPr>");
        xml.push_str(&run_props);
        xml.push_str("</w:rPr>");
    }

    let mut pieces = run.text.split('\n').peekable();
    while let Some(piece) = pieces.next() {
        if !piece.is_empty() {
            xml.push_str("<w:t xml:space=\"preserve\">");
            xml.push_str(&escape_xml(piece));
            xml.push_str("</w:t>");
        }
        if pieces.peek().is_some() {
            xml.push_str("<w:br/>");
        }
    }
    xml.push_str("</w:r>");
}

fn write_para_props(xml: &mut String, props: &ParaProps) {
    match props.align {
        Align::Left => {}
        Align::Center => xml.push_str("<w:jc w:val=\"center\"/>"),
        Align::Right => xml.push_str("<w:jc w:val=\"right\"/>"),
        Align::Justify => xml.push_str("<w:jc w:val=\"both\"/>"),
    }
    if props.indent_left_pt != 0.0
        || props.indent_right_pt != 0.0
        || props.indent_first_pt != 0.0
    {
        xml.push_str(&format!(
            "<w:ind w:left=\"{}\" w:right=\"{}\" w:firstLine=\"{}\"/>",
            (props.indent_left_pt * 20.0).round() as i64,
            (props.indent_right_pt * 20.0).round() as i64,
            (props.indent_first_pt * 20.0).round() as i64
        ));
    }
    if props.space_before_pt != 0.0 || props.space_after_pt != 0.0 || props.line_spacing != 1.0 {
        xml.push_str(&format!(
            "<w:spacing w:before=\"{}\" w:after=\"{}\" w:line=\"{}\" w:lineRule=\"auto\"/>",
            (props.space_before_pt * 20.0).round() as i64,
            (props.space_after_pt * 20.0).round() as i64,
            (props.line_spacing * 240.0).round() as i64
        ));
    }
}

fn write_char_props(xml: &mut String, props: &CharProps) {
    if props.font_family.is_some() || props.font_family_east_asian.is_some() {
        let ascii = props.font_family.as_deref().unwrap_or("");
        let east_asian = props.font_family_east_asian.as_deref().unwrap_or(ascii);
        xml.push_str(&format!(
            "<w:rFonts w:ascii=\"{}\" w:hAnsi=\"{}\" w:eastAsia=\"{}\" w:cs=\"{}\" w:hint=\"eastAsia\"/>",
            escape_xml(ascii),
            escape_xml(ascii),
            escape_xml(east_asian),
            escape_xml(ascii)
        ));
    }
    if props.bold {
        xml.push_str("<w:b/>");
    }
    if props.bold_complex {
        xml.push_str("<w:bCs/>");
    }
    if props.italic {
        xml.push_str("<w:i/>");
    }
    if props.italic_complex {
        xml.push_str("<w:iCs/>");
    }
    if props.color != Color::BLACK {
        xml.push_str(&format!("<w:color w:val=\"{}\"/>", props.color.to_hex()));
    }
    if (props.size_pt - quire_model::DEFAULT_FONT_SIZE_PT).abs() > f64::EPSILON {
        xml.push_str(&format!(
            "<w:sz w:val=\"{}\"/>",
            (props.size_pt * 2.0).round() as i64
        ));
    }
    if let Some(size) = props.size_complex_pt {
        xml.push_str(&format!(
            "<w:szCs w:val=\"{}\"/>",
            (size * 2.0).round() as i64
        ));
    }
    if props.underline {
        xml.push_str("<w:u w:val=\"single\"/>");
    }
    if props.language.is_some() || props.language_east_asian.is_some() {
        let language = props.language.as_deref().unwrap_or("en-US");
        let east_asian = props.language_east_asian.as_deref().unwrap_or(language);
        xml.push_str(&format!(
            "<w:lang w:val=\"{}\" w:eastAsia=\"{}\"/>",
            escape_xml(language),
            escape_xml(east_asian)
        ));
    }
}

fn write_image(
    xml: &mut String,
    image: &ImageBlock,
    relationship: Option<&String>,
    picture_id: u32,
) {
    let Some(relationship) = relationship else {
        return;
    };
    let cx = (image.width_pt * 12_700.0).round() as i64;
    let cy = (image.height_pt * 12_700.0).round() as i64;
    let name = escape_xml(&image.name);

    xml.push_str("<w:p><w:r><w:drawing>");
    xml.push_str("<wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">");
    xml.push_str(&format!("<wp:extent cx=\"{cx}\" cy=\"{cy}\"/>"));
    xml.push_str(&format!(
        "<wp:docPr id=\"{picture_id}\" name=\"Picture {picture_id}\"/>"
    ));
    xml.push_str(
        "<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic>",
    );
    xml.push_str(&format!(
        "<pic:nvPicPr><pic:cNvPr id=\"{picture_id}\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr>"
    ));
    xml.push_str(&format!(
        "<pic:blipFill><a:blip r:embed=\"{}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>",
        escape_xml(relationship)
    ));
    xml.push_str(&format!(
        "<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>"
    ));
    xml.push_str("</pic:pic></a:graphicData></a:graphic>");
    xml.push_str("</wp:inline></w:drawing></w:r></w:p>");
}


fn collect_images<'a>(blocks: &'a [Block], out: &mut Vec<&'a ImageBlock>) {
    for block in blocks {
        match block {
            Block::Paragraph(_) => {}
            Block::Image(image) => out.push(image),
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        collect_images(&cell.blocks, out);
                    }
                }
            }
        }
    }
}

fn write_table(
    xml: &mut String,
    table: &Table,
    images: &BTreeMap<ImageId, String>,
    picture_id: &mut u32,
) {
    xml.push_str("<w:tbl><w:tblPr>");
    if let Some(style) = &table.style {
        xml.push_str(&format!(
            "<w:tblStyle w:val=\"{}\"/>",
            escape_xml(style)
        ));
    }
    write_table_borders(xml, &table.borders);
    xml.push_str("</w:tblPr><w:tblGrid>");

    let columns = table.column_count().max(1);
    for index in 0..columns {
        let width_pt = table.grid.get(index).copied().unwrap_or(72.0);
        xml.push_str(&format!(
            "<w:gridCol w:w=\"{}\"/>",
            (width_pt * 20.0).round().max(0.0) as i64
        ));
    }
    xml.push_str("</w:tblGrid>");

    for row in &table.rows {
        xml.push_str("<w:tr>");
        if row.header || row.height_pt.is_some() {
            xml.push_str("<w:trPr>");
            if row.header {
                xml.push_str("<w:tblHeader/>");
            }
            if let Some(height) = row.height_pt {
                xml.push_str(&format!(
                    "<w:trHeight w:val=\"{}\" w:hRule=\"atLeast\"/>",
                    (height * 20.0).round().max(0.0) as i64
                ));
            }
            xml.push_str("</w:trPr>");
        }
        for cell in &row.cells {
            write_cell(xml, cell, images, picture_id);
        }
        xml.push_str("</w:tr>");
    }

    xml.push_str("</w:tbl>");
}

fn write_cell(
    xml: &mut String,
    cell: &TableCell,
    images: &BTreeMap<ImageId, String>,
    picture_id: &mut u32,
) {
    xml.push_str("<w:tc><w:tcPr>");
    if cell.span > 1 {
        xml.push_str(&format!("<w:gridSpan w:val=\"{}\"/>", cell.span));
    }
    match cell.vmerge {
        VMerge::None => {}
        VMerge::Restart => xml.push_str("<w:vMerge w:val=\"restart\"/>"),
        VMerge::Continue => xml.push_str("<w:vMerge/>"),
    }
    write_cell_borders(xml, &cell.borders);
    if let Some(color) = cell.shading {
        xml.push_str(&format!(
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>",
            color.to_hex()
        ));
    }
    if cell.padding_pt > 0.0 {
        let width = (cell.padding_pt * 20.0).round() as i64;
        xml.push_str("<w:tcMar>");
        for side in ["top", "left", "bottom", "right"] {
            xml.push_str(&format!(
                "<w:{side} w:w=\"{width}\" w:type=\"dxa\"/>"
            ));
        }
        xml.push_str("</w:tcMar>");
    }
    xml.push_str("</w:tcPr>");

    for block in &cell.blocks {
        match block {
            Block::Paragraph(paragraph) => write_paragraph(xml, paragraph),
            Block::Image(image) => {
                *picture_id += 1;
                write_image(xml, image, images.get(&image.id), *picture_id);
            }
            Block::Table(table) => write_table(xml, table, images, picture_id),
        }
    }

    xml.push_str("</w:tc>");
}

fn write_table_borders(xml: &mut String, borders: &TableBorders) {
    if borders.top.is_none()
        && borders.right.is_none()
        && borders.bottom.is_none()
        && borders.left.is_none()
        && borders.inside_horizontal.is_none()
        && borders.inside_vertical.is_none()
    {
        return;
    }
    xml.push_str("<w:tblBorders>");
    write_border(xml, "top", borders.top);
    write_border(xml, "left", borders.left);
    write_border(xml, "bottom", borders.bottom);
    write_border(xml, "right", borders.right);
    write_border(xml, "insideH", borders.inside_horizontal);
    write_border(xml, "insideV", borders.inside_vertical);
    xml.push_str("</w:tblBorders>");
}

fn write_cell_borders(xml: &mut String, borders: &CellBorders) {
    if borders.top.is_none()
        && borders.right.is_none()
        && borders.bottom.is_none()
        && borders.left.is_none()
    {
        return;
    }
    xml.push_str("<w:tcBorders>");
    write_border(xml, "top", borders.top);
    write_border(xml, "left", borders.left);
    write_border(xml, "bottom", borders.bottom);
    write_border(xml, "right", borders.right);
    xml.push_str("</w:tcBorders>");
}

fn write_border(xml: &mut String, name: &str, border: Option<Border>) {
    let Some(border) = border else {
        return;
    };
    let size = (border.width_pt * 8.0).round().max(2.0) as i64;
    xml.push_str(&format!(
        "<w:{name} w:val=\"single\" w:sz=\"{size}\" w:space=\"0\" w:color=\"{}\"/>",
        border.color.to_hex()
    ));
}

fn numbering_xml(document: &Document) -> String {
    let numbering = document.numbering();
    let mut xml = String::from(XML_DECL);
    xml.push_str(&format!("<w:numbering xmlns:w=\"{W_NS}\">"));

    for abstract_numbering in numbering.abstracts.values() {
        xml.push_str(&format!(
            "<w:abstractNum w:abstractNumId=\"{}\">",
            abstract_numbering.id
        ));
        for level in abstract_numbering.levels.values() {
            xml.push_str(&format!("<w:lvl w:ilvl=\"{}\">", level.level));
            xml.push_str(&format!("<w:start w:val=\"{}\"/>", level.start));
            xml.push_str(&format!(
                "<w:numFmt w:val=\"{}\"/>",
                format_name(level.format)
            ));
            xml.push_str(&format!(
                "<w:suff w:val=\"{}\"/>",
                suffix_name(level.suffix)
            ));
            xml.push_str(&format!(
                "<w:lvlText w:val=\"{}\"/>",
                escape_xml(&level.text)
            ));
            xml.push_str(&format!(
                "<w:pPr><w:ind w:left=\"{}\" w:hanging=\"{}\"/></w:pPr>",
                (level.indent_left_pt * 20.0).round().max(0.0) as i64,
                (level.hanging_pt * 20.0).round().max(0.0) as i64
            ));
            xml.push_str("</w:lvl>");
        }
        xml.push_str("</w:abstractNum>");
    }

    for instance in numbering.instances.values() {
        xml.push_str(&format!(
            "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"{}\"/>",
            instance.id, instance.abstract_id
        ));
        for (level, start) in &instance.start_overrides {
            xml.push_str(&format!(
                "<w:lvlOverride w:ilvl=\"{level}\"><w:startOverride w:val=\"{start}\"/></w:lvlOverride>"
            ));
        }
        xml.push_str("</w:num>");
    }

    xml.push_str("</w:numbering>");
    xml
}

fn format_name(format: NumberFormat) -> &'static str {
    match format {
        NumberFormat::Decimal => "decimal",
        NumberFormat::LowerLetter => "lowerLetter",
        NumberFormat::UpperLetter => "upperLetter",
        NumberFormat::LowerRoman => "lowerRoman",
        NumberFormat::UpperRoman => "upperRoman",
        NumberFormat::Bullet => "bullet",
        NumberFormat::None => "none",
    }
}

fn suffix_name(suffix: NumberSuffix) -> &'static str {
    match suffix {
        NumberSuffix::Tab => "tab",
        NumberSuffix::Space => "space",
        NumberSuffix::Nothing => "nothing",
    }
}
fn styles_xml(document: &Document) -> String {
    let mut xml = String::from(XML_DECL);
    xml.push_str(&format!("<w:styles xmlns:w=\"{W_NS}\">"));
    for style in document.styles().iter() {
        write_style(&mut xml, style);
    }
    xml.push_str("</w:styles>");
    xml
}

fn write_style(xml: &mut String, style: &Style) {
    let kind = match style.kind {
        StyleKind::Paragraph => "paragraph",
        StyleKind::Character => "character",
        // Table and numbering styles need parts this writer does not emit yet.
        StyleKind::Other => return,
    };
    xml.push_str(&format!(
        "<w:style w:type=\"{kind}\" w:styleId=\"{}\"><w:name w:val=\"{}\"/>",
        escape_xml(&style.id),
        escape_xml(&style.name)
    ));
    if let Some(props) = &style.para {
        xml.push_str("<w:pPr>");
        write_para_props(xml, props);
        xml.push_str("</w:pPr>");
    }
    if let Some(props) = &style.char {
        xml.push_str("<w:rPr>");
        write_char_props(xml, props);
        xml.push_str("</w:rPr>");
    }
    xml.push_str("</w:style>");
}

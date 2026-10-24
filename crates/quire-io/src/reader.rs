//! WordprocessingML reader.

use std::collections::BTreeMap;
use std::io::{Read, Seek};

use quick_xml::events::Event;
use quick_xml::Reader;
use quire_base::{ImageId, ParaId, QuireError, Result, RunId, TableId};
use quire_model::{
    Block, Border, CellBorders, CharProps, Color, ContentTypes, Document, FidelityFeature,
    FieldKind, FidelityReport, HeaderFooter, HeaderFooterSet, ImageBlock, Numbering, NumberingRef,
    PackageRelationship, Paragraph, Run, Section, SectionProps, StyleSheet, Table, TableBorders,
    TableCell, TableRow, VMerge,
};

use crate::opc::{mime_for_part, normalize_target, parse_relationships};
use crate::overrides::{apply_char_element, apply_para_element, CharOverride, ParaOverride};
use crate::styles::{parse_styles, StyleDefaults};
use crate::xml::{attribute, emu_to_pt, local_name, twips_to_pt};

const DOCUMENT_PART: &str = "word/document.xml";
const STYLES_PART: &str = "word/styles.xml";
const NUMBERING_PART: &str = "word/numbering.xml";
const DOCUMENT_RELS_PART: &str = "word/_rels/document.xml.rels";
const SETTINGS_PART: &str = "word/settings.xml";

#[derive(Clone, Debug)]
pub(crate) struct ImageData {
    pub mime: String,
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct HeaderFooterRefs {
    header_default: Option<String>,
    header_first: Option<String>,
    header_even: Option<String>,
    footer_default: Option<String>,
    footer_first: Option<String>,
    footer_even: Option<String>,
}

#[derive(Default)]
pub(crate) struct StorySection {
    pub blocks: Vec<Block>,
    pub section_props: Option<SectionProps>,
    pub refs: HeaderFooterRefs,
    pub title_page: bool,
}

pub(crate) struct Story {
    /// Sections completed by a paragraph-level `w:sectPr`, in document order.
    pub sections: Vec<StorySection>,
    /// Blocks of the final section (closed by the body-level `w:sectPr`).
    pub blocks: Vec<Block>,
    pub section_props: Option<SectionProps>,
    pub refs: HeaderFooterRefs,
    pub title_page: bool,
    pub fidelity: FidelityReport,
}

/// Read a `.docx` package.
pub fn read_reader<R: Read + Seek>(reader: R) -> Result<Document> {
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|err| QuireError::format(format!("cannot open package: {err}")))?;

    let styles_xml = read_part_text(&mut archive, STYLES_PART)?;
    let numbering_xml = read_part_text(&mut archive, NUMBERING_PART)?;
    let settings_xml = read_part_text(&mut archive, SETTINGS_PART)?;
    let relationships = match read_part_text(&mut archive, DOCUMENT_RELS_PART)? {
        Some(xml) => parse_relationships(&xml)?,
        None => BTreeMap::new(),
    };

    let document_xml = read_part_text(&mut archive, DOCUMENT_PART)?
        .ok_or_else(|| QuireError::format(format!("missing {DOCUMENT_PART}")))?;
    let content_types = read_part_text(&mut archive, "[Content_Types].xml")?
        .map(|xml| parse_content_types(&xml))
        .unwrap_or_default();
    let package_relationships = read_part_text(&mut archive, "_rels/.rels")?
        .map(|xml| parse_relationships(&xml))
        .transpose()?
        .map(|relationships| {
            relationships
                .into_values()
                .map(|relationship| PackageRelationship {
                    id: relationship.id,
                    kind: relationship.kind,
                    target: relationship.target,
                    external: relationship.external,
                })
                .collect()
        })
        .unwrap_or_default();

    let (sheet, defaults): (StyleSheet, StyleDefaults) = match styles_xml.as_deref() {
        Some(xml) => parse_styles(xml)?,
        None => (StyleSheet::new(), StyleDefaults::default()),
    };
    let numbering = match numbering_xml.as_deref() {
        Some(xml) => crate::numbering::parse_numbering(xml)?,
        None => Numbering::default(),
    };

    let document_images = load_images(&mut archive, &relationships);
    let story = parse_story(&document_xml, &sheet, &defaults, &document_images, false)?;
    let mut fidelity = story.fidelity.clone();

    let mut document = Document::new();
    document.clear_sections();

    let mut parts: Vec<StorySection> = story.sections;
    parts.push(StorySection {
        blocks: story.blocks,
        section_props: story.section_props,
        refs: story.refs,
        title_page: story.title_page,
    });
    for part in parts {
        let (headers, footers) = load_header_footer_sets(
            &mut archive,
            &relationships,
            &part.refs,
            &sheet,
            &defaults,
            &mut fidelity,
        )?;
        let mut section = Section::default();
        if let Some(props) = part.section_props {
            section.props = props;
        }
        section.blocks = part.blocks;
        section.headers = headers;
        section.footers = footers;
        section.title_page = part.title_page;
        document.push_section(section);
    }

    *document.styles_mut() = sheet;
    *document.numbering_mut() = numbering;
    document.set_even_and_odd_headers(
        settings_xml
            .as_deref()
            .map(parse_even_and_odd_headers)
            .unwrap_or(false),
    );

    document.renumber();
    document.set_fidelity(fidelity);
    document.package_mut().parts = collect_parts(&mut archive);
    document.package_mut().content_types = content_types;
    document.package_mut().document_relationships = relationships
        .values()
        .map(|relationship| PackageRelationship {
            id: relationship.id.clone(),
            kind: relationship.kind.clone(),
            target: relationship.target.clone(),
            external: relationship.external,
        })
        .collect();
    document.package_mut().package_relationships = package_relationships;
    document.validate()?;
    Ok(document)
}

fn load_header_footer_sets<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    relationships: &BTreeMap<String, crate::opc::Relationship>,
    refs: &HeaderFooterRefs,
    sheet: &StyleSheet,
    defaults: &StyleDefaults,
    fidelity: &mut FidelityReport,
) -> Result<(HeaderFooterSet, HeaderFooterSet)> {
    let headers = HeaderFooterSet {
        default: load_story_part(
            archive,
            relationships,
            refs.header_default.as_deref(),
            sheet,
            defaults,
            fidelity,
        )?,
        first: load_story_part(
            archive,
            relationships,
            refs.header_first.as_deref(),
            sheet,
            defaults,
            fidelity,
        )?,
        even: load_story_part(
            archive,
            relationships,
            refs.header_even.as_deref(),
            sheet,
            defaults,
            fidelity,
        )?,
    };
    let footers = HeaderFooterSet {
        default: load_story_part(
            archive,
            relationships,
            refs.footer_default.as_deref(),
            sheet,
            defaults,
            fidelity,
        )?,
        first: load_story_part(
            archive,
            relationships,
            refs.footer_first.as_deref(),
            sheet,
            defaults,
            fidelity,
        )?,
        even: load_story_part(
            archive,
            relationships,
            refs.footer_even.as_deref(),
            sheet,
            defaults,
            fidelity,
        )?,
    };
    Ok((headers, footers))
}

fn load_images<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    relationships: &BTreeMap<String, crate::opc::Relationship>,
) -> BTreeMap<String, ImageData> {
    let mut images = BTreeMap::new();
    for (id, relationship) in relationships {
        if !relationship.is_image() {
            continue;
        }
        let Some(path) = normalize_target("word", &relationship.target) else {
            continue;
        };
        let Some(bytes) = read_part_bytes(archive, &path) else {
            continue;
        };
        let name = path.rsplit('/').next().unwrap_or("image").to_string();
        images.insert(
            id.clone(),
            ImageData {
                mime: mime_for_part(&path).to_string(),
                name,
                bytes,
            },
        );
    }
    images
}

fn part_rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((directory, name)) => format!("{directory}/_rels/{name}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

fn load_story_part<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    relationships: &BTreeMap<String, crate::opc::Relationship>,
    id: Option<&str>,
    sheet: &StyleSheet,
    defaults: &StyleDefaults,
    fidelity: &mut FidelityReport,
) -> Result<Option<HeaderFooter>> {
    let Some(id) = id else {
        return Ok(None);
    };
    let Some(relationship) = relationships.get(id) else {
        return Ok(None);
    };
    let Some(path) = normalize_target("word", &relationship.target) else {
        return Ok(None);
    };
    let Some(xml) = read_part_text(archive, &path)? else {
        return Ok(None);
    };
    let part_relationships = match read_part_text(archive, &part_rels_path(&path))? {
        Some(rels) => parse_relationships(&rels)?,
        None => BTreeMap::new(),
    };
    let images = load_images(archive, &part_relationships);
    let story = parse_story(&xml, sheet, defaults, &images, true)?;
    fidelity.extend(&story.fidelity);
    Ok(Some(HeaderFooter { blocks: story.blocks }))
}

fn parse_even_and_odd_headers(xml: &str) -> bool {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                let qname = element.name();
                if local_name(qname.as_ref()) == b"evenAndOddHeaders" {
                    return attribute(&element, b"val")
                        .map(|value| value != "0" && value != "false")
                        .unwrap_or(true);
                }
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
}


fn parse_content_types(xml: &str) -> ContentTypes {
    let mut content_types = ContentTypes::default();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                let qname = element.name();
                match local_name(qname.as_ref()) {
                    b"Default" => {
                        if let (Some(extension), Some(content_type)) = (
                            attribute(&element, b"Extension"),
                            attribute(&element, b"ContentType"),
                        ) {
                            content_types.defaults.insert(extension, content_type);
                        }
                    }
                    b"Override" => {
                        if let (Some(part), Some(content_type)) = (
                            attribute(&element, b"PartName"),
                            attribute(&element, b"ContentType"),
                        ) {
                            content_types.overrides.insert(part, content_type);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    content_types
}

fn known_part(name: &str) -> bool {
    matches!(
        name,
        "[Content_Types].xml"
            | "_rels/.rels"
            | "word/document.xml"
            | "word/_rels/document.xml.rels"
            | "word/styles.xml"
            | "word/numbering.xml"
            | "word/settings.xml"
    ) || (name.starts_with("word/header") && name.ends_with(".xml"))
        || (name.starts_with("word/footer") && name.ends_with(".xml"))
        || (name.starts_with("word/_rels/header") && name.ends_with(".rels"))
        || (name.starts_with("word/_rels/footer") && name.ends_with(".rels"))
        || name.starts_with("word/media/")
}

fn collect_parts<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> BTreeMap<String, Vec<u8>> {
    let names: Vec<String> = archive.file_names().map(str::to_string).collect();
    let mut parts = BTreeMap::new();
    for name in names {
        if known_part(&name) {
            continue;
        }
        if let Some(bytes) = read_part_bytes(archive, &name) {
            parts.insert(name, bytes);
        }
    }
    parts
}

fn read_part_bytes<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<Vec<u8>> {
    let mut part = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    part.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn read_part_text<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<String>> {
    let Some(bytes) = read_part_bytes(archive, name) else {
        return Ok(None);
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|err| QuireError::format(format!("{name} is not UTF-8: {err}")))
}

#[derive(Clone, Debug, Default)]
struct PendingImage {
    relationship: Option<String>,
    width_pt: Option<f64>,
    height_pt: Option<f64>,
}

#[derive(Clone, Debug, Default)]
struct TableDraft {
    style: Option<String>,
    grid: Vec<f64>,
    borders: TableBorders,
    rows: Vec<RowDraft>,
}

#[derive(Clone, Debug, Default)]
struct RowDraft {
    height_pt: Option<f64>,
    header: bool,
    cells: Vec<CellDraft>,
}

#[derive(Clone, Debug)]
struct CellDraft {
    blocks: Vec<Block>,
    span: u16,
    vmerge: VMerge,
    borders: CellBorders,
    shading: Option<Color>,
    padding_pt: f64,
    padding_seen: bool,
}

impl Default for CellDraft {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            span: 1,
            vmerge: VMerge::None,
            borders: CellBorders::default(),
            shading: None,
            padding_pt: 4.0,
            padding_seen: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct FieldDraft {
    instruction: String,
    result: String,
    props: Option<CharProps>,
    in_result: bool,
}

impl TableDraft {
    fn finish(self) -> Table {
        Table {
            id: TableId::new(0),
            style: self.style,
            grid: self.grid,
            borders: self.borders,
            rows: self
                .rows
                .into_iter()
                .map(|row| TableRow {
                    height_pt: row.height_pt,
                    header: row.header,
                    cells: row
                        .cells
                        .into_iter()
                        .map(|cell| TableCell {
                            blocks: cell.blocks,
                            span: cell.span,
                            vmerge: cell.vmerge,
                            borders: cell.borders,
                            shading: cell.shading,
                            padding_pt: cell.padding_pt,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

fn parse_story(
    xml: &str,
    sheet: &StyleSheet,
    defaults: &StyleDefaults,
    images: &BTreeMap<String, ImageData>,
    allow_page_fields: bool,
) -> Result<Story> {

    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;

    let mut blocks: Vec<Block> = Vec::new();
    let mut sections: Vec<StorySection> = Vec::new();
    let mut section_props = SectionProps::default();
    let mut saw_section = false;
    let mut pending_section = false;

    let mut paragraph: Option<Paragraph> = None;
    let mut para_style: Option<String> = None;
    let mut para_override = ParaOverride::default();

    let mut run: Option<Run> = None;
    let mut run_style: Option<String> = None;
    let mut run_override = CharOverride::default();

    let mut in_run_props = false;
    let mut in_para_props = false;
    let mut in_text = false;
    let mut in_section = false;
    let mut drawing_depth = 0u32;
    let mut pending: Option<PendingImage> = None;

    let mut table: Option<TableDraft> = None;
    let mut table_row: Option<RowDraft> = None;
    let mut table_cell: Option<CellDraft> = None;
    let mut block_stack: Vec<Vec<Block>> = Vec::new();
    let mut table_depth = 0u32;
    let mut in_tbl_borders = false;
    let mut in_tc_borders = false;
    let mut in_tc_margins = false;
    let mut in_tr_props = false;
    let mut in_num_props = false;
    let mut refs = HeaderFooterRefs::default();
    let mut title_page = false;
    let mut fidelity = FidelityReport::default();
    let mut in_tbl_props = false;
    let mut in_tc_props = false;
    let mut field: Option<FieldDraft> = None;
    let mut field_nesting = 0u32;
    let mut in_instruction = false;
    let mut para_num_id: Option<u32> = None;
    let mut para_num_level: u8 = 0;

    loop {
        let event = reader
            .read_event()
            .map_err(|err| QuireError::format(format!("xml error: {err}")))?;

        let is_empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(element) | Event::Empty(element) => {
                let name = local_name(element.name().as_ref()).to_vec();
                match name.as_slice() {
                    b"p" => {
                        if field.is_some() {
                            finalize_field(
                                &mut field,
                                &mut field_nesting,
                                allow_page_fields,
                                &mut fidelity,
                                &mut paragraph,
                            );
                        }
                        flush_paragraph(
                            &mut paragraph,
                            &mut blocks,
                            sheet,
                            defaults,
                            &para_style,
                            &para_override,
                            true,
                        );
                        para_style = None;
                        para_override = ParaOverride::default();
                        paragraph = Some(Paragraph::new(ParaId::new(0)));
                        para_num_id = None;
                        para_num_level = 0;
                        in_num_props = false;
                        if is_empty {
                            flush_paragraph(
                                &mut paragraph,
                                &mut blocks,
                                sheet,
                                defaults,
                                &para_style,
                                &para_override,
                                true,
                            );
                            para_style = None;
                            para_override = ParaOverride::default();
                        }
                    }
                    b"tbl" => {
                        if table_depth > 0 {
                            fidelity.note(FidelityFeature::NestedTables);
                        }
                        if table_depth == 0 {
                            flush_paragraph(
                                &mut paragraph,
                                &mut blocks,
                                sheet,
                                defaults,
                                &para_style,
                                &para_override,
                                true,
                            );
                            block_stack.push(std::mem::take(&mut blocks));
                            table = Some(TableDraft::default());
                        }
                        table_depth += 1;
                    }
                    b"tblPr" if table_depth == 1 => in_tbl_props = true,
                    b"tblStyle" if table_depth == 1 => {
                        if let (Some(value), Some(table)) =
                            (attribute(&element, b"val"), table.as_mut())
                        {
                            table.style = Some(value);
                        }
                    }
                    b"tblBorders" if table_depth == 1 => in_tbl_borders = true,
                    b"tblGrid" if table_depth == 1 => {}
                    b"gridCol" if table_depth == 1 => {
                        if let (Some(width), Some(table)) = (
                            attribute(&element, b"w").and_then(|value| twips_to_pt(&value)),
                            table.as_mut(),
                        ) {
                            table.grid.push(width.max(0.0));
                        }
                    }
                    b"tr" if table_depth == 1 => table_row = Some(RowDraft::default()),
                    b"trPr" if table_depth == 1 => in_tr_props = true,
                    b"trHeight" if in_tr_props => {
                        if let (Some(height), Some(row)) = (
                            attribute(&element, b"val").and_then(|value| twips_to_pt(&value)),
                            table_row.as_mut(),
                        ) {
                            row.height_pt = Some(height.max(0.0));
                        }
                    }
                    b"tblHeader" if in_tr_props => {
                        if let Some(row) = table_row.as_mut() {
                            row.header = true;
                        }
                    }
                    b"tc" if table_depth == 1 => {
                        flush_paragraph(
                            &mut paragraph,
                            &mut blocks,
                            sheet,
                            defaults,
                            &para_style,
                            &para_override,
                            true,
                        );
                        block_stack.push(std::mem::take(&mut blocks));
                        table_cell = Some(CellDraft::default());
                    }
                    b"tcPr" if table_depth == 1 => in_tc_props = true,
                    b"gridSpan" if table_depth == 1 => {
                        if let (Some(value), Some(cell)) = (
                            attribute(&element, b"val").and_then(|value| value.parse::<u16>().ok()),
                            table_cell.as_mut(),
                        ) {
                            if value > 0 {
                                cell.span = value;
                            }
                        }
                    }
                    b"vMerge" if table_depth == 1 => {
                        if let Some(cell) = table_cell.as_mut() {
                            cell.vmerge = if attribute(&element, b"val").as_deref()
                                == Some("restart")
                            {
                                VMerge::Restart
                            } else {
                                VMerge::Continue
                            };
                        }
                    }
                    b"tcBorders" if table_depth == 1 => in_tc_borders = true,
                    b"shd" if table_depth == 1 => {
                        if let (Some(fill), Some(cell)) =
                            (attribute(&element, b"fill"), table_cell.as_mut())
                        {
                            if fill != "auto" {
                                cell.shading = Color::from_hex(&fill);
                            }
                        }
                    }
                    b"tcMar" if table_depth == 1 => in_tc_margins = true,
                    b"top" if in_tbl_borders => {
                        if let Some(table) = table.as_mut() {
                            table.borders.top = parse_border(&element);
                        }
                    }
                    b"right" if in_tbl_borders => {
                        if let Some(table) = table.as_mut() {
                            table.borders.right = parse_border(&element);
                        }
                    }
                    b"bottom" if in_tbl_borders => {
                        if let Some(table) = table.as_mut() {
                            table.borders.bottom = parse_border(&element);
                        }
                    }
                    b"left" if in_tbl_borders => {
                        if let Some(table) = table.as_mut() {
                            table.borders.left = parse_border(&element);
                        }
                    }
                    b"insideH" if in_tbl_borders => {
                        if let Some(table) = table.as_mut() {
                            table.borders.inside_horizontal = parse_border(&element);
                        }
                    }
                    b"insideV" if in_tbl_borders => {
                        if let Some(table) = table.as_mut() {
                            table.borders.inside_vertical = parse_border(&element);
                        }
                    }
                    b"top" if in_tc_borders => {
                        if let Some(cell) = table_cell.as_mut() {
                            cell.borders.top = parse_border(&element);
                        }
                    }
                    b"right" if in_tc_borders => {
                        if let Some(cell) = table_cell.as_mut() {
                            cell.borders.right = parse_border(&element);
                        }
                    }
                    b"bottom" if in_tc_borders => {
                        if let Some(cell) = table_cell.as_mut() {
                            cell.borders.bottom = parse_border(&element);
                        }
                    }
                    b"left" if in_tc_borders => {
                        if let Some(cell) = table_cell.as_mut() {
                            cell.borders.left = parse_border(&element);
                        }
                    }
                    b"top" | b"left" | b"bottom" | b"right" if in_tc_margins => {
                        if let (Some(value), Some(cell)) = (
                            attribute(&element, b"w").and_then(|value| twips_to_pt(&value)),
                            table_cell.as_mut(),
                        ) {
                            if !cell.padding_seen {
                                cell.padding_pt = 0.0;
                                cell.padding_seen = true;
                            }
                            cell.padding_pt = cell.padding_pt.max(value.max(0.0));
                        }
                    }
                    b"headerReference" => {
                        if let Some(id) = attribute(&element, b"id") {
                            match attribute(&element, b"type").as_deref() {
                                Some("first") => refs.header_first = Some(id),
                                Some("even") => refs.header_even = Some(id),
                                _ => refs.header_default = Some(id),
                            }
                        }
                    }
                    b"footerReference" => {
                        if let Some(id) = attribute(&element, b"id") {
                            match attribute(&element, b"type").as_deref() {
                                Some("first") => refs.footer_first = Some(id),
                                Some("even") => refs.footer_even = Some(id),
                                _ => refs.footer_default = Some(id),
                            }
                        }
                    }
                    b"titlePg" => title_page = true,
                    b"fldSimple" => {
                        if field.is_some() {
                            field_nesting += 1;
                            fidelity.note(FidelityFeature::Fields);
                        } else {
                            field_nesting = 1;
                            field = Some(FieldDraft {
                                instruction: attribute(&element, b"instr").unwrap_or_default(),
                                in_result: true,
                                ..FieldDraft::default()
                            });
                            if is_empty {
                                finalize_field(
                                    &mut field,
                                    &mut field_nesting,
                                    allow_page_fields,
                                    &mut fidelity,
                                    &mut paragraph,
                                );
                            }
                        }
                    }
                    b"fldChar" => match attribute(&element, b"fldCharType").as_deref() {
                        Some("begin") => {
                            if field.is_some() {
                                field_nesting += 1;
                                fidelity.note(FidelityFeature::Fields);
                            } else {
                                field_nesting = 1;
                                field = Some(FieldDraft::default());
                            }
                        }
                        Some("separate") => {
                            if let Some(field) = field.as_mut() {
                                field.in_result = true;
                            }
                        }
                        Some("end") => finalize_field(
                            &mut field,
                            &mut field_nesting,
                            allow_page_fields,
                            &mut fidelity,
                            &mut paragraph,
                        ),
                        _ => {}
                    },
                    b"instrText" => {
                        in_instruction = true;
                        in_text = true;
                    }
                    b"footnoteReference" => fidelity.note(FidelityFeature::Footnotes),
                    b"endnoteReference" => fidelity.note(FidelityFeature::Endnotes),
                    b"commentReference" | b"commentRangeStart" | b"commentRangeEnd" => {
                        fidelity.note(FidelityFeature::Comments)
                    }
                    b"ins" | b"del" | b"moveFrom" | b"moveTo" | b"rPrChange" | b"pPrChange"
                    | b"sectPrChange" | b"tblPrChange" | b"trPrChange" | b"tcPrChange" => {
                        fidelity.note(FidelityFeature::Revisions)
                    }
                    b"anchor" => fidelity.note(FidelityFeature::FloatingShapes),
                    b"txbxContent" => fidelity.note(FidelityFeature::TextBoxes),
                    b"object" | b"pict" => fidelity.note(FidelityFeature::LegacyDrawings),
                    b"sdt" => fidelity.note(FidelityFeature::ContentControls),
                    b"hyperlink" => fidelity.note(FidelityFeature::Hyperlinks),
                    b"sym" => fidelity.note(FidelityFeature::Symbols),
                    b"ruby" => fidelity.note(FidelityFeature::Ruby),
                    b"bookmarkStart" | b"bookmarkEnd" => fidelity.note(FidelityFeature::Bookmarks),
                    b"AlternateContent" => fidelity.note(FidelityFeature::AlternateContent),
                    b"numPr" if !is_empty => in_num_props = true,
                    b"numId" if in_num_props => {
                        para_num_id = attribute(&element, b"val")
                            .and_then(|value| value.parse::<u32>().ok());
                        sync_numbering(&mut paragraph, para_num_id, para_num_level);
                    }
                    b"ilvl" if in_num_props => {
                        if let Some(value) = attribute(&element, b"val")
                            .and_then(|value| value.parse::<u8>().ok())
                        {
                            para_num_level = value;
                            sync_numbering(&mut paragraph, para_num_id, para_num_level);
                        }
                    }
                    b"pPr" if !is_empty => in_para_props = true,
                    b"pStyle" => {
                        if let Some(value) = attribute(&element, b"val") {
                            para_style = Some(value);
                        }
                    }
                    b"r" if !is_empty => {
                        ensure_paragraph(&mut paragraph);
                        run_style = None;
                        run_override = CharOverride::default();
                        run = Some(Run::new(RunId::new(0), "", CharProps::default()));
                    }
                    b"rPr" if !is_empty => in_run_props = true,
                    b"rStyle" => {
                        if let Some(value) = attribute(&element, b"val") {
                            run_style = Some(value);
                        }
                    }
                    b"t" if !is_empty => in_text = true,
                    b"sectPr" => {
                        saw_section = true;
                        if in_para_props {
                            pending_section = true;
                        }
                        if is_empty {
                            continue;
                        }
                        in_section = true;
                    }
                    b"drawing" if !is_empty => {
                        drawing_depth += 1;
                        if drawing_depth == 1 {
                            pending = Some(PendingImage::default());
                        }
                    }
                    b"extent" if drawing_depth > 0 => {
                        if let Some(image) = pending.as_mut() {
                            image.width_pt =
                                attribute(&element, b"cx").and_then(|value| emu_to_pt(&value));
                            image.height_pt =
                                attribute(&element, b"cy").and_then(|value| emu_to_pt(&value));
                        }
                    }
                    b"blip" if drawing_depth > 0 => {
                        if let Some(image) = pending.as_mut() {
                            image.relationship = attribute(&element, b"embed")
                                .or_else(|| attribute(&element, b"link"));
                        }
                    }
                    b"pgSz" if in_section => apply_page_size(&element, &mut section_props),
                    b"pgMar" if in_section => apply_margins(&element, &mut section_props),
                    b"tab" => push_char(&mut run, '\t'),
                    b"br" | b"cr" => push_char(&mut run, '\n'),
                    _ => {
                        if in_run_props {
                            if !matches!(
                                name.as_slice(),
                                b"rStyle"
                                    | b"rFonts"
                                    | b"b"
                                    | b"bCs"
                                    | b"i"
                                    | b"iCs"
                                    | b"u"
                                    | b"sz"
                                    | b"szCs"
                                    | b"color"
                                    | b"lang"
                            ) {
                                fidelity.note(FidelityFeature::UnknownRunProperties);
                            }
                            apply_char_element(&name, &element, &mut run_override);
                        } else if in_para_props {
                            if !matches!(
                                name.as_slice(),
                                b"pStyle"
                                    | b"numPr"
                                    | b"jc"
                                    | b"ind"
                                    | b"spacing"
                                    | b"rPr"
                                    | b"sectPr"
                            ) {
                                fidelity.note(FidelityFeature::UnknownParagraphProperties);
                            }
                            apply_para_element(&name, &element, &mut para_override);
                        } else if in_section {
                            fidelity.note(FidelityFeature::UnknownSectionProperties);
                        } else if in_tbl_props || in_tr_props || in_tc_props {
                            fidelity.note(FidelityFeature::UnknownTableProperties);
                        }
                    }
                }
            }
            Event::Text(text) => {
                if in_text {
                    let value = text
                        .unescape()
                        .map_err(|err| QuireError::format(format!("text error: {err}")))?;
                    if in_instruction {
                        if let Some(field) = field.as_mut() {
                            field.instruction.push_str(&value);
                        }
                    } else if let Some(field) = field.as_mut() {
                        if field.in_result {
                            field.result.push_str(&value);
                            if field.props.is_none() {
                                field.props = run.as_ref().map(|run| run.props.clone());
                            }
                        }
                    } else {
                        push_str(&mut run, &value);
                    }
                }
            }
            Event::End(element) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                match name {
                    b"t" => in_text = false,
                    b"instrText" => {
                        in_instruction = false;
                        in_text = false;
                    }
                    b"fldSimple" => finalize_field(
                        &mut field,
                        &mut field_nesting,
                        allow_page_fields,
                        &mut fidelity,
                        &mut paragraph,
                    ),
                    b"rPr" => in_run_props = false,
                    b"pPr" => in_para_props = false,
                    b"drawing" if !is_empty => {
                        drawing_depth = drawing_depth.saturating_sub(1);
                        if drawing_depth == 0 {
                            finish_drawing(
                                &mut pending,
                                images,
                                &mut paragraph,
                                &mut blocks,
                                sheet,
                                defaults,
                                &para_style,
                                &para_override,
                            );
                            run = None;
                        }
                    }
                    b"r" if !is_empty => {
                        flush_run(
                            &mut run,
                            &mut paragraph,
                            sheet,
                            defaults,
                            &para_style,
                            &run_style,
                            &run_override,
                        );
                    }
                    b"p" => {
                        if field.is_some() {
                            finalize_field(
                                &mut field,
                                &mut field_nesting,
                                allow_page_fields,
                                &mut fidelity,
                                &mut paragraph,
                            );
                        }
                        flush_paragraph(
                            &mut paragraph,
                            &mut blocks,
                            sheet,
                            defaults,
                            &para_style,
                            &para_override,
                            true,
                        );
                        para_style = None;
                        para_override = ParaOverride::default();
                        if pending_section && table_depth == 0 {
                            pending_section = false;
                            sections.push(StorySection {
                                blocks: std::mem::take(&mut blocks),
                                section_props: saw_section.then_some(section_props),
                                refs: std::mem::take(&mut refs),
                                title_page,
                            });
                            section_props = SectionProps::default();
                            saw_section = false;
                            refs = HeaderFooterRefs::default();
                            title_page = false;
                        }
                    }
                    b"numPr" => in_num_props = false,
                    b"tblBorders" => in_tbl_borders = false,
                    b"tcBorders" => in_tc_borders = false,
                    b"tcMar" => in_tc_margins = false,
                    b"trPr" => in_tr_props = false,
                    b"tblPr" => in_tbl_props = false,
                    b"tcPr" => in_tc_props = false,
                    b"tc" if table_depth == 1 => {
                        flush_paragraph(
                            &mut paragraph,
                            &mut blocks,
                            sheet,
                            defaults,
                            &para_style,
                            &para_override,
                            true,
                        );
                        let cell_blocks = std::mem::take(&mut blocks);
                        blocks = block_stack.pop().unwrap_or_default();
                        if let Some(mut cell) = table_cell.take() {
                            cell.blocks = cell_blocks;
                            if let Some(row) = table_row.as_mut() {
                                row.cells.push(cell);
                            }
                        }
                    }
                    b"tr" if table_depth == 1 => {
                        if let (Some(row), Some(table)) = (table_row.take(), table.as_mut()) {
                            table.rows.push(row);
                        }
                    }
                    b"tbl" => {
                        table_depth = table_depth.saturating_sub(1);
                        if table_depth == 0 {
                            flush_paragraph(
                                &mut paragraph,
                                &mut blocks,
                                sheet,
                                defaults,
                                &para_style,
                                &para_override,
                                true,
                            );
                            blocks = block_stack.pop().unwrap_or_default();
                            if let Some(table) = table.take() {
                                blocks.push(Block::Table(table.finish()));
                            }
                        }
                    }
                    b"sectPr" => in_section = false,
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    flush_paragraph(
        &mut paragraph,
        &mut blocks,
        sheet,
        defaults,
        &para_style,
        &para_override,
        true,
    );

    Ok(Story {
        sections,
        blocks,
        section_props: saw_section.then_some(section_props),
        refs,
        title_page,
        fidelity,
    })
}

#[allow(clippy::too_many_arguments)]
fn flush_paragraph(
    paragraph: &mut Option<Paragraph>,
    blocks: &mut Vec<Block>,
    sheet: &StyleSheet,
    defaults: &StyleDefaults,
    para_style: &Option<String>,
    para_override: &ParaOverride,
    keep_empty: bool,
) {
    let Some(mut finished) = paragraph.take() else {
        return;
    };
    if finished.is_empty() {
        if keep_empty {
            finished.props = paragraph_props(sheet, defaults, para_style, para_override);
            finished.style = para_style.clone();
            blocks.push(Block::Paragraph(finished));
        }
        return;
    }
    finished.props = paragraph_props(sheet, defaults, para_style, para_override);
    finished.style = para_style.clone();
    blocks.push(Block::Paragraph(finished));
}

fn paragraph_props(
    sheet: &StyleSheet,
    defaults: &StyleDefaults,
    para_style: &Option<String>,
    para_override: &ParaOverride,
) -> quire_model::ParaProps {
    let base = para_style
        .as_ref()
        .and_then(|id| sheet.paragraph_props(id))
        .cloned()
        .or_else(|| {
            defaults
                .paragraph_id
                .as_ref()
                .and_then(|id| sheet.paragraph_props(id))
                .cloned()
        })
        .unwrap_or_else(|| defaults.para.clone());
    para_override.apply(&base)
}

#[allow(clippy::too_many_arguments)]
fn flush_run(
    run: &mut Option<Run>,
    paragraph: &mut Option<Paragraph>,
    sheet: &StyleSheet,
    defaults: &StyleDefaults,
    para_style: &Option<String>,
    run_style: &Option<String>,
    run_override: &CharOverride,
) {
    let Some(mut finished) = run.take() else {
        return;
    };
    if finished.text.is_empty() {
        return;
    }
    ensure_paragraph(paragraph);
    let base = run_style
        .as_ref()
        .and_then(|id| sheet.char_props(id))
        .cloned()
        .or_else(|| {
            para_style
                .as_ref()
                .and_then(|id| sheet.char_props(id))
                .cloned()
        })
        .or_else(|| {
            defaults
                .paragraph_id
                .as_ref()
                .and_then(|id| sheet.char_props(id))
                .cloned()
        })
        .unwrap_or_else(|| defaults.char.clone());
    finished.props = run_override.apply(&base);
    finished.style = run_style.clone();
    if let Some(paragraph) = paragraph.as_mut() {
        paragraph.runs.push(finished);
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_drawing(
    pending: &mut Option<PendingImage>,
    images: &BTreeMap<String, ImageData>,
    paragraph: &mut Option<Paragraph>,
    blocks: &mut Vec<Block>,
    sheet: &StyleSheet,
    defaults: &StyleDefaults,
    para_style: &Option<String>,
    para_override: &ParaOverride,
) {
    let Some(pending) = pending.take() else {
        return;
    };
    let Some(relationship) = pending.relationship else {
        return;
    };
    let Some(image) = images.get(&relationship) else {
        return;
    };

    // An image-only paragraph must not leave a blank line behind.
    flush_paragraph(
        paragraph,
        blocks,
        sheet,
        defaults,
        para_style,
        para_override,
        false,
    );

    let width = pending.width_pt.unwrap_or(96.0).max(1.0);
    let height = pending.height_pt.unwrap_or(96.0).max(1.0);
    blocks.push(Block::Image(ImageBlock::new(
        ImageId::new(0),
        image.name.clone(),
        image.mime.clone(),
        image.bytes.clone(),
        width,
        height,
    )));
}

fn finalize_field(
    field: &mut Option<FieldDraft>,
    nesting: &mut u32,
    allow_page_fields: bool,
    fidelity: &mut FidelityReport,
    paragraph: &mut Option<Paragraph>,
) {
    if *nesting == 0 {
        return;
    }
    *nesting -= 1;
    if *nesting > 0 {
        return;
    }
    let Some(draft) = field.take() else {
        return;
    };
    let instruction = draft
        .instruction
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let kind = match instruction.as_str() {
        "PAGE" => Some(FieldKind::Page),
        "NUMPAGES" => Some(FieldKind::NumPages),
        _ => None,
    };
    if kind.is_none() || !allow_page_fields {
        fidelity.note(FidelityFeature::Fields);
    }

    ensure_paragraph(paragraph);
    let mut run = Run::new(
        RunId::new(0),
        draft.result,
        draft.props.unwrap_or_default(),
    );
    if allow_page_fields {
        run.field = kind;
    }
    if let Some(paragraph) = paragraph.as_mut() {
        paragraph.runs.push(run);
    }
}

fn sync_numbering(paragraph: &mut Option<Paragraph>, num_id: Option<u32>, level: u8) {
    if let Some(paragraph) = paragraph.as_mut() {
        paragraph.numbering = num_id
            .filter(|id| *id > 0)
            .map(|num_id| NumberingRef { num_id, level });
    }
}

fn ensure_paragraph(paragraph: &mut Option<Paragraph>) {
    if paragraph.is_none() {
        *paragraph = Some(Paragraph::new(ParaId::new(0)));
    }
}

fn push_char(run: &mut Option<Run>, ch: char) {
    if let Some(run) = run.as_mut() {
        run.text.push(ch);
    }
}

fn push_str(run: &mut Option<Run>, text: &str) {
    if let Some(run) = run.as_mut() {
        run.text.push_str(text);
    }
}

fn parse_border(element: &quick_xml::events::BytesStart<'_>) -> Option<Border> {
    let value = attribute(element, b"val").unwrap_or_else(|| "single".to_string());
    if matches!(value.as_str(), "nil" | "none") {
        return None;
    }
    let size = attribute(element, b"sz")
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap_or(4.0);
    let color = attribute(element, b"color")
        .and_then(|value| Color::from_hex(&value))
        .unwrap_or(Color::rgb(0x80, 0x80, 0x80));
    Some(Border::new((size / 8.0).max(0.25), color))
}

fn apply_page_size(element: &quick_xml::events::BytesStart<'_>, props: &mut SectionProps) {
    if let Some(value) = attribute(element, b"w") {
        if let Some(pt) = twips_to_pt(&value) {
            if pt > 0.0 {
                props.page.width_pt = pt;
            }
        }
    }
    if let Some(value) = attribute(element, b"h") {
        if let Some(pt) = twips_to_pt(&value) {
            if pt > 0.0 {
                props.page.height_pt = pt;
            }
        }
    }
    if attribute(element, b"orient").is_some_and(|value| value == "landscape")
        && props.page.width_pt < props.page.height_pt
    {
        std::mem::swap(&mut props.page.width_pt, &mut props.page.height_pt);
    }
}

fn apply_margins(element: &quick_xml::events::BytesStart<'_>, props: &mut SectionProps) {
    if let Some(value) = attribute(element, b"top") {
        if let Some(pt) = twips_to_pt(&value) {
            props.margins.top_pt = pt.max(0.0);
        }
    }
    if let Some(value) = attribute(element, b"bottom") {
        if let Some(pt) = twips_to_pt(&value) {
            props.margins.bottom_pt = pt.max(0.0);
        }
    }
    if let Some(value) = attribute(element, b"left") {
        if let Some(pt) = twips_to_pt(&value) {
            props.margins.left_pt = pt.max(0.0);
        }
    }
    if let Some(value) = attribute(element, b"right") {
        if let Some(pt) = twips_to_pt(&value) {
            props.margins.right_pt = pt.max(0.0);
        }
    }
}

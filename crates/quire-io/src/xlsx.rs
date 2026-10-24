//! Read-only XLSX import.
//!
//! Each worksheet is imported as a document heading plus a table. Formulas,
//! number formats, merged cells, charts and images are not interpreted; cached
//! cell values are used.

use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read, Write};

use quick_xml::events::Event;
use quick_xml::Reader;
use quire_base::{QuireError, Result};
use quire_model::{
    Border, CharProps, Color, Document, Paragraph, Section, Table, TableBorders, TableCell,
    TableRow,
};

use crate::opc::normalize_target;
use crate::sheet::SheetGrid;
use crate::xml::{attribute, local_name};

const MAX_ROWS: usize = 2000;
const MAX_COLUMNS: usize = 64;

struct Sheet {
    name: String,
    path: String,
}

fn read_part(bytes: &[u8], name: &str) -> Result<Option<Vec<u8>>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| QuireError::format(format!("cannot open xlsx: {error}")))?;
    let Ok(mut part) = archive.by_name(name) else {
        return Ok(None);
    };
    let mut data = Vec::new();
    part.read_to_end(&mut data)?;
    Ok(Some(data))
}

fn read_text(bytes: &[u8], name: &str) -> Result<Option<String>> {
    Ok(read_part(bytes, name)?.and_then(|data| String::from_utf8(data).ok()))
}

fn column_index(reference: &str) -> Option<usize> {
    let mut value = 0usize;
    let mut seen = false;
    for byte in reference.bytes() {
        if byte.is_ascii_alphabetic() {
            value = value * 26 + usize::from(byte.to_ascii_uppercase() - b'A' + 1);
            seen = true;
        } else if seen {
            break;
        }
    }
    seen.then(|| value.saturating_sub(1))
}

fn workbook_sheets(xml: &str) -> Vec<Sheet> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut sheets = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                if local_name(element.name().as_ref()) == b"sheet" {
                    if let (Some(name), Some(id)) = (
                        attribute(&element, b"name"),
                        attribute(&element, b"id"),
                    ) {
                        sheets.push(Sheet {
                            name,
                            path: id,
                        });
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    sheets
}

fn workbook_relationships(xml: &str) -> HashMap<String, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut rels = HashMap::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                if local_name(element.name().as_ref()) == b"Relationship" {
                    if let (Some(id), Some(target)) = (
                        attribute(&element, b"Id"),
                        attribute(&element, b"Target"),
                    ) {
                        if let Some(path) = normalize_target("xl", &target) {
                            rels.insert(id, path);
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    rels
}

fn shared_strings(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut in_si = false;
    let mut in_t = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => match local_name(element.name().as_ref()) {
                b"si" => {
                    in_si = true;
                    current.clear();
                }
                b"t" if in_si => in_t = true,
                _ => {}
            },
            Ok(Event::End(element)) => match local_name(element.name().as_ref()) {
                b"t" => in_t = false,
                b"si" if in_si => {
                    in_si = false;
                    strings.push(std::mem::take(&mut current));
                }
                _ => {}
            },
            Ok(Event::Text(text)) => {
                if in_t {
                    if let Ok(value) = text.unescape() {
                        current.push_str(value.as_ref());
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    strings
}

fn format_number(value: &str) -> String {
    let Ok(number) = value.trim().parse::<f64>() else {
        return value.to_string();
    };
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{}", number as i64)
    } else {
        format!("{number}")
    }
}

fn sheet_cells(xml: &str, strings: &[String]) -> BTreeMap<(usize, usize), String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut cells = BTreeMap::new();
    let mut row_index = 0usize;
    let mut cell_column = 0usize;
    let mut cell_type = String::new();
    let mut in_value = false;
    let mut in_inline = false;
    let mut in_text = false;
    let mut value = String::new();
    let mut inline = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                match local_name(element.name().as_ref()) {
                    b"row" => {
                        if let Some(value) = attribute(&element, b"r") {
                            if let Ok(index) = value.parse::<usize>() {
                                row_index = index.saturating_sub(1);
                            }
                        }
                    }
                    b"c" => {
                        cell_type = attribute(&element, b"t").unwrap_or_default();
                        cell_column = attribute(&element, b"r")
                            .and_then(|reference| column_index(&reference))
                            .unwrap_or(cell_column);
                        value.clear();
                        inline.clear();
                    }
                    b"v" => in_value = true,
                    b"is" => in_inline = true,
                    b"t" if in_inline => in_text = true,
                    _ => {}
                }
            }
            Ok(Event::End(element)) => match local_name(element.name().as_ref()) {
                b"v" => in_value = false,
                b"t" if in_inline => in_text = false,
                b"is" => in_inline = false,
                b"c" => {
                    let text = if cell_type == "s" {
                        value
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|index| strings.get(index).cloned())
                            .unwrap_or_default()
                    } else if cell_type == "inlineStr" {
                        inline.clone()
                    } else if cell_type == "b" {
                        if value.trim() == "1" {
                            "TRUE".to_string()
                        } else {
                            "FALSE".to_string()
                        }
                    } else if cell_type == "e" {
                        value.clone()
                    } else {
                        format_number(&value)
                    };
                    if !text.is_empty() {
                        cells.insert((row_index, cell_column), text);
                    }
                    cell_column += 1;
                }
                _ => {}
            },
            Ok(Event::Text(text)) => {
                if in_value || in_text {
                    if let Ok(text_value) = text.unescape() {
                        if in_value {
                            value.push_str(text_value.as_ref());
                        } else {
                            inline.push_str(text_value.as_ref());
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    cells
}

pub(crate) fn read_grids(bytes: &[u8]) -> Result<Vec<SheetGrid>> {
    let workbook = read_text(bytes, "xl/workbook.xml")?
        .ok_or_else(|| QuireError::format("xlsx has no workbook.xml"))?;
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(QuireError::format("xlsx has no worksheets"));
    }
    let relationships = read_text(bytes, "xl/_rels/workbook.xml.rels")?
        .map(|xml| workbook_relationships(&xml))
        .unwrap_or_default();
    let strings = read_text(bytes, "xl/sharedStrings.xml")?
        .map(|xml| shared_strings(&xml))
        .unwrap_or_default();

    let mut grids = Vec::new();
    for sheet in sheets {
        let path = relationships
            .get(&sheet.path)
            .cloned()
            .unwrap_or_else(|| "xl/worksheets/sheet1.xml".to_string());
        let cells = read_part(bytes, &path)?
            .and_then(|data| String::from_utf8(data).ok())
            .map(|xml| sheet_cells(&xml, &strings))
            .unwrap_or_default();
        let rows = cells
            .keys()
            .map(|(row, _)| *row + 1)
            .max()
            .unwrap_or(0)
            .min(MAX_ROWS);
        let columns = cells
            .keys()
            .map(|(_, column)| *column + 1)
            .max()
            .unwrap_or(0)
            .min(MAX_COLUMNS);

        let mut grid_rows = Vec::with_capacity(rows);
        for row_index in 0..rows {
            let mut row = Vec::with_capacity(columns);
            for column_index in 0..columns {
                row.push(
                    cells
                        .get(&(row_index, column_index))
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            grid_rows.push(row);
        }
        grids.push(SheetGrid {
            name: sheet.name,
            rows: grid_rows,
        });
    }
    Ok(grids)
}

fn column_name(mut index: usize) -> String {
    let mut name = String::new();
    loop {
        name.insert(0, (b'A' + (index % 26) as u8) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    name
}

fn numeric_cell(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed != value {
        return false;
    }
    if trimmed.len() > 1
        && trimmed.starts_with('0')
        && !trimmed.starts_with("0.")
    {
        return false;
    }
    if trimmed.starts_with('+') {
        return false;
    }
    trimmed.parse::<f64>().is_ok()
}

fn sheet_xml(sheet: &SheetGrid) -> String {
    use quire_base::xml::escape_xml;
    let mut data = String::new();
    for (row_index, row) in sheet.rows.iter().enumerate() {
        if row.iter().all(|value| value.is_empty()) {
            continue;
        }
        data.push_str(&format!("<row r=\"{}\">", row_index + 1));
        for column_index in 0..row.len() {
            let value = sheet.display_cell(row_index, column_index);
            if value.is_empty() {
                continue;
            }
            let reference = format!("{}{}", column_name(column_index), row_index + 1);
            if numeric_cell(&value) {
                data.push_str(&format!(
                    "<c r=\"{reference}\"><v>{}</v></c>",
                    escape_xml(&value)
                ));
            } else {
                data.push_str(&format!(
                    "<c r=\"{reference}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                    escape_xml(&value)
                ));
            }
        }
        data.push_str("</row>");
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
<sheetData>{data}</sheetData></worksheet>"
    )
}

pub(crate) fn write_grids(sheets: &[SheetGrid]) -> Result<Vec<u8>> {
    use quire_base::xml::escape_xml;
    let fallback;
    let sheets = if sheets.is_empty() {
        fallback = vec![SheetGrid {
            name: "Sheet1".to_string(),
            rows: Vec::new(),
        }];
        fallback.as_slice()
    } else {
        sheets
    };

    let mut workbook_sheets = String::new();
    let mut workbook_rels = String::new();
    let mut content_types = String::new();
    let mut parts: Vec<(String, String)> = Vec::new();
    for (index, sheet) in sheets.iter().enumerate() {
        let id = index + 1;
        let name = if sheet.name.is_empty() {
            format!("Sheet{id}")
        } else {
            sheet.name.clone()
        };
        workbook_sheets.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{id}\" r:id=\"rId{id}\"/>",
            escape_xml(&name)
        ));
        workbook_rels.push_str(&format!(
            "<Relationship Id=\"rId{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{id}.xml\"/>"
        ));
        content_types.push_str(&format!(
            "<Override PartName=\"/xl/worksheets/sheet{id}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
        ));
        parts.push((format!("xl/worksheets/sheet{id}.xml"), sheet_xml(sheet)));
    }

    let content_types = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
{content_types}</Types>"
    );
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
</Relationships>"
        .to_string();
    let workbook = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<sheets>{workbook_sheets}</sheets></workbook>"
    );
    let workbook_rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{workbook_rels}</Relationships>"
    );

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", root_rels),
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", workbook_rels),
        ] {
            zip.start_file(name, options)
                .map_err(|error| QuireError::format(format!("cannot write xlsx: {error}")))?;
            zip.write_all(body.as_bytes())?;
        }
        for (name, body) in parts {
            zip.start_file(name, options)
                .map_err(|error| QuireError::format(format!("cannot write xlsx: {error}")))?;
            zip.write_all(body.as_bytes())?;
        }
        zip.finish()
            .map_err(|error| QuireError::format(format!("cannot finish xlsx: {error}")))?;
    }
    Ok(cursor.into_inner())
}

/// Read an XLSX workbook as document tables.
pub fn read(bytes: &[u8]) -> Result<Document> {
    let workbook = read_text(bytes, "xl/workbook.xml")?
        .ok_or_else(|| QuireError::format("xlsx has no workbook.xml"))?;
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(QuireError::format("xlsx has no worksheets"));
    }
    let relationships = read_text(bytes, "xl/_rels/workbook.xml.rels")?
        .map(|xml| workbook_relationships(&xml))
        .unwrap_or_default();
    let strings = read_text(bytes, "xl/sharedStrings.xml")?
        .map(|xml| shared_strings(&xml))
        .unwrap_or_default();

    let mut document = Document::new();
    document.clear_sections();
    let mut section = Section::default();
    let border = Border::new(0.5, Color::rgb(0x80, 0x80, 0x80));
    let borders = TableBorders {
        top: Some(border),
        right: Some(border),
        bottom: Some(border),
        left: Some(border),
        inside_horizontal: Some(border),
        inside_vertical: Some(border),
    };

    for sheet in sheets {
        let path = relationships
            .get(&sheet.path)
            .cloned()
            .unwrap_or_else(|| "xl/worksheets/sheet1.xml".to_string());
        let Some(data) = read_part(bytes, &path)? else {
            continue;
        };
        let Ok(xml) = String::from_utf8(data) else {
            continue;
        };
        let cells = sheet_cells(&xml, &strings);
        let rows = cells.keys().map(|(row, _)| *row + 1).max().unwrap_or(0);
        let columns = cells.keys().map(|(_, column)| *column + 1).max().unwrap_or(0);
        let rows = rows.min(MAX_ROWS);
        let columns = columns.min(MAX_COLUMNS);
        if rows == 0 || columns == 0 {
            continue;
        }

        let mut heading = Paragraph::plain(format!("工作表：{}", sheet.name));
        if let Some(run) = heading.runs.first_mut() {
            run.props = CharProps {
                size_pt: 14.0,
                bold: true,
                ..CharProps::default()
            };
        }
        section.push_paragraph_block(heading);

        let mut table = Table::new(quire_base::TableId::new(0));
        table.grid = vec![90.0; columns];
        table.borders = borders;
        for row_index in 0..rows {
            let mut row = TableRow::default();
            for column_index in 0..columns {
                let text = cells
                    .get(&(row_index, column_index))
                    .cloned()
                    .unwrap_or_default();
                let mut cell = TableCell::default();
                cell.blocks
                    .push(quire_model::Block::Paragraph(Paragraph::plain(text)));
                row.cells.push(cell);
            }
            table.rows.push(row);
        }
        section.push_table(table);
    }

    document.push_section(section);
    document.renumber();
    document.validate()?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_column_references() {
        assert_eq!(column_index("A1"), Some(0));
        assert_eq!(column_index("Z9"), Some(25));
        assert_eq!(column_index("AA2"), Some(26));
    }

    #[test]
    fn formats_numbers() {
        assert_eq!(format_number("12.0"), "12");
        assert_eq!(format_number("12.5"), "12.5");
    }

    #[test]
    fn imports_a_small_workbook() {
        use std::io::Write;
        let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let strings = r#"<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>Name</t></si><si><t>Qty</t></si></sst>"#;
        let sheet = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row><row r="2"><c r="A2"><v>12.0</v></c><c r="B2" t="inlineStr"><is><t>Inline</t></is></c></row></sheetData></worksheet>"#;
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, body) in [
                ("xl/workbook.xml", workbook),
                ("xl/_rels/workbook.xml.rels", rels),
                ("xl/sharedStrings.xml", strings),
                ("xl/worksheets/sheet1.xml", sheet),
            ] {
                zip.start_file(name, options).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let doc = read(&cursor.into_inner()).unwrap();
        assert_eq!(doc.first_section().table_count(), 1);
        let table = match &doc.first_section().blocks[1] {
            quire_model::Block::Table(table) => table,
            other => panic!("expected table, got {other:?}"),
        };
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells[0].plain_text(), "Name");
        assert_eq!(table.rows[1].cells[0].plain_text(), "12");
        assert_eq!(table.rows[1].cells[1].plain_text(), "Inline");
    }


    #[test]
    fn grid_round_trip_keeps_values_and_sheet_names() {
        let sheets = vec![
            SheetGrid {
                name: "Data".to_string(),
                rows: vec![
                    vec!["Name".to_string(), "Qty".to_string()],
                    vec!["Pen".to_string(), "12".to_string()],
                ],
            },
            SheetGrid {
                name: "Notes".to_string(),
                rows: vec![vec!["hello".to_string()]],
            },
        ];
        let bytes = write_grids(&sheets).unwrap();
        let read = read_grids(&bytes).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].name, "Data");
        assert_eq!(read[0].rows[1][1], "12");
        assert_eq!(read[1].name, "Notes");
        assert_eq!(read[1].rows[0][0], "hello");
    }

}

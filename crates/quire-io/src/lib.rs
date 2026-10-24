//! DOCX (ISO/IEC 29500) reading and writing.
//!
//! The reader resolves document defaults and named styles into effective
//! properties, maps images to [`quire_model::ImageBlock`] values, and drops
//! markup it does not model yet. The writer emits a minimal but valid package.
//!
//! Scope of this milestone:
//! * body paragraphs, runs, `w:t`/`w:tab`/`w:br`;
//! * bold, italic, underline, size, colour (direct and inherited);
//! * paragraph and character styles from `word/styles.xml`;
//! * alignment, indents, spacing;
//! * page size and margins;
//! * inline images referenced by relationships.
//!
//! Not yet: numbering, tables, headers/footers, fields, footnotes, revision
//! marks, floating shapes, or passthrough of unknown parts. These are explicit
//! next steps; the writer only ever sees the modelled subset.

#![forbid(unsafe_code)]

mod csv;
mod formula;
mod formats;
mod numbering;
mod odt;
mod odt_write;
mod opc;
mod overrides;
mod reader;
mod rtf;
mod sheet;
mod styles;
mod text;
mod xlsx;
mod writer;
mod xml;

use std::fs::File;
use std::io::{Cursor, Read, Seek, Write};
use std::path::Path;

use quire_base::Result;
use quire_model::Document;

pub use formats::{
    Format, read_document, read_document_bytes, write_document, write_document_with_policy,
};
pub use sheet::{SheetGrid, read_sheets, write_sheets};

/// Read a `.docx` file from disk.
pub fn read_docx(path: impl AsRef<Path>) -> Result<Document> {
    let file = File::open(path)?;
    read_reader(file)
}

/// Read a `.docx` package from memory.
pub fn read_docx_bytes(bytes: &[u8]) -> Result<Document> {
    read_reader(Cursor::new(bytes))
}

/// Read a `.docx` package from any seekable reader.
pub fn read_reader<R: Read + Seek>(reader: R) -> Result<Document> {
    reader::read_reader(reader)
}

/// Write a document as a `.docx` file.
pub fn write_docx(path: impl AsRef<Path>, document: &Document) -> Result<()> {
    let file = File::create(path)?;
    write_docx_to(file, document)
}

/// Write a document as a `.docx` package to a seekable writer.
pub fn write_docx_to<W: Write + Seek>(writer: W, document: &Document) -> Result<()> {
    writer::write(writer, document)
}

/// Build a `.docx` package in memory.
pub fn docx_bytes(document: &Document) -> Result<Vec<u8>> {
    writer::bytes(document)
}

/// Write even when unsupported content is present. Diagnostic use only.
pub fn write_docx_force(path: impl AsRef<Path>, document: &Document) -> Result<()> {
    let file = File::create(path)?;
    writer::write_force(file, document)
}

/// Build a package in memory even when unsupported content is present.
pub fn docx_bytes_force(document: &Document) -> Result<Vec<u8>> {
    writer::bytes_force(document)
}

/// Write a document as a `.odt` file.
pub fn write_odt(path: impl AsRef<Path>, document: &Document) -> Result<()> {
    let file = File::create(path)?;
    write_odt_to(file, document)
}

/// Write a document as an `.odt` package to a seekable writer.
pub fn write_odt_to<W: Write + Seek>(writer: W, document: &Document) -> Result<()> {
    odt_write::write(writer, document)
}

/// Build an `.odt` package in memory.
pub fn odt_bytes(document: &Document) -> Result<Vec<u8>> {
    odt_write::bytes(document)
}

/// Write an `.odt` file even when unsupported content is present. Diagnostic use only.
pub fn write_odt_force(path: impl AsRef<Path>, document: &Document) -> Result<()> {
    let file = File::create(path)?;
    odt_write::write_force(file, document)
}

/// Build an `.odt` package in memory even when unsupported content is present.
pub fn odt_bytes_force(document: &Document) -> Result<Vec<u8>> {
    odt_write::bytes_force(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quire_base::ParaId;
    use quire_model::{
        AbstractNumbering, Align, Block, Border, CellBorders, CharProps, Color, HeaderFooter, ImageBlock, Margins,
        NumberFormat, NumberSuffix, Numbering, NumberingInstance, NumberingLevel, NumberingRef,
        FieldKind, PageSetup, Paragraph, Run, SectionProps, Style, StyleKind, StyleSheet, Table, TableBorders,
        TableCell, TableRow, VMerge, Section,
    };

    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc,
        0xcf, 0xc0, 0x50, 0x0f, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn round_trips_text_and_formatting() {
        let mut doc = Document::new();
        let mut paragraph = Paragraph::plain("Hello ����");
        paragraph.runs[0].props.bold = true;
        paragraph.runs[0].props.size_pt = 14.0;
        doc.push_paragraph(paragraph);

        let bytes = docx_bytes(&doc).unwrap();
        let read = read_docx_bytes(&bytes).unwrap();

        assert_eq!(read.paragraph_count(), 1);
        let paragraph = read.paragraph(0, 0).unwrap();
        assert_eq!(paragraph.text(), "Hello ����");
        assert!(paragraph.runs[0].props.bold);
        assert!((paragraph.runs[0].props.size_pt - 14.0).abs() < 1e-9);
    }

    #[test]
    fn round_trips_paragraph_properties() {
        let mut doc = Document::new();
        let mut paragraph = Paragraph::plain("centered");
        paragraph.props.align = Align::Center;
        paragraph.props.indent_left_pt = 24.0;
        doc.push_paragraph(paragraph);

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        let paragraph = read.paragraph(0, 0).unwrap();
        assert_eq!(paragraph.props.align, Align::Center);
        assert!((paragraph.props.indent_left_pt - 24.0).abs() < 1e-9);
    }

    #[test]
    fn round_trips_page_geometry() {
        let mut doc = Document::new();
        doc.first_section_mut().props = SectionProps {
            page: PageSetup::letter(),
            margins: Margins::none(),
        };
        doc.push_paragraph(Paragraph::plain("x"));

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        assert!((read.first_section().props.page.width_pt - 612.0).abs() < 1.0);
        assert!((read.first_section().props.margins.left_pt).abs() < 1.0);
    }

    #[test]
    fn parses_explicit_breaks_and_tabs() {
        let mut doc = Document::new();
        let mut paragraph = Paragraph::new(ParaId::new(1));
        paragraph.push_text("a\tb\nc", CharProps::default());
        doc.push_paragraph(paragraph);

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        assert_eq!(read.paragraph(0, 0).unwrap().text(), "a\tb\nc");
    }

    #[test]
    fn rejects_non_package_bytes() {
        let error = read_docx_bytes(b"not a zip").unwrap_err();
        assert!(matches!(error, quire_base::QuireError::Format(_)));
    }

    #[test]
    fn round_trips_named_styles() {
        let mut doc = Document::new();
        let mut sheet = StyleSheet::new();
        let mut style = Style::new("Body", "Body Text", StyleKind::Paragraph);
        style.para = Some(quire_model::ParaProps {
            align: Align::Center,
            ..Default::default()
        });
        style.char = Some(CharProps {
            size_pt: 12.0,
            ..Default::default()
        });
        sheet.insert(style);
        *doc.styles_mut() = sheet;

        let mut paragraph = Paragraph::plain("styled");
        paragraph.style = Some("Body".to_string());
        paragraph.props.align = Align::Center;
        paragraph.runs[0].props.size_pt = 12.0;
        doc.push_paragraph(paragraph);

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        let paragraph = read.paragraph(0, 0).unwrap();
        assert_eq!(paragraph.style.as_deref(), Some("Body"));
        assert_eq!(paragraph.props.align, Align::Center);
        assert!((paragraph.runs[0].props.size_pt - 12.0).abs() < 1e-9);
        assert_eq!(read.styles().len(), 1);
    }

    #[test]
    fn round_trips_images() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("before"));
        doc.first_section_mut().push_image(ImageBlock::new(
            quire_base::ImageId::new(1),
            "pixel.png",
            "image/png",
            PNG_1X1.to_vec(),
            72.0,
            72.0,
        ));
        doc.push_paragraph(Paragraph::plain("after"));

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        assert_eq!(read.block_count(), 3);

        let images: Vec<&ImageBlock> = read
            .blocks()
            .filter_map(|block| match block {
                quire_model::Block::Image(image) => Some(image),
                _ => None,
            })
            .collect();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].mime, "image/png");
        assert_eq!(images[0].data.as_ref(), PNG_1X1);
        assert!((images[0].width_pt - 72.0).abs() < 1.0);
    }

    #[test]
    fn image_only_paragraph_does_not_leave_blank_line() {
        let mut doc = Document::new();
        doc.first_section_mut().push_image(ImageBlock::new(
            quire_base::ImageId::new(1),
            "pixel.png",
            "image/png",
            PNG_1X1.to_vec(),
            10.0,
            10.0,
        ));

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        assert_eq!(read.block_count(), 1);
        assert_eq!(read.paragraph_count(), 0);
    }

    #[test]
    fn round_trips_tables() {
        let mut doc = Document::new();
        let mut table = Table::new(quire_base::TableId::new(0));
        table.grid = vec![120.0, 80.0];
        table.borders = TableBorders {
            top: Some(Border::new(0.5, Color::BLACK)),
            left: Some(Border::new(0.5, Color::BLACK)),
            right: Some(Border::new(0.5, Color::BLACK)),
            bottom: Some(Border::new(0.5, Color::BLACK)),
            inside_horizontal: Some(Border::new(0.25, Color::rgb(0x80, 0x80, 0x80))),
            inside_vertical: Some(Border::new(0.25, Color::rgb(0x80, 0x80, 0x80))),
        };

        let mut first_row = TableRow::default();
        let mut left = TableCell {
            shading: Some(Color::rgb(0xF5, 0xF0, 0xE6)),
            borders: CellBorders {
                left: Some(Border::new(1.0, Color::rgb(0xD9, 0x48, 0x0F))),
                ..Default::default()
            },
            ..TableCell::default()
        };
        left.blocks.push(Block::Paragraph(Paragraph::plain("A")));
        let mut right = TableCell::default();
        right.blocks.push(Block::Paragraph(Paragraph::plain("B")));
        first_row.cells.push(left);
        first_row.cells.push(right);

        let mut second_row = TableRow {
            header: false,
            height_pt: Some(30.0),
            ..TableRow::default()
        };
        let mut merged = TableCell {
            span: 2,
            vmerge: VMerge::Restart,
            ..TableCell::default()
        };
        merged.blocks.push(Block::Paragraph(Paragraph::plain("C")));
        second_row.cells.push(merged);

        table.rows.push(first_row);
        table.rows.push(second_row);
        doc.first_section_mut().push_table(table);

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        assert_eq!(read.first_section().table_count(), 1);
        let table = match &read.first_section().blocks[0] {
            Block::Table(table) => table,
            other => panic!("expected table, got {other:?}"),
        };
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].cells.len(), 2);
        assert_eq!(table.rows[1].cells[0].span, 2);
        assert_eq!(table.rows[1].cells[0].vmerge, VMerge::Restart);
        assert_eq!(table.rows[1].height_pt, Some(30.0));
        assert_eq!(table.rows[0].cells[0].shading, Some(Color::rgb(0xF5, 0xF0, 0xE6)));
        assert!(table.rows[0].cells[0].borders.left.is_some());
        assert_eq!(table.rows[0].cells[0].plain_text(), "A");
        assert_eq!(table.grid.len(), 2);
        assert!(read.plain_text().contains("A\tB"));
        assert!(read.plain_text().contains('C'));
    }

    fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, body) in parts {
                zip.start_file(*name, options).unwrap();
                use std::io::Write;
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn reads_handcrafted_table_markup() {
        let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:tbl>
  <w:tblPr>
    <w:tblStyle w:val="TableGrid"/>
    <w:tblBorders>
      <w:top w:val="single" w:sz="8" w:color="FF0000"/>
      <w:insideH w:val="single" w:sz="4" w:color="000000"/>
    </w:tblBorders>
  </w:tblPr>
  <w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="3000"/></w:tblGrid>
  <w:tr><w:trPr><w:tblHeader/></w:trPr>
    <w:tc><w:tcPr><w:gridSpan w:val="2"/><w:shd w:val="clear" w:color="auto" w:fill="EEEEEE"/><w:tcMar><w:left w:w="240" w:type="dxa"/></w:tcMar></w:tcPr>
      <w:p><w:r><w:t>Merged</w:t></w:r></w:p>
    </w:tc>
  </w:tr>
  <w:tr>
    <w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc>
    <w:tc><w:tcPr><w:tcBorders><w:bottom w:val="double" w:sz="12" w:color="0000FF"/></w:tcBorders></w:tcPr><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc>
  </w:tr>
</w:tbl>
<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>
</w:body></w:document>"#;

        let bytes = package(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
            ("word/document.xml", document),
        ]);

        let read = read_docx_bytes(&bytes).unwrap();
        let table = match &read.first_section().blocks[0] {
            Block::Table(table) => table,
            other => panic!("expected table, got {other:?}"),
        };
        assert_eq!(table.style.as_deref(), Some("TableGrid"));
        assert_eq!(table.grid, vec![100.0, 150.0]);
        assert!(table.rows[0].header);
        assert_eq!(table.rows[0].cells[0].span, 2);
        assert_eq!(table.rows[0].cells[0].shading, Some(Color::rgb(0xEE, 0xEE, 0xEE)));
        assert!((table.rows[0].cells[0].padding_pt - 12.0).abs() < 1e-9);
        assert!((table.borders.top.unwrap().width_pt - 1.0).abs() < 1e-9);
        assert_eq!(table.rows[1].cells[0].vmerge, VMerge::Restart);
        assert_eq!(
            table.rows[1].cells[1].borders.bottom.unwrap().color,
            Color::rgb(0, 0, 0xFF)
        );
        assert_eq!(table.plain_text(), "Merged\nA\tB\n");
    }

    fn sample_numbering() -> Numbering {
        let mut numbering = Numbering::default();
        let mut abstract_numbering = AbstractNumbering {
            id: 0,
            ..AbstractNumbering::default()
        };
        abstract_numbering.levels.insert(
            0,
            NumberingLevel {
                level: 0,
                start: 1,
                format: NumberFormat::Decimal,
                text: "%1.".to_string(),
                indent_left_pt: 36.0,
                hanging_pt: 18.0,
                suffix: NumberSuffix::Space,
            },
        );
        abstract_numbering.levels.insert(
            1,
            NumberingLevel {
                level: 1,
                start: 1,
                format: NumberFormat::LowerLetter,
                text: "%2)".to_string(),
                indent_left_pt: 72.0,
                hanging_pt: 18.0,
                suffix: NumberSuffix::Tab,
            },
        );
        numbering.abstracts.insert(0, abstract_numbering);
        numbering.instances.insert(
            1,
            NumberingInstance {
                id: 1,
                abstract_id: 0,
                ..NumberingInstance::default()
            },
        );
        numbering
    }

    #[test]
    fn round_trips_numbering() {
        let mut doc = Document::new();
        *doc.numbering_mut() = sample_numbering();

        let mut first = Paragraph::plain("First");
        first.numbering = Some(NumberingRef {
            num_id: 1,
            level: 0,
        });
        doc.push_paragraph(first);
        let mut second = Paragraph::plain("Second");
        second.numbering = Some(NumberingRef {
            num_id: 1,
            level: 1,
        });
        doc.push_paragraph(second);

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        assert_eq!(read.numbering().instances.len(), 1);
        assert_eq!(read.numbering().abstracts.len(), 1);
        assert_eq!(
            read.paragraph(0, 0).unwrap().numbering,
            Some(NumberingRef {
                num_id: 1,
                level: 0
            })
        );
        assert_eq!(
            read.paragraph(0, 1).unwrap().numbering,
            Some(NumberingRef {
                num_id: 1,
                level: 1
            })
        );
        let level = read.numbering().level(1, 1).unwrap();
        assert_eq!(level.format, NumberFormat::LowerLetter);
        assert_eq!(level.text, "%2)");
    }

    #[test]
    fn reads_handcrafted_numbering_markup() {
        let numbering = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0"><w:start w:val="3"/><w:numFmt w:val="upperRoman"/><w:lvlText w:val="%1."/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl>
  </w:abstractNum>
  <w:num w:numId="7"><w:abstractNumId w:val="0"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="5"/></w:lvlOverride></w:num>
</w:numbering>"#;
        let document = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="7"/></w:numPr></w:pPr><w:r><w:t>Listed</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>
</w:body></w:document>"#;

        let bytes = package(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
            ("word/document.xml", document),
            ("word/numbering.xml", numbering),
        ]);

        let read = read_docx_bytes(&bytes).unwrap();
        assert_eq!(read.numbering().level(7, 0).unwrap().start, 3);
        assert_eq!(read.numbering().start(7, 0), Some(5));
        assert_eq!(
            read.numbering().level(7, 0).unwrap().format,
            NumberFormat::UpperRoman
        );
        assert_eq!(
            read.paragraph(0, 0).unwrap().numbering,
            Some(NumberingRef {
                num_id: 7,
                level: 0
            })
        );
    }

    #[test]
    fn round_trips_headers_and_footers() {
        let mut doc = Document::new();
        doc.set_even_and_odd_headers(true);
        {
            let section = doc.first_section_mut();
            section.title_page = true;
            section.headers.default = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("Default header"))],
            });
            section.headers.first = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("First header"))],
            });
            section.footers.even = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("Even footer"))],
            });
        }
        doc.push_paragraph(Paragraph::plain("Body"));

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        assert!(read.even_and_odd_headers());
        assert!(read.first_section().title_page);
        let headers = &read.first_section().headers;
        assert_eq!(
            headers.default.as_ref().unwrap().blocks[0]
                .as_paragraph()
                .unwrap()
                .text(),
            "Default header"
        );
        assert_eq!(
            headers.first.as_ref().unwrap().blocks[0]
                .as_paragraph()
                .unwrap()
                .text(),
            "First header"
        );
        assert_eq!(
            read.first_section().footers.even.as_ref().unwrap().blocks[0]
                .as_paragraph()
                .unwrap()
                .text(),
            "Even footer"
        );
    }

    #[test]
    fn reads_handcrafted_header_reference() {
        let document = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body text</w:t></w:r></w:p>
<w:sectPr><w:headerReference w:type="default" r:id="rId5"/><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>
</w:body></w:document>"#;
        let header = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>From header</w:t></w:r></w:p></w:hdr>"#;
        let document_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
        let bytes = package(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document),
            ("word/header1.xml", header),
        ]);

        let read = read_docx_bytes(&bytes).unwrap();
        let header = read.first_section().headers.default.as_ref().unwrap();
        assert_eq!(
            header.blocks[0].as_paragraph().unwrap().text(),
            "From header"
        );
    }


    #[test]
    fn reads_handcrafted_multiple_sections() {
        let document = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>S1</w:t></w:r></w:p>
<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId5"/><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:bottom="1440" w:left="1800" w:right="1800"/></w:sectPr></w:pPr></w:p>
<w:p><w:r><w:t>S2</w:t></w:r></w:p>
<w:p><w:r><w:t>S3</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="16838" w:h="11906"/></w:sectPr>
</w:body></w:document>"#;
        let header = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>First section header</w:t></w:r></w:p></w:hdr>"#;
        let document_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
        let bytes = package(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
            ("word/_rels/document.xml.rels", document_rels),
            ("word/document.xml", document),
            ("word/header1.xml", header),
        ]);

        let read = read_docx_bytes(&bytes).unwrap();
        assert_eq!(read.sections().len(), 2);

        let first = &read.sections()[0];
        let texts: Vec<_> = first
            .blocks
            .iter()
            .filter_map(|block| block.as_paragraph())
            .map(|paragraph| paragraph.text().to_string())
            .collect();
        assert_eq!(texts, vec!["S1".to_string(), String::new()]);
        assert_eq!(
            first.headers.default.as_ref().unwrap().blocks[0]
                .as_paragraph()
                .unwrap()
                .text(),
            "First section header"
        );
        assert!((first.props.page.width_pt - 595.28).abs() < 0.5);
        assert!((first.props.margins.left_pt - 90.0).abs() < 0.5);
        assert!((first.props.margins.top_pt - 72.0).abs() < 0.5);
        assert!(first.headers.default.is_some());

        let last = read.sections().last().unwrap();
        let texts: Vec<_> = last
            .blocks
            .iter()
            .filter_map(|block| block.as_paragraph())
            .map(|paragraph| paragraph.text().to_string())
            .collect();
        assert_eq!(texts, vec!["S2".to_string(), "S3".to_string()]);
        assert!((last.props.page.width_pt - 841.89).abs() < 0.5);
        assert!(last.headers.default.is_none());
        assert!(read.plain_text().contains("S3"));
    }

    #[test]
    fn round_trips_multiple_sections() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("One"));
        {
            let first = doc.first_section_mut();
            first.headers.default = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("Header one"))],
            });
            first.footers.default = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("Footer one"))],
            });
            first.props.page = PageSetup::a4();
        }
        let mut second = Section {
            title_page: true,
            ..Section::default()
        };
        second.props.page = PageSetup {
            width_pt: 841.89,
            height_pt: 595.28,
        };
        second.props.margins.left_pt = 36.0;
        second.blocks.push(Block::Paragraph(Paragraph::plain("Two")));
        second.headers.first = Some(HeaderFooter {
            blocks: vec![Block::Paragraph(Paragraph::plain("Header two"))],
        });
        doc.push_section(second);
        let mut third = Section::default();
        third.blocks.push(Block::Paragraph(Paragraph::plain("Three")));
        third.headers.default = Some(HeaderFooter {
            blocks: vec![Block::Paragraph(Paragraph::plain("Header three"))],
        });
        doc.push_section(third);

        let written = docx_bytes(&doc).unwrap();
        let read = read_docx_bytes(&written).unwrap();
        assert_eq!(read.sections().len(), 3);

        assert_eq!(read.sections()[0].blocks.len(), 1);
        assert_eq!(
            read.sections()[0]
                .blocks
                .iter()
                .filter_map(|block| block.as_paragraph())
                .map(|paragraph| paragraph.text().to_string())
                .collect::<Vec<_>>(),
            vec!["One".to_string()]
        );
        assert_eq!(
            read.sections()[0].headers.default.as_ref().unwrap().blocks[0]
                .as_paragraph()
                .unwrap()
                .text(),
            "Header one"
        );
        assert_eq!(
            read.sections()[0].footers.default.as_ref().unwrap().blocks[0]
                .as_paragraph()
                .unwrap()
                .text(),
            "Footer one"
        );

        assert!(read.sections()[1].title_page);
        assert!((read.sections()[1].props.page.width_pt - 841.89).abs() < 0.5);
        assert!((read.sections()[1].props.margins.left_pt - 36.0).abs() < 0.5);
        assert_eq!(
            read.sections()[1].headers.first.as_ref().unwrap().blocks[0]
                .as_paragraph()
                .unwrap()
                .text(),
            "Header two"
        );
        assert!(read.sections()[1].headers.default.is_none());

        assert_eq!(
            read.sections()[2].blocks[0]
                .as_paragraph()
                .unwrap()
                .text(),
            "Three"
        );
        assert_eq!(
            read.sections()[2].headers.default.as_ref().unwrap().blocks[0]
                .as_paragraph()
                .unwrap()
                .text(),
            "Header three"
        );
        assert!((read.sections()[2].props.page.width_pt - 595.28).abs() < 0.5);

        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(written)).unwrap();
        let mut names = Vec::new();
        for index in 0..archive.len() {
            names.push(archive.by_index(index).unwrap().name().to_string());
        }
        for part in [
            "word/header1.xml",
            "word/header2.xml",
            "word/header3.xml",
            "word/footer1.xml",
        ] {
            assert!(names.iter().any(|name| name == part), "missing {part}; have {names:?}");
        }

        let mut document_xml = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut document_xml)
            .unwrap();
        assert_eq!(
            document_xml.matches("<w:pPr><w:sectPr>").count(),
            2,
            "each non-final section must close inside a paragraph: {document_xml}"
        );
        let tail = document_xml.rsplit("</w:p>").next().unwrap();
        assert!(
            tail.contains("<w:sectPr>"),
            "final section must be body-level: {document_xml}"
        );

        let mut rels = String::new();
        archive
            .by_name("word/_rels/document.xml.rels")
            .unwrap()
            .read_to_string(&mut rels)
            .unwrap();
        assert_eq!(rels.matches("/relationships/header\"").count(), 3);
        assert_eq!(rels.matches("/relationships/footer\"").count(), 1);
    }

    #[test]
    fn strict_writer_refuses_lossy_documents() {
        let document = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:fldSimple w:instr=" PAGE "><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>
<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>
</w:body></w:document>"#;
        let bytes = package(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
            ("word/document.xml", document),
        ]);

        let read = read_docx_bytes(&bytes).unwrap();
        assert!(!read.fidelity().is_lossless());
        assert!(read.fidelity().summary().contains("fields"));
        assert!(docx_bytes(&read).is_err());
        assert!(docx_bytes_force(&read).is_ok());
    }

    #[test]
    fn preserves_unknown_parts_and_relationships() {
        let document = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Body</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>
</w:body></w:document>"#;
        let theme = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office"/>"#;
        let core = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"/>"#;
        let bytes = package(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="theme/theme1.xml"/></Relationships>"#,
            ),
            ("word/document.xml", document),
            ("word/theme/theme1.xml", theme),
            ("docProps/core.xml", core),
        ]);

        let read = read_docx_bytes(&bytes).unwrap();
        assert!(read.fidelity().is_lossless());
        assert_eq!(
            read.package().parts.get("word/theme/theme1.xml").unwrap(),
            theme.as_bytes()
        );
        assert_eq!(
            read.package().parts.get("docProps/core.xml").unwrap(),
            core.as_bytes()
        );

        let written = docx_bytes(&read).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(written)).unwrap();
        let mut theme_out = String::new();
        archive
            .by_name("word/theme/theme1.xml")
            .unwrap()
            .read_to_string(&mut theme_out)
            .unwrap();
        assert_eq!(theme_out, theme);

        let mut content_types = String::new();
        archive
            .by_name("[Content_Types].xml")
            .unwrap()
            .read_to_string(&mut content_types)
            .unwrap();
        assert!(content_types.contains("/word/theme/theme1.xml"));
        assert!(content_types.contains("/docProps/core.xml"));

        let mut package_rels = String::new();
        archive
            .by_name("_rels/.rels")
            .unwrap()
            .read_to_string(&mut package_rels)
            .unwrap();
        assert!(package_rels.contains("docProps/core.xml"));
        assert!(package_rels.contains("officeDocument"));

        let mut document_rels = String::new();
        archive
            .by_name("word/_rels/document.xml.rels")
            .unwrap()
            .read_to_string(&mut document_rels)
            .unwrap();
        assert!(document_rels.contains("theme/theme1.xml"));
        assert!(document_rels.contains("styles.xml"));
    }


    #[test]
    fn round_trips_font_properties() {
        let mut doc = Document::new();
        let mut paragraph = Paragraph::plain("Fonts");
        {
            let props = &mut paragraph.runs[0].props;
            props.font_family = Some("Georgia".to_string());
            props.font_family_east_asian = Some("SimSun".to_string());
            props.size_complex_pt = Some(11.0);
            props.bold_complex = true;
            props.language = Some("en-US".to_string());
            props.language_east_asian = Some("zh-CN".to_string());
        }
        doc.push_paragraph(paragraph);

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        assert!(read.fidelity().is_lossless());
        let props = &read.paragraph(0, 0).unwrap().runs[0].props;
        assert_eq!(props.font_family.as_deref(), Some("Georgia"));
        assert_eq!(props.font_family_east_asian.as_deref(), Some("SimSun"));
        assert_eq!(props.size_complex_pt, Some(11.0));
        assert!(props.bold_complex);
        assert_eq!(props.language.as_deref(), Some("en-US"));
        assert_eq!(props.language_east_asian.as_deref(), Some("zh-CN"));
    }


    #[test]
    fn round_trips_header_page_fields() {
        let mut doc = Document::new();
        let mut header = Paragraph::plain("Page ");
        let mut page_field = Run::plain("1");
        page_field.field = Some(FieldKind::Page);
        header.runs.push(page_field);
        header.push_text(" of ", CharProps::default());
        let mut total_field = Run::plain("1");
        total_field.field = Some(FieldKind::NumPages);
        header.runs.push(total_field);

        doc.first_section_mut().headers.default = Some(HeaderFooter {
            blocks: vec![Block::Paragraph(header)],
        });
        doc.push_paragraph(Paragraph::plain("Body"));

        let read = read_docx_bytes(&docx_bytes(&doc).unwrap()).unwrap();
        assert!(read.fidelity().is_lossless());
        let header = read.first_section().headers.default.as_ref().unwrap();
        let paragraph = header.blocks[0].as_paragraph().unwrap();
        assert_eq!(paragraph.runs[1].field, Some(FieldKind::Page));
        assert_eq!(paragraph.runs[1].text, "1");
        assert_eq!(paragraph.runs[3].field, Some(FieldKind::NumPages));

        let again = read_docx_bytes(&docx_bytes(&read).unwrap()).unwrap();
        let paragraph = again.first_section().headers.default.as_ref().unwrap().blocks[0]
            .as_paragraph()
            .unwrap();
        assert_eq!(paragraph.runs[1].field, Some(FieldKind::Page));
    }

    #[test]
    fn reads_complex_header_field() {
        let header = r#"<?xml version="1.0"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p>
<w:r><w:fldChar w:fldCharType="begin"/></w:r>
<w:r><w:instrText> NUMPAGES </w:instrText></w:r>
<w:r><w:fldChar w:fldCharType="separate"/></w:r>
<w:r><w:t>9</w:t></w:r>
<w:r><w:fldChar w:fldCharType="end"/></w:r>
</w:p></w:hdr>"#;
        let document = r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>
<w:p><w:r><w:t>Body</w:t></w:r></w:p>
<w:sectPr><w:headerReference w:type="default" r:id="rId5"/><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>
</w:body></w:document>"#;
        let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
        let bytes = package(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
            ("word/_rels/document.xml.rels", rels),
            ("word/document.xml", document),
            ("word/header1.xml", header),
        ]);

        let read = read_docx_bytes(&bytes).unwrap();
        assert!(read.fidelity().is_lossless());
        let paragraph = read.first_section().headers.default.as_ref().unwrap().blocks[0]
            .as_paragraph()
            .unwrap();
        assert_eq!(paragraph.runs.len(), 1);
        assert_eq!(paragraph.runs[0].field, Some(FieldKind::NumPages));
        assert_eq!(paragraph.runs[0].text, "9");
    }

    #[test]
    fn body_page_field_is_still_unsupported() {
        let document = r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:fldSimple w:instr=" PAGE "><w:r><w:t>7</w:t></w:r></w:fldSimple></w:p>
<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>
</w:body></w:document>"#;
        let bytes = package(&[
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
            ("word/document.xml", document),
        ]);
        let read = read_docx_bytes(&bytes).unwrap();
        assert!(!read.fidelity().is_lossless());
        assert_eq!(read.paragraph(0, 0).unwrap().text(), "7");
        assert!(docx_bytes(&read).is_err());
    }

}

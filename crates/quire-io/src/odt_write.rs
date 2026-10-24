//! ODF 1.2 text (`.odt`) export.
//!
//! spec: ODF 1.2 part 1 §3.1 (document content), §3.6 (automatic styles),
//!       §3.8 (master styles), §16.1 (packages)
//! design: docs/01-architecture.md
//!
//! The writer is intentionally lossless-or-refuse: it serialises everything
//! the model currently represents and rejects documents whose fidelity report
//! lists unrepresentable content. List numbering is not written yet, so
//! documents that use it are refused instead of silently flattened.

use std::io::{Cursor, Seek, Write};
use std::sync::Arc;

use quire_base::xml::escape_xml;
use quire_base::{QuireError, Result};
use quire_model::{
    Align, Block, CellBorders, CharProps, Color, Document, FieldKind, HeaderFooter,
    HeaderFooterSet, ImageBlock, ParaProps, Paragraph, Run, SectionProps, StyleKind, Table, TableRow,
    VMerge,
};

use crate::opc::extension_for_mime;

const MIMETYPE: &str = "application/vnd.oasis.opendocument.text";

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";

const NAMESPACES: &str = concat!(
    " xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\"",
    " xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\"",
    " xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\"",
    " xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\"",
    " xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\"",
    " xmlns:fo=\"urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0\"",
    " xmlns:xlink=\"http://www.w3.org/1999/xlink\"",
    " xmlns:svg=\"urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0\"",
    " xmlns:dc=\"http://purl.org/dc/elements/1.1/\"",
    " xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\"",
    " xmlns:number=\"urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0\"",
);

/// Serialise `document` as an ODT package, refusing lossy content.
pub fn write<W: Write + Seek>(writer: W, document: &Document) -> Result<()> {
    write_package(writer, document, false)
}

/// Serialise `document` as an ODT package, allowing known-lossy content.
pub fn write_force<W: Write + Seek>(writer: W, document: &Document) -> Result<()> {
    write_package(writer, document, true)
}

/// Serialise `document` into a fresh ODT byte buffer.
pub fn bytes(document: &Document) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    write_package(&mut cursor, document, false)?;
    Ok(cursor.into_inner())
}

/// Serialise `document` into a byte buffer, allowing known-lossy content.
pub fn bytes_force(document: &Document) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    write_package(&mut cursor, document, true)?;
    Ok(cursor.into_inner())
}

fn write_package<W: Write + Seek>(
    writer: W,
    document: &Document,
    allow_lossy: bool,
) -> Result<()> {
    document.validate()?;
    if !allow_lossy {
        if !document.fidelity().is_lossless() {
            return Err(QuireError::unsupported(format!(
                "document uses content that cannot be written back yet: {}",
                document.fidelity().summary()
            )));
        }
        if has_numbering(document) {
            return Err(QuireError::unsupported(
                "odt export does not write list numbering yet",
            ));
        }
    }

    let mut odt = OdtWriter::new(document);
    let (content, styles) = odt.render();

    let stored = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut zip = zip::ZipWriter::new(writer);
    zip.start_file("mimetype", stored).map_err(zip_error)?;
    zip.write_all(MIMETYPE.as_bytes())?;
    zip.start_file("META-INF/manifest.xml", deflated)
        .map_err(zip_error)?;
    zip.write_all(odt.manifest_xml().as_bytes())?;
    zip.start_file("styles.xml", deflated).map_err(zip_error)?;
    zip.write_all(styles.as_bytes())?;
    zip.start_file("content.xml", deflated).map_err(zip_error)?;
    zip.write_all(content.as_bytes())?;
    for image in &odt.images {
        zip.start_file(image.path.as_str(), deflated)
            .map_err(zip_error)?;
        zip.write_all(&image.data)?;
    }
    zip.finish().map_err(zip_error)?;
    Ok(())
}

fn zip_error(error: zip::result::ZipError) -> QuireError {
    QuireError::format(format!("odt zip error: {error}"))
}

fn master_name(index: usize) -> String {
    if index == 0 {
        "Standard".to_string()
    } else {
        format!("Standard{index}")
    }
}

fn has_numbering(document: &Document) -> bool {
    document.sections().iter().any(|section| {
        blocks_have_numbering(&section.blocks)
            || set_has_numbering(&section.headers)
            || set_has_numbering(&section.footers)
    })
}

fn set_has_numbering(set: &HeaderFooterSet) -> bool {
    [&set.default, &set.first, &set.even]
        .into_iter()
        .flatten()
        .any(|part| blocks_have_numbering(&part.blocks))
}

fn blocks_have_numbering(blocks: &[Block]) -> bool {
    blocks.iter().any(|block| match block {
        Block::Paragraph(paragraph) => paragraph.numbering.is_some(),
        Block::Table(table) => table
            .rows
            .iter()
            .any(|row| row.cells.iter().any(|cell| blocks_have_numbering(&cell.blocks))),
        Block::Image(_) => false,
    })
}

#[derive(Clone, Debug, PartialEq)]
struct CharStyle {
    props: CharProps,
    parent: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct ParaStyle {
    props: ParaProps,
    parent: Option<String>,
    break_before: bool,
    master_page: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct CellStyle {
    borders: CellBorders,
    shading: Option<Color>,
    padding_pt: f64,
}

/// Assigns stable `prefix + ordinal` names to distinct style values.
struct StyleTable<T> {
    prefix: &'static str,
    entries: Vec<(String, T)>,
}

impl<T: Clone + PartialEq> StyleTable<T> {
    fn new(prefix: &'static str) -> Self {
        Self {
            prefix,
            entries: Vec::new(),
        }
    }

    fn intern(&mut self, value: &T) -> String {
        for (name, existing) in &self.entries {
            if existing == value {
                return name.clone();
            }
        }
        let name = format!("{}{}", self.prefix, self.entries.len() + 1);
        self.entries.push((name.clone(), value.clone()));
        name
    }

    fn entries(&self) -> &[(String, T)] {
        &self.entries
    }
}

struct PendingImage {
    path: String,
    mime: String,
    data: Arc<[u8]>,
}

struct OdtWriter<'a> {
    document: &'a Document,
    chars: StyleTable<CharStyle>,
    paras: StyleTable<ParaStyle>,
    cell_styles: StyleTable<CellStyle>,
    table_styles: Vec<String>,
    images: Vec<PendingImage>,
    table_count: usize,
    image_count: usize,
}

impl<'a> OdtWriter<'a> {
    fn new(document: &'a Document) -> Self {
        Self {
            document,
            chars: StyleTable::new("T"),
            paras: StyleTable::new("P"),
            cell_styles: StyleTable::new("TC"),
            table_styles: Vec::new(),
            images: Vec::new(),
            table_count: 0,
            image_count: 0,
        }
    }

    fn render(&mut self) -> (String, String) {
        let masters = self.master_styles_xml();
        let body = self.body_xml();
        let auto = self.auto_styles_xml();
        let layouts = self.page_layouts_xml();
        let named = self.named_styles_xml();

        let content = format!(
            "{XML_DECL}<office:document-content{NAMESPACES} office:version=\"1.2\">\
             <office:automatic-styles>{auto}</office:automatic-styles>\
             <office:body><office:text>{body}</office:text></office:body>\
             </office:document-content>"
        );
        let styles = format!(
            "{XML_DECL}<office:document-styles{NAMESPACES} office:version=\"1.2\">\
             <office:styles>{named}</office:styles>\
             <office:automatic-styles>{layouts}{auto}</office:automatic-styles>\
             <office:master-styles>{masters}</office:master-styles>\
             </office:document-styles>"
        );
        (content, styles)
    }

    fn manifest_xml(&self) -> String {
        let mut out = format!(
            "{XML_DECL}<manifest:manifest \
             xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\" \
             manifest:version=\"1.2\">"
        );
        out.push_str(&format!(
            "<manifest:file-entry manifest:full-path=\"/\" manifest:media-type=\"{MIMETYPE}\"/>"
        ));
        out.push_str(
            "<manifest:file-entry manifest:full-path=\"content.xml\" manifest:media-type=\"text/xml\"/>",
        );
        out.push_str(
            "<manifest:file-entry manifest:full-path=\"styles.xml\" manifest:media-type=\"text/xml\"/>",
        );
        for image in &self.images {
            out.push_str(&format!(
                "<manifest:file-entry manifest:full-path=\"{}\" manifest:media-type=\"{}\"/>",
                escape_xml(&image.path),
                escape_xml(&image.mime)
            ));
        }
        out.push_str("</manifest:manifest>");
        out
    }

    fn body_xml(&mut self) -> String {
        let document = self.document;
        let sections = document.sections();
        if sections.is_empty() {
            return "<text:p/>".to_string();
        }
        let mut out = String::new();
        for (index, section) in sections.iter().enumerate() {
            let master = master_name(index);
            let needs_break = index > 0;
            let mut emitted_break = false;
            for block in &section.blocks {
                match block {
                    Block::Paragraph(paragraph) => {
                        let with_break = needs_break && !emitted_break;
                        emitted_break = true;
                        let master = with_break.then_some(master.as_str());
                        out.push_str(&self.paragraph_xml(paragraph, with_break, master));
                    }
                    Block::Image(image) => {
                        if needs_break && !emitted_break {
                            emitted_break = true;
                            out.push_str(&self.break_paragraph(&master));
                        }
                        out.push_str(&self.image_xml(image));
                    }
                    Block::Table(table) => {
                        if needs_break && !emitted_break {
                            emitted_break = true;
                            out.push_str(&self.break_paragraph(&master));
                        }
                        out.push_str(&self.table_xml(table));
                    }
                }
            }
            if needs_break && !emitted_break {
                out.push_str(&self.break_paragraph(&master));
            }
        }
        out
    }

    fn blocks_xml(&mut self, blocks: &[Block]) -> String {
        if blocks.is_empty() {
            return "<text:p/>".to_string();
        }
        let mut out = String::new();
        for block in blocks {
            match block {
                Block::Paragraph(paragraph) => {
                    out.push_str(&self.paragraph_xml(paragraph, false, None));
                }
                Block::Image(image) => out.push_str(&self.image_xml(image)),
                Block::Table(table) => out.push_str(&self.table_xml(table)),
            }
        }
        out
    }

    fn break_paragraph(&mut self, master: &str) -> String {
        let style = self.paras.intern(&ParaStyle {
            props: ParaProps::default(),
            parent: None,
            break_before: true,
            master_page: Some(master.to_string()),
        });
        format!("<text:p text:style-name=\"{style}\"/>")
    }

    fn paragraph_xml(
        &mut self,
        paragraph: &Paragraph,
        break_before: bool,
        master_page: Option<&str>,
    ) -> String {
        let style = self.paras.intern(&ParaStyle {
            props: paragraph.props.clone(),
            parent: paragraph.style.clone(),
            break_before,
            master_page: master_page.map(str::to_string),
        });
        let mut out = format!("<text:p text:style-name=\"{style}\">");
        for run in &paragraph.runs {
            out.push_str(&self.run_xml(run));
        }
        out.push_str("</text:p>");
        out
    }

    fn run_xml(&mut self, run: &Run) -> String {
        let style = self.chars.intern(&CharStyle {
            props: run.props.clone(),
            parent: run.style.clone(),
        });
        if let Some(field) = run.field {
            let (element, cached) = match field {
                FieldKind::Page => ("text:page-number", field.value(1, 1)),
                FieldKind::NumPages => ("text:page-count", field.value(1, 1)),
            };
            return format!(
                "<{element} text:style-name=\"{style}\">{}</{element}>",
                escape_xml(&cached)
            );
        }
        if run.text.is_empty() {
            return String::new();
        }
        format!(
            "<text:span text:style-name=\"{style}\">{}</text:span>",
            text_content(&run.text)
        )
    }

    fn image_xml(&mut self, image: &ImageBlock) -> String {
        self.image_count += 1;
        let number = self.image_count;
        let extension = extension_for_mime(&image.mime);
        let path = format!("Pictures/image{number}.{extension}");
        self.images.push(PendingImage {
            path: path.clone(),
            mime: image.mime.clone(),
            data: Arc::clone(&image.data),
        });
        format!(
            "<text:p><draw:frame draw:style-name=\"fr1\" draw:name=\"Image{number}\" \
             text:anchor-type=\"as-char\" svg:width=\"{:.2}pt\" svg:height=\"{:.2}pt\">\
             <draw:image xlink:href=\"{}\" xlink:type=\"simple\" xlink:show=\"embed\" \
             xlink:actuate=\"onLoad\"/></draw:frame></text:p>",
            image.width_pt,
            image.height_pt,
            escape_xml(&path)
        )
    }

    fn table_xml(&mut self, table: &Table) -> String {
        self.table_count += 1;
        let index = self.table_count;
        let name = format!("Table{index}");
        let columns = table.column_count().max(1);
        let grid = table_grid(table, columns);
        let total: f64 = grid.iter().sum();

        let mut styles = format!(
            "<style:style style:name=\"{name}\" style:family=\"table\">\
             <style:table-properties style:width=\"{total:.2}pt\" table:align=\"margins\" \
             table:border-model=\"collapsing\"/></style:style>"
        );
        for (column, width) in grid.iter().enumerate() {
            let letter = column_letter(column);
            styles.push_str(&format!(
                "<style:style style:name=\"{name}.{letter}\" style:family=\"table-column\">\
                 <style:table-column-properties style:column-width=\"{width:.2}pt\"/>\
                 </style:style>"
            ));
        }
        self.table_styles.push(styles);

        let mut out = format!("<table:table table:name=\"{name}\" table:style-name=\"{name}\">");
        for column in 0..columns {
            let letter = column_letter(column);
            out.push_str(&format!(
                "<table:table-column table:style-name=\"{name}.{letter}\"/>"
            ));
        }
        for (row_index, row) in table.rows.iter().enumerate() {
            out.push_str(&self.row_xml(table, row, row_index, columns));
        }
        out.push_str("</table:table>");
        out
    }

    fn row_xml(
        &mut self,
        table: &Table,
        row: &TableRow,
        row_index: usize,
        columns: usize,
    ) -> String {
        let mut out = String::from("<table:table-row>");
        let mut column = 0usize;
        for cell in &row.cells {
            if column >= columns {
                break;
            }
            let span = cell.span.max(1) as usize;
            if cell.vmerge == VMerge::Continue {
                for _ in 0..span {
                    out.push_str("<table:covered-table-cell/>");
                }
                column += span;
                continue;
            }
            let row_span = vertical_span(table, row_index, column);
            let style = self.cell_styles.intern(&CellStyle {
                borders: cell.borders,
                shading: cell.shading,
                padding_pt: cell.padding_pt,
            });
            out.push_str(&format!(
                "<table:table-cell table:style-name=\"{style}\" office:value-type=\"string\""
            ));
            if span > 1 {
                out.push_str(&format!(" table:number-columns-spanned=\"{span}\""));
            }
            if row_span > 1 {
                out.push_str(&format!(" table:number-rows-spanned=\"{row_span}\""));
            }
            out.push('>');
            out.push_str(&self.blocks_xml(&cell.blocks));
            out.push_str("</table:table-cell>");
            column += span;
        }
        while column < columns {
            out.push_str("<table:table-cell/>");
            column += 1;
        }
        out.push_str("</table:table-row>");
        out
    }

    fn auto_styles_xml(&self) -> String {
        let mut out = String::new();
        if !self.images.is_empty() {
            out.push_str(
                "<style:style style:name=\"fr1\" style:family=\"graphic\">\
                 <style:graphic-properties style:vertical-pos=\"top\" \
                 style:vertical-rel=\"baseline\"/></style:style>",
            );
        }
        for (name, style) in self.chars.entries() {
            out.push_str(&format!(
                "<style:style style:name=\"{name}\" style:family=\"text\""
            ));
            if let Some(parent) = &style.parent {
                out.push_str(&format!(
                    " style:parent-style-name=\"{}\"",
                    escape_xml(parent)
                ));
            }
            out.push_str("><style:text-properties");
            out.push_str(&char_props_attrs(&style.props));
            out.push_str("/></style:style>");
        }
        for (name, style) in self.paras.entries() {
            out.push_str(&format!(
                "<style:style style:name=\"{name}\" style:family=\"paragraph\""
            ));
            if let Some(parent) = &style.parent {
                out.push_str(&format!(
                    " style:parent-style-name=\"{}\"",
                    escape_xml(parent)
                ));
            }
            if let Some(master) = &style.master_page {
                out.push_str(&format!(
                    " style:master-page-name=\"{}\"",
                    escape_xml(master)
                ));
            }
            out.push_str("><style:paragraph-properties");
            if style.break_before {
                out.push_str(" fo:break-before=\"page\"");
            }
            out.push_str(&para_props_attrs(&style.props));
            out.push_str("/></style:style>");
        }
        for (name, style) in self.cell_styles.entries() {
            out.push_str(&format!(
                "<style:style style:name=\"{name}\" style:family=\"table-cell\">\
                 <style:table-cell-properties"
            ));
            for (side, border) in [
                ("top", style.borders.top),
                ("right", style.borders.right),
                ("bottom", style.borders.bottom),
                ("left", style.borders.left),
            ] {
                if let Some(border) = border {
                    out.push_str(&format!(
                        " fo:border-{side}=\"{:.2}pt solid #{}\"",
                        border.width_pt,
                        border.color.to_hex()
                    ));
                }
            }
            if let Some(shading) = style.shading {
                out.push_str(&format!(" fo:background-color=\"#{}\"", shading.to_hex()));
            }
            out.push_str(&format!(" fo:padding=\"{:.2}pt\"/>", style.padding_pt));
            out.push_str("</style:style>");
        }
        for fragment in &self.table_styles {
            out.push_str(fragment);
        }
        out
    }

    fn named_styles_xml(&self) -> String {
        let mut out = String::from(
            "<style:default-style style:family=\"paragraph\">\
             <style:paragraph-properties fo:orphans=\"2\" fo:widows=\"2\"/>\
             <style:text-properties fo:font-size=\"10.50pt\"/></style:default-style>\
             <style:style style:name=\"Standard\" style:family=\"paragraph\" style:class=\"text\"/>",
        );
        for style in self.document.styles().iter() {
            let family = match style.kind {
                StyleKind::Paragraph => "paragraph",
                StyleKind::Character => "text",
                StyleKind::Other => continue,
            };
            out.push_str(&format!(
                "<style:style style:name=\"{}\" style:display-name=\"{}\" style:family=\"{family}\">",
                escape_xml(&style.id),
                escape_xml(&style.name)
            ));
            if let Some(props) = &style.para {
                out.push_str("<style:paragraph-properties");
                out.push_str(&para_props_attrs(props));
                out.push_str("/>");
            }
            if let Some(props) = &style.char {
                out.push_str("<style:text-properties");
                out.push_str(&char_props_attrs(props));
                out.push_str("/>");
            }
            out.push_str("</style:style>");
        }
        out
    }

    fn page_layouts_xml(&self) -> String {
        let document = self.document;
        let sections = document.sections();
        let mirrored = document.even_and_odd_headers();
        if sections.is_empty() {
            return page_layout_xml("PM1", &SectionProps::default(), mirrored);
        }
        let mut out = String::new();
        for (index, section) in sections.iter().enumerate() {
            out.push_str(&page_layout_xml(
                &format!("PM{}", index + 1),
                &section.props,
                mirrored,
            ));
        }
        out
    }

    fn master_styles_xml(&mut self) -> String {
        let document = self.document;
        let sections = document.sections();
        if sections.is_empty() {
            return "<style:master-page style:name=\"Standard\" style:page-layout-name=\"PM1\"/>"
                .to_string();
        }
        let mut out = String::new();
        for (index, section) in sections.iter().enumerate() {
            let name = master_name(index);
            out.push_str(&format!(
                "<style:master-page style:name=\"{}\" style:page-layout-name=\"PM{}\">",
                escape_xml(&name),
                index + 1
            ));
            out.push_str(&self.header_footer_part("header", section.headers.default.as_ref()));
            out.push_str(&self.header_footer_part("header-left", section.headers.even.as_ref()));
            out.push_str(&self.header_footer_part("header-first", section.headers.first.as_ref()));
            out.push_str(&self.header_footer_part("footer", section.footers.default.as_ref()));
            out.push_str(&self.header_footer_part("footer-left", section.footers.even.as_ref()));
            out.push_str(&self.header_footer_part("footer-first", section.footers.first.as_ref()));
            out.push_str("</style:master-page>");
        }
        out
    }

    fn header_footer_part(&mut self, element: &str, part: Option<&HeaderFooter>) -> String {
        match part {
            Some(part) => format!(
                "<style:{element}>{}</style:{element}>",
                self.blocks_xml(&part.blocks)
            ),
            None => String::new(),
        }
    }
}

fn page_layout_xml(name: &str, props: &SectionProps, mirrored: bool) -> String {
    let page = props.page;
    let margins = props.margins;
    let orientation = if page.width_pt > page.height_pt {
        "landscape"
    } else {
        "portrait"
    };
    let mut out = format!(
        "<style:page-layout style:name=\"{name}\"><style:page-layout-properties \
         fo:page-width=\"{:.2}pt\" fo:page-height=\"{:.2}pt\" \
         style:print-orientation=\"{orientation}\" \
         fo:margin-top=\"{:.2}pt\" fo:margin-bottom=\"{:.2}pt\" \
         fo:margin-left=\"{:.2}pt\" fo:margin-right=\"{:.2}pt\"",
        page.width_pt, page.height_pt, margins.top_pt, margins.bottom_pt, margins.left_pt,
        margins.right_pt
    );
    if mirrored {
        out.push_str(" style:page-usage=\"mirrored\"");
    }
    out.push_str("/></style:page-layout>");
    out
}

fn char_props_attrs(props: &CharProps) -> String {
    let mut out = format!(
        " fo:font-size=\"{:.2}pt\" fo:color=\"#{}\"",
        props.size_pt,
        props.color.to_hex()
    );
    if props.bold {
        out.push_str(" fo:font-weight=\"bold\"");
    }
    if props.italic {
        out.push_str(" fo:font-style=\"italic\"");
    }
    if props.underline {
        out.push_str(" style:text-underline-style=\"solid\"");
    }
    if let Some(family) = &props.font_family {
        out.push_str(&format!(" fo:font-family=\"{}\"", escape_xml(family)));
    }
    if let Some(family) = &props.font_family_east_asian {
        out.push_str(&format!(
            " style:font-family-asian=\"{}\"",
            escape_xml(family)
        ));
    }
    if let Some(size) = props.size_complex_pt {
        out.push_str(&format!(" fo:font-size-asian=\"{size:.2}pt\""));
    }
    if props.bold_complex {
        out.push_str(" fo:font-weight-asian=\"bold\"");
    }
    if props.italic_complex {
        out.push_str(" fo:font-style-asian=\"italic\"");
    }
    if let Some(language) = &props.language {
        out.push_str(&format!(" fo:language=\"{}\"", escape_xml(language)));
    }
    if let Some(language) = &props.language_east_asian {
        out.push_str(&format!(
            " style:language-asian=\"{}\"",
            escape_xml(language)
        ));
    }
    out
}

fn para_props_attrs(props: &ParaProps) -> String {
    let align = match props.align {
        Align::Left => "start",
        Align::Center => "center",
        Align::Right => "end",
        Align::Justify => "justify",
    };
    let mut out = format!(" fo:text-align=\"{align}\"");
    if props.indent_left_pt != 0.0 {
        out.push_str(&format!(" fo:margin-left=\"{:.2}pt\"", props.indent_left_pt));
    }
    if props.indent_right_pt != 0.0 {
        out.push_str(&format!(
            " fo:margin-right=\"{:.2}pt\"",
            props.indent_right_pt
        ));
    }
    if props.indent_first_pt != 0.0 {
        out.push_str(&format!(" fo:text-indent=\"{:.2}pt\"", props.indent_first_pt));
    }
    if props.space_before_pt != 0.0 {
        out.push_str(&format!(" fo:margin-top=\"{:.2}pt\"", props.space_before_pt));
    }
    if props.space_after_pt != 0.0 {
        out.push_str(&format!(
            " fo:margin-bottom=\"{:.2}pt\"",
            props.space_after_pt
        ));
    }
    if (props.line_spacing - 1.0).abs() > f64::EPSILON {
        out.push_str(&format!(
            " fo:line-height=\"{:.1}%\"",
            props.line_spacing * 100.0
        ));
    }
    out
}

fn text_content(text: &str) -> String {
    let mut out = String::new();
    let mut spaces = 0usize;
    for ch in text.chars() {
        match ch {
            ' ' => spaces += 1,
            '\t' => {
                flush_spaces(&mut out, &mut spaces);
                out.push_str("<text:tab/>");
            }
            '\n' => {
                flush_spaces(&mut out, &mut spaces);
                out.push_str("<text:line-break/>");
            }
            '\r' => {}
            _ => {
                flush_spaces(&mut out, &mut spaces);
                out.push_str(&escape_xml(&ch.to_string()));
            }
        }
    }
    flush_spaces(&mut out, &mut spaces);
    out
}

fn flush_spaces(out: &mut String, spaces: &mut usize) {
    if *spaces == 0 {
        return;
    }
    if *spaces == 1 {
        out.push(' ');
    } else {
        out.push_str(&format!("<text:s text:c=\"{}\"/>", *spaces));
    }
    *spaces = 0;
}

fn table_grid(table: &Table, columns: usize) -> Vec<f64> {
    if table.grid.len() == columns && table.grid.iter().all(|width| *width > 0.0) {
        table.grid.clone()
    } else {
        let width = 400.0 / columns as f64;
        vec![width; columns]
    }
}

fn vertical_span(table: &Table, row_index: usize, column: usize) -> usize {
    let mut span = 1;
    for row in table.rows.iter().skip(row_index + 1) {
        let mut position = 0usize;
        let mut next = None;
        for cell in &row.cells {
            if position == column {
                next = Some(cell);
                break;
            }
            position += cell.span.max(1) as usize;
        }
        match next {
            Some(cell) if cell.vmerge == VMerge::Continue => span += 1,
            _ => break,
        }
    }
    span
}

fn column_letter(mut index: usize) -> String {
    let mut letters = String::new();
    loop {
        letters.insert(0, (b'A' + (index % 26) as u8) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    letters
}

#[cfg(test)]
mod tests {
    use super::*;
    use quire_base::{ImageId, ParaId, RunId, TableId};
    use quire_model::{
        FidelityFeature, HeaderFooter, NumberingRef, Section, Style, StyleKind, StyleSheet,
        TableCell,
    };
    use std::io::Read;

    fn zip_part(bytes: &[u8], name: &str) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut part = archive.by_name(name).unwrap();
        let mut text = String::new();
        part.read_to_string(&mut text).unwrap();
        text
    }

    fn has_part(bytes: &[u8], name: &str) -> bool {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        archive.by_name(name).is_ok()
    }

    fn paragraph_with(text: &str) -> Paragraph {
        let mut paragraph = Paragraph::new(ParaId::new(1));
        paragraph.push_text(text, CharProps::default());
        paragraph
    }

    #[test]
    fn writes_mimetype_first_and_uncompressed() {
        let mut document = Document::new();
        document.push_paragraph(paragraph_with("正文"));

        let bytes = bytes(&document).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes[..])).unwrap();
        let mut first = archive.by_index(0).unwrap();
        assert_eq!(first.name(), "mimetype");
        assert_eq!(
            first.compression(),
            zip::CompressionMethod::Stored,
            "ODF 1.2 §3.1 requires the mimetype member first and uncompressed"
        );
        let mut content = String::new();
        first.read_to_string(&mut content).unwrap();
        assert_eq!(content, MIMETYPE);
    }

    #[test]
    fn round_trips_text_and_char_formatting() {
        let mut document = Document::new();
        let mut paragraph = Paragraph::plain("你好 Quire");
        paragraph.runs[0].props.bold = true;
        paragraph.runs[0].props.size_pt = 14.0;
        document.push_paragraph(paragraph);

        let bytes = bytes(&document).unwrap();
        let read = crate::odt::read(&bytes).unwrap();
        assert_eq!(read.paragraph_count(), 1);
        let paragraph = read.paragraph(0, 0).unwrap();
        assert_eq!(paragraph.text(), "你好 Quire");
        assert!(paragraph.runs[0].props.bold);
        assert!((paragraph.runs[0].props.size_pt - 14.0).abs() < 1e-9);
    }

    #[test]
    fn round_trips_paragraph_properties() {
        let mut document = Document::new();
        let mut paragraph = Paragraph::plain("居中");
        paragraph.props.align = Align::Center;
        paragraph.props.indent_left_pt = 24.0;
        paragraph.props.space_after_pt = 6.0;
        paragraph.props.line_spacing = 1.5;
        document.push_paragraph(paragraph);

        let read = crate::odt::read(&bytes(&document).unwrap()).unwrap();
        let paragraph = read.paragraph(0, 0).unwrap();
        assert_eq!(paragraph.props.align, Align::Center);
        assert!((paragraph.props.indent_left_pt - 24.0).abs() < 1e-9);
        assert!((paragraph.props.space_after_pt - 6.0).abs() < 1e-9);
        assert!((paragraph.props.line_spacing - 1.5).abs() < 1e-9);
    }

    #[test]
    fn round_trips_tabs_breaks_and_spaces() {
        let mut document = Document::new();
        document.push_paragraph(paragraph_with("a\tb  c\nd"));
        document.push_paragraph(paragraph_with("single space stays literal"));

        let read = crate::odt::read(&bytes(&document).unwrap()).unwrap();
        assert_eq!(read.paragraph(0, 0).unwrap().text(), "a\tb  c\nd");
        assert_eq!(
            read.paragraph(0, 1).unwrap().text(),
            "single space stays literal"
        );
        let content = zip_part(&bytes(&document).unwrap(), "content.xml");
        assert!(content.contains("<text:tab/>"));
        assert!(content.contains("<text:line-break/>"));
        assert!(content.contains("<text:s text:c=\"2\"/>"));
    }

    #[test]
    fn escapes_markup_characters_in_text() {
        let mut document = Document::new();
        document.push_paragraph(paragraph_with(r#"a<b>&"c"'…"#));

        let content = zip_part(&bytes(&document).unwrap(), "content.xml");
        assert!(content.contains("a&lt;b&gt;&amp;&quot;c&quot;&apos;…"));

        let read = crate::odt::read(&bytes(&document).unwrap()).unwrap();
        assert_eq!(read.paragraph(0, 0).unwrap().text(), r#"a<b>&"c"'…"#);
    }

    #[test]
    fn refuses_numbering_until_forced() {
        let mut document = Document::new();
        let mut paragraph = Paragraph::plain("列表项");
        paragraph.numbering = Some(NumberingRef { num_id: 1, level: 0 });
        document.push_paragraph(paragraph);

        let error = bytes(&document).unwrap_err().to_string();
        assert!(error.contains("numbering"), "unexpected error: {error}");
        let forced = bytes_force(&document).unwrap();
        assert!(zip_part(&forced, "content.xml").contains("列表项"));
    }

    #[test]
    fn refuses_lossy_fidelity_until_forced() {
        let mut document = Document::new();
        document.push_paragraph(paragraph_with("有超链接"));
        document.note_fidelity(FidelityFeature::Hyperlinks);

        assert!(bytes(&document).is_err());
        let forced = bytes_force(&document).unwrap();
        assert!(zip_part(&forced, "content.xml").contains("有超链接"));
    }

    #[test]
    fn writes_tables_with_spans_and_merges() {
        let mut document = Document::new();
        let mut table = Table::new(TableId::new(1));
        table.grid = vec![100.0, 120.0, 140.0];

        let mut start = TableCell::default();
        start.blocks.push(Block::Paragraph(paragraph_with("跨行")));
        start.borders.top = Some(quire_model::Border {
            width_pt: 1.0,
            color: quire_model::Color::from_hex("D9480F").unwrap(),
        });

        let mut spanning = TableCell {
            span: 2,
            ..TableCell::default()
        };
        spanning.blocks.push(Block::Paragraph(paragraph_with("跨列")));

        let continued = TableCell {
            vmerge: VMerge::Continue,
            ..TableCell::default()
        };

        table.rows.push(TableRow {
            height_pt: None,
            header: false,
            cells: vec![start, spanning],
        });
        table.rows.push(TableRow {
            height_pt: None,
            header: false,
            cells: vec![continued],
        });
        document.first_section_mut().push_table(table);

        let bytes = bytes(&document).unwrap();
        let content = zip_part(&bytes, "content.xml");
        assert!(content.contains("table:number-columns-spanned=\"2\""));
        assert!(content.contains("table:number-rows-spanned=\"2\""));
        assert!(content.contains("<table:covered-table-cell/>"));
        assert!(content.contains("fo:border-top=\"1.00pt solid #D9480F\""));
        assert!(content.contains("style:column-width=\"120.00pt\""));
    }

    #[test]
    fn writes_headers_footers_and_page_fields() {
        let mut document = Document::new();
        let mut footer_paragraph = Paragraph::new(ParaId::new(2));
        let mut field_run = Run::new(RunId::new(1), "1", CharProps::default());
        field_run.field = Some(FieldKind::Page);
        footer_paragraph.runs.push(field_run);
        document.first_section_mut().footers.default = Some(HeaderFooter {
            blocks: vec![Block::Paragraph(footer_paragraph)],
        });
        document.first_section_mut().headers.default = Some(HeaderFooter {
            blocks: vec![Block::Paragraph(paragraph_with("页眉"))],
        });
        document.push_paragraph(paragraph_with("正文"));

        let styles = zip_part(&bytes(&document).unwrap(), "styles.xml");
        assert!(styles.contains("<style:master-page"));
        assert!(styles.contains("<style:header>"));
        assert!(styles.contains("<style:footer>"));
        assert!(styles.contains("页眉"));
        assert!(styles.contains("<text:page-number"));
    }

    #[test]
    fn writes_images_into_pictures() {
        const PNG_1X1: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0xfc, 0xcf, 0xc0, 0x50, 0x0f, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xa9,
            0x8c, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let mut document = Document::new();
        document.first_section_mut().push_image(ImageBlock::new(
            ImageId::new(1),
            "pixel.png",
            "image/png",
            PNG_1X1.to_vec(),
            72.0,
            72.0,
        ));

        let bytes = bytes(&document).unwrap();
        assert!(has_part(&bytes, "Pictures/image1.png"));
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes[..])).unwrap();
        let mut picture = archive.by_name("Pictures/image1.png").unwrap();
        let mut stored = Vec::new();
        picture.read_to_end(&mut stored).unwrap();
        assert_eq!(stored.as_slice(), PNG_1X1);

        let content = zip_part(&bytes, "content.xml");
        assert!(content.contains("xlink:href=\"Pictures/image1.png\""));
        assert!(content.contains("svg:width=\"72.00pt\""));
        let manifest = zip_part(&bytes, "META-INF/manifest.xml");
        assert!(manifest.contains("manifest:full-path=\"Pictures/image1.png\""));
    }

    #[test]
    fn writes_second_section_with_page_break() {
        let mut document = Document::new();
        document.push_paragraph(paragraph_with("第一节"));
        let mut second = Section::default();
        second.push_paragraph_block(paragraph_with("第二节"));
        document.push_section(second);

        let bytes = bytes(&document).unwrap();
        let content = zip_part(&bytes, "content.xml");
        assert!(content.contains("fo:break-before=\"page\""));
        assert!(content.contains("style:master-page-name=\"Standard1\""));
        let styles = zip_part(&bytes, "styles.xml");
        assert!(styles.contains("style:name=\"Standard1\""));
    }

    #[test]
    fn writes_named_styles() {
        let mut document = Document::new();
        let mut sheet = StyleSheet::new();
        let mut style = Style::new("Body", "Body Text", StyleKind::Paragraph);
        style.para = Some(quire_model::ParaProps {
            align: Align::Justify,
            ..Default::default()
        });
        sheet.insert(style);
        *document.styles_mut() = sheet;

        let styles = zip_part(&bytes(&document).unwrap(), "styles.xml");
        assert!(styles.contains("style:name=\"Body\""));
        assert!(styles.contains("style:display-name=\"Body Text\""));
        assert!(styles.contains("fo:text-align=\"justify\""));
    }

    #[test]
    fn interns_identical_styles_to_one_name() {
        let mut document = Document::new();
        for text in ["相同", "格式"] {
            let mut paragraph = Paragraph::plain(text);
            paragraph.runs[0].props.bold = true;
            document.push_paragraph(paragraph);
        }

        let content = zip_part(&bytes(&document).unwrap(), "content.xml");
        assert_eq!(content.matches("text:style-name=\"T1\"").count(), 2);
        assert!(!content.contains("T2"));
    }
}

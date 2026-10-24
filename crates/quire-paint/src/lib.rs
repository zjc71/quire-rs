//! Device-independent painting.
//!
//! Layout produces positioned text; this crate turns it into a backend-specific
//! representation. Milestone 1 ships the SVG backend, which is enough to
//! inspect layout in a browser and to drive golden screenshot tests. GPU
//! (vello) and PDF backends implement the same idea without changing layout.
//!
//! SVG output is a *structural preview*: each run carries `textLength`, so the
//! preview keeps the synthetic advances used during layout even if the viewer
//! substitutes a different font.

#![forbid(unsafe_code)]

use base64::Engine;
use quire_base::xml::escape_xml;
use quire_layout::{Layout, PageLayout};
use quire_model::{Border, Color};

/// Theme and preview options.
#[derive(Clone, Debug, PartialEq)]
pub struct SvgOptions {
    pub page_gap_pt: f64,
    pub viewport_margin_pt: f64,
    pub canvas_color: Color,
    pub page_color: Color,
    pub font_family: String,
    /// Paint the canvas background behind the pages.
    pub draw_canvas_background: bool,
    /// Force glyphs to the advances computed by layout.
    pub fit_text_to_layout: bool,
    pub debug_boxes: bool,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            page_gap_pt: 24.0,
            viewport_margin_pt: 32.0,
            canvas_color: Color::rgb(0xF3, 0xEF, 0xE8),
            page_color: Color::rgb(0xFF, 0xFF, 0xFF),
            font_family: "Georgia, 'Noto Serif CJK SC', 'Songti SC', serif".to_string(),
            fit_text_to_layout: true,
            draw_canvas_background: true,
            debug_boxes: false,
        }
    }
}

/// Render a layout as an SVG document.
pub fn render_svg(layout: &Layout, options: &SvgOptions) -> String {
    let width = layout
        .pages
        .iter()
        .map(|page| page.width_pt)
        .fold(0.0_f64, f64::max)
        + options.viewport_margin_pt * 2.0;
    let height = layout
        .pages
        .iter()
        .map(|page| page.height_pt)
        .sum::<f64>()
        + options.page_gap_pt * (layout.pages.len().saturating_sub(1) as f64)
        + options.viewport_margin_pt * 2.0;

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{width:.2}\" height=\"{height:.2}\" viewBox=\"0 0 {width:.2} {height:.2}\">"
    ));
    if options.draw_canvas_background {
        svg.push_str(&format!(
            "<rect x=\"0\" y=\"0\" width=\"{width:.2}\" height=\"{height:.2}\" fill=\"{}\"/>",
            options.canvas_color.to_css()
        ));
    }

    let mut y = options.viewport_margin_pt;
    for page in &layout.pages {
        let x = (width - page.width_pt) / 2.0;
        render_page(&mut svg, page, x, y, options);
        y += page.height_pt + options.page_gap_pt;
    }

    svg.push_str("</svg>");
    svg
}


fn render_border(
    svg: &mut String,
    border: Option<Border>,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
) {
    let Some(border) = border else {
        return;
    };
    svg.push_str(&format!(
        "<line x1=\"{x1:.2}\" y1=\"{y1:.2}\" x2=\"{x2:.2}\" y2=\"{y2:.2}\" stroke=\"{}\" stroke-width=\"{:.2}\"/>",
        border.color.to_css(),
        border.width_pt
    ));
}
fn run_font_family(run: &quire_layout::PlacedRun, fallback: &str) -> String {
    let mut families = Vec::new();
    if let Some(family) = &run.props.font_family {
        families.push(family.as_str());
    }
    if let Some(family) = &run.props.font_family_east_asian {
        families.push(family.as_str());
    }
    if families.is_empty() {
        return fallback.to_string();
    }
    families.push(fallback);
    families.join(", ")
}

fn render_page(svg: &mut String, page: &PageLayout, x: f64, y: f64, options: &SvgOptions) {
    svg.push_str(&format!(
        "<g transform=\"translate({x:.2},{y:.2})\">"
    ));
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{:.2}\" height=\"{:.2}\" fill=\"{}\" stroke=\"#DED8CC\" stroke-width=\"0.8\"/>",
        page.width_pt,
        page.height_pt,
        options.page_color.to_css()
    ));

    for cell in &page.cells {
        if let Some(color) = cell.shading {
            svg.push_str(&format!(
                "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"{}\"/>",
                cell.rect.x,
                cell.rect.y,
                cell.rect.width,
                cell.rect.height,
                color.to_css()
            ));
        }
    }

    for image in &page.images {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&image.data);
        svg.push_str(&format!(
            "<image x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" preserveAspectRatio=\"none\" xlink:href=\"data:{};base64,{}\"><title>{}</title></image>",
            image.rect.x,
            image.rect.y,
            image.rect.width,
            image.rect.height,
            escape_xml(&image.mime),
            encoded,
            escape_xml(&image.name)
        ));
    }

    for cell in &page.cells {
        let right = cell.rect.right();
        let bottom = cell.rect.bottom();
        render_border(svg, cell.borders.top, cell.rect.x, cell.rect.y, right, cell.rect.y);
        render_border(svg, cell.borders.bottom, cell.rect.x, bottom, right, bottom);
        render_border(svg, cell.borders.left, cell.rect.x, cell.rect.y, cell.rect.x, bottom);
        render_border(svg, cell.borders.right, right, cell.rect.y, right, bottom);
    }

    for line in &page.lines {
        if options.debug_boxes {
            svg.push_str(&format!(
                "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"none\" stroke=\"#F0A080\" stroke-width=\"0.5\"/>",
                line.x, line.y, line.width, line.height
            ));
        }
        for run in &line.runs {
            if run.text.is_empty() {
                continue;
            }
            let mut styles = String::new();
            if run.props.bold {
                styles.push_str(" font-weight=\"700\"");
            }
            if run.props.italic {
                styles.push_str(" font-style=\"italic\"");
            }
            if run.props.underline {
                styles.push_str(" text-decoration=\"underline\"");
            }
            let font_family = run_font_family(run, &options.font_family);
            let fit = if options.fit_text_to_layout {
                format!(
                    " textLength=\"{:.2}\" lengthAdjust=\"spacingAndGlyphs\"",
                    run.width
                )
            } else {
                String::new()
            };
            svg.push_str(&format!(
                "<text x=\"{:.2}\" y=\"{:.2}\" font-family=\"{}\" font-size=\"{:.2}\" fill=\"{}\" xml:space=\"preserve\"{}{}>{}</text>",
                run.x,
                line.baseline,
                escape_xml(&font_family),
                run.props.size_pt,
                run.props.color.to_css(),
                fit,
                styles,
                escape_xml(&run.text)
            ));
        }
    }

    svg.push_str("</g>");
}

/// Render one page as a standalone SVG document.
pub fn render_svg_page(
    layout: &Layout,
    index: usize,
    options: &SvgOptions,
) -> Option<String> {
    let page = layout.pages.get(index)?;
    let single = Layout {
        width_pt: page.width_pt,
        pages: vec![page.clone()],
    };
    Some(render_svg(&single, options))
}

/// Render every page as a separate SVG document.
pub fn render_svg_pages(layout: &Layout, options: &SvgOptions) -> Vec<String> {
    layout
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let single = Layout {
                width_pt: page.width_pt,
                pages: vec![page.clone()],
            };
            let mut svg = render_svg(&single, options);
            // Annotate for callers that write one file per page.
            svg = svg.replacen(
                "<svg ",
                &format!("<svg data-page=\"{}\" ", index + 1),
                1,
            );
            svg
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use quire_base::{ImageId, TableId};
    use quire_layout::{LayoutOptions, layout_deterministic};
    use quire_model::{
        Block, Border, Color, Document, ImageBlock, Paragraph, Table, TableBorders, TableCell,
        TableRow,
    };

    #[test]
    fn renders_text_and_page() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Hello 世界"));
        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let svg = render_svg(&layout, &SvgOptions::default());

        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("Hello 世界"));
        assert!(svg.contains("textLength"));
        assert!(svg.contains("<rect"));
    }

    #[test]
    fn renders_embedded_images() {
        let mut doc = Document::new();
        doc.first_section_mut().push_image(ImageBlock::new(
            ImageId::new(1),
            "pixel.png",
            "image/png",
            vec![1, 2, 3],
            10.0,
            10.0,
        ));
        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let svg = render_svg(&layout, &SvgOptions::default());
        assert!(svg.contains("<image"));
        assert!(svg.contains("data:image/png;base64,"));
    }

    #[test]
    fn escapes_markup_in_text() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("a < b & c"));
        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let svg = render_svg(&layout, &SvgOptions::default());
        assert!(svg.contains("a &lt; b &amp; c"));
        assert!(!svg.contains("a < b & c"));
    }

    #[test]
    fn renders_table_borders_and_text() {
        let mut doc = Document::new();
        let mut table = Table::new(TableId::new(1));
        table.grid = vec![80.0, 80.0];
        table.borders = TableBorders {
            top: Some(Border::new(0.5, Color::BLACK)),
            left: Some(Border::new(0.5, Color::BLACK)),
            ..TableBorders::default()
        };
        let mut row = TableRow::default();
        let mut cell = TableCell::default();
        cell.blocks.push(Block::Paragraph(Paragraph::plain("cell")));
        row.cells.push(cell);
        table.rows.push(row);
        doc.first_section_mut().push_table(table);

        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let svg = render_svg(&layout, &SvgOptions::default());
        assert!(svg.contains("<line"));
        assert!(svg.contains("cell"));
    }

    #[test]
    fn uses_run_font_family() {
        let mut doc = Document::new();
        let mut paragraph = Paragraph::plain("Font");
        paragraph.runs[0].props.font_family = Some("Georgia".to_string());
        paragraph.runs[0].props.font_family_east_asian = Some("SimSun".to_string());
        doc.push_paragraph(paragraph);

        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let svg = render_svg(&layout, &SvgOptions::default());
        assert!(svg.contains("font-family=\"Georgia, SimSun"));
    }

}

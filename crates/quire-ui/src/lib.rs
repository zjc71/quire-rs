//! Native viewer support: rasterizing laid-out pages for display.

#![forbid(unsafe_code)]

/// A rasterized page in RGBA order (premultiplied, as produced by tiny-skia).
#[derive(Clone, Debug)]
pub struct RasterPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl RasterPage {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * self.width + x) * 4) as usize;
        [
            self.rgba[index],
            self.rgba[index + 1],
            self.rgba[index + 2],
            self.rgba[index + 3],
        ]
    }
}

/// Reusable SVG rasterizer with system fonts loaded once.
pub struct Rasterizer {
    options: usvg::Options<'static>,
}

impl Rasterizer {
    pub fn new() -> Self {
        let mut options = usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        Self { options }
    }

    pub fn rasterize(&self, svg: &str, scale: f32) -> Result<RasterPage, String> {
        let scale = scale.max(0.05);
        let tree = usvg::Tree::from_str(svg, &self.options)
            .map_err(|error| format!("cannot parse page SVG: {error}"))?;
        let width = ((tree.size().width() * scale).ceil() as u32).max(1);
        let height = ((tree.size().height() * scale).ceil() as u32).max(1);
        let mut pixmap =
            tiny_skia::Pixmap::new(width, height).ok_or_else(|| "cannot allocate page".to_string())?;
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        Ok(RasterPage {
            width,
            height,
            rgba: pixmap.data().to_vec(),
        })
    }
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quire_layout::{LayoutOptions, layout_deterministic};
    use quire_model::{Document, Paragraph};
    use quire_paint::{SvgOptions, render_svg_page};

    #[test]
    fn rasterizes_a_page() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Hello viewer"));
        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let svg = render_svg_page(&layout, 0, &SvgOptions::default()).unwrap();

        let page = Rasterizer::new().rasterize(&svg, 1.0).unwrap();
        assert!(page.width > 0);
        assert!(page.height > 0);
        assert_eq!(page.rgba.len(), (page.width * page.height * 4) as usize);
        assert!(page.rgba.iter().any(|byte| *byte != 0));
    }
}

//! Deterministic line breaking and pagination.
//!
//! Input is an immutable document snapshot plus view parameters; output is a
//! fully positioned layout tree. No fonts are loaded and no platform API is
//! touched, so the same input always produces byte-identical output.
//!
//! Current limits (deliberate, documented):
//! * paragraphs only — tables, floats and images arrive in later milestones;
//! * a simple greedy line breaker with basic East Asian punctuation rules;
//! * no widow/orphan control yet;
//! * synthetic metrics unless the caller supplies another [`Metrics`].

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use quire_base::{Pt, Rect, Result, RunId};
use quire_model::{
    Align, Block, CellBorders, CharProps, Document, ImageBlock, NumberFormat, Numbering,
    NumberingLevel, NumberingRef, Paragraph, Run, Section, Table, TableCell, VMerge,
};
use quire_text::{is_closing_punct, tokenize, Metrics, TextStyle, Token};

/// How the document is laid out.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// Fixed paper size with page breaks.
    #[default]
    Page,
    /// Single continuous flow at the viewport width.
    Web,
}

/// Layout parameters.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LayoutOptions {
    pub view: ViewMode,
    /// Viewport width for [`ViewMode::Web`]; ignored in page mode.
    pub viewport_width_pt: Option<f64>,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            view: ViewMode::Page,
            viewport_width_pt: None,
        }
    }
}

/// A positioned text run.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedRun {
    pub x: f64,
    pub width: f64,
    pub text: String,
    pub props: CharProps,
}

/// A single laid out line.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub page: usize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub baseline: f64,
    pub paragraph: u32,
    pub runs: Vec<PlacedRun>,
}

/// A positioned image.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedImage {
    pub rect: Rect,
    pub name: String,
    pub mime: String,
    pub data: Arc<[u8]>,
}

/// A positioned table cell.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedCell {
    pub rect: Rect,
    pub borders: CellBorders,
    pub shading: Option<quire_model::Color>,
}

/// One page of output.
#[derive(Clone, Debug, PartialEq)]
pub struct PageLayout {
    pub width_pt: f64,
    pub height_pt: f64,
    pub lines: Vec<Line>,
    pub images: Vec<PlacedImage>,
    pub cells: Vec<PlacedCell>,
}

/// Complete layout result.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub width_pt: f64,
    pub pages: Vec<PageLayout>,
}

impl Layout {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn line_count(&self) -> usize {
        self.pages.iter().map(|page| page.lines.len()).sum()
    }
}

#[derive(Clone, Debug)]
struct StyledToken {
    kind: Token,
    text: String,
    props: CharProps,
    width: f64,
    space: bool,
}

#[derive(Clone, Debug, Default)]
struct DraftLine {
    runs: Vec<StyledToken>,
    width: f64,
    height: f64,
    ascent: f64,
}

impl DraftLine {
    fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

/// Reusable paragraph layout cache.
#[derive(Default)]
pub struct LayoutCache {
    entries: HashMap<u64, Arc<Vec<DraftLine>>>,
    hits: u64,
    misses: u64,
}

impl LayoutCache {
    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn misses(&self) -> u64 {
        self.misses
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn hash_f64(value: f64, hasher: &mut DefaultHasher) {
    value.to_bits().hash(hasher);
}

fn hash_char_props(props: &CharProps, hasher: &mut DefaultHasher) {
    props.bold.hash(hasher);
    props.italic.hash(hasher);
    props.underline.hash(hasher);
    props.bold_complex.hash(hasher);
    props.italic_complex.hash(hasher);
    hash_f64(props.size_pt, hasher);
    props.color.r.hash(hasher);
    props.color.g.hash(hasher);
    props.color.b.hash(hasher);
    props.font_family.hash(hasher);
    props.font_family_east_asian.hash(hasher);
    props.size_complex_pt.map(f64::to_bits).hash(hasher);
    props.language.hash(hasher);
    props.language_east_asian.hash(hasher);
}

fn paragraph_cache_key(
    paragraph: &Paragraph,
    first_width: f64,
    rest_width: f64,
    metrics: &dyn Metrics,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    paragraph.id.get().hash(&mut hasher);
    paragraph.text().hash(&mut hasher);
    for run in &paragraph.runs {
        hash_char_props(&run.props, &mut hasher);
        run.field.is_some().hash(&mut hasher);
    }
    let align = match paragraph.props.align {
        Align::Left => 0u8,
        Align::Center => 1,
        Align::Right => 2,
        Align::Justify => 3,
    };
    align.hash(&mut hasher);
    for value in [
        paragraph.props.indent_left_pt,
        paragraph.props.indent_right_pt,
        paragraph.props.indent_first_pt,
        paragraph.props.space_before_pt,
        paragraph.props.space_after_pt,
        paragraph.props.line_spacing,
        first_width,
        rest_width,
    ] {
        hash_f64(value, &mut hasher);
    }
    metrics.cache_key().hash(&mut hasher);
    hasher.finish()
}

/// Lay out a document with a fresh paragraph cache.
pub fn layout(
    document: &Document,
    options: &LayoutOptions,
    metrics: &dyn Metrics,
) -> Result<Layout> {
    let mut cache = LayoutCache::default();
    layout_with_cache(document, options, metrics, &mut cache)
}

/// Lay out a document, reusing unchanged paragraph line breaks.
pub fn layout_with_cache(
    document: &Document,
    options: &LayoutOptions,
    metrics: &dyn Metrics,
    cache: &mut LayoutCache,
) -> Result<Layout> {
    document.validate()?;

    let mut pages: Vec<PageLayout> = Vec::new();
    let mut width_pt = options.viewport_width_pt.unwrap_or(794.0);

    let mut numbering = NumberingState::new(document.numbering());
    let even_odd = document.even_and_odd_headers();
    let mut ranges: Vec<(usize, usize, usize, Geometry)> = Vec::new();

    for (section_index, section) in document.sections().iter().enumerate() {
        let geometry = Geometry::new(section, options);
        width_pt = geometry.page_width;

        let first_page = pages.len();
        let mut cursor = PageCursor::new(&geometry);
        for block in &section.blocks {
            match block {
                Block::Paragraph(paragraph) => {
                    layout_paragraph(
                        paragraph,
                        &geometry,
                        metrics,
                        &mut pages,
                        &mut cursor,
                        &mut numbering,
                        Some(cache),
                    );
                }
                Block::Image(image) => {
                    layout_image(image, &geometry, &mut pages, &mut cursor);
                }
                Block::Table(table) => {
                    layout_table(
                        table,
                        &geometry,
                        metrics,
                        &mut pages,
                        &mut cursor,
                        &mut numbering,
                    );
                }
            }
        }
        pages.push(cursor.page);
        ranges.push((section_index, first_page, pages.len(), geometry));
    }

    let total_pages = pages.len() as u32;
    for (section_index, first_page, last_page, geometry) in ranges {
        let section = &document.sections()[section_index];
        for (page_index, page) in pages
            .iter_mut()
            .enumerate()
            .skip(first_page)
            .take(last_page - first_page)
        {
            let page_number = page_index as u32 + 1;
            apply_story(
                &section.headers,
                page,
                page_index,
                page_number,
                total_pages,
                section.title_page,
                even_odd,
                Slot::Header,
                &geometry,
                document.numbering(),
                metrics,
            );
            apply_story(
                &section.footers,
                page,
                page_index,
                page_number,
                total_pages,
                section.title_page,
                even_odd,
                Slot::Footer,
                &geometry,
                document.numbering(),
                metrics,
            );
        }
    }

    if options.view == ViewMode::Web {
        for page in &mut pages {
            let content_bottom = page
                .lines
                .iter()
                .map(|line| line.y + line.height)
                .chain(page.images.iter().map(|image| image.rect.bottom()))
                .chain(page.cells.iter().map(|cell| cell.rect.bottom()))
                .fold(0.0_f64, f64::max);
            page.height_pt = (content_bottom + 48.0).max(200.0);
        }
    }

    if pages.is_empty() {
        pages.push(PageLayout {
            width_pt,
            height_pt: 842.0,
            lines: Vec::new(),
            images: Vec::new(),
            cells: Vec::new(),
        });
    }

    Ok(Layout { width_pt, pages })
}

struct StoryLayout {
    lines: Vec<Line>,
    images: Vec<PlacedImage>,
    cells: Vec<PlacedCell>,
    height: f64,
}

#[derive(Copy, Clone)]
enum Slot {
    Header,
    Footer,
}

#[derive(Copy, Clone, Debug, Default)]
struct FieldContext {
    page_number: u32,
    total_pages: u32,
}

fn resolve_fields(blocks: &[Block], context: FieldContext) -> Vec<Block> {
    blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph(paragraph) => {
                let mut paragraph = paragraph.clone();
                for run in &mut paragraph.runs {
                    if let Some(field) = run.field {
                        run.text = field.value(context.page_number, context.total_pages);
                        run.field = None;
                    }
                }
                Block::Paragraph(paragraph)
            }
            Block::Image(image) => Block::Image(image.clone()),
            Block::Table(table) => {
                let mut table = table.clone();
                for row in &mut table.rows {
                    for cell in &mut row.cells {
                        cell.blocks = resolve_fields(&cell.blocks, context);
                    }
                }
                Block::Table(table)
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn apply_story(
    set: &quire_model::HeaderFooterSet,
    page: &mut PageLayout,
    page_index: usize,
    page_number: u32,
    total_pages: u32,
    title_page: bool,
    even_odd: bool,
    slot: Slot,
    geometry: &Geometry,
    numbering: &Numbering,
    metrics: &dyn Metrics,
) {
    let Some(item) = set.for_page(page_number, title_page, even_odd) else {
        return;
    };
    let story = layout_story(
        &item.blocks,
        numbering,
        geometry,
        metrics,
        FieldContext {
            page_number,
            total_pages,
        },
    );
    if story.lines.is_empty() && story.images.is_empty() && story.cells.is_empty() {
        return;
    }

    let offset_y = match slot {
        Slot::Header => 36.0,
        Slot::Footer => (page.height_pt - 36.0 - story.height).max(0.0),
    };
    let offset_x = geometry.content_left;

    for line in &story.lines {
        let mut line = line.clone();
        line.x += offset_x;
        line.y += offset_y;
        line.baseline += offset_y;
        line.page = page_index;
        page.lines.push(line);
    }
    for image in &story.images {
        let mut image = image.clone();
        image.rect.x += offset_x;
        image.rect.y += offset_y;
        page.images.push(image);
    }
    for cell in &story.cells {
        let mut cell = cell.clone();
        cell.rect.x += offset_x;
        cell.rect.y += offset_y;
        page.cells.push(cell);
    }
}

fn layout_story(
    blocks: &[Block],
    numbering: &Numbering,
    parent: &Geometry,
    metrics: &dyn Metrics,
    context: FieldContext,
) -> StoryLayout {
    let blocks = resolve_fields(blocks, context);
    let width = parent.content_width.max(24.0);
    let geometry = Geometry {
        page_width: width,
        page_height: f64::INFINITY,
        content_left: 0.0,
        content_top: 0.0,
        content_width: width,
        content_bottom: f64::INFINITY,
    };
    let mut pages: Vec<PageLayout> = Vec::new();
    let mut cursor = PageCursor::new(&geometry);
    let mut state = NumberingState::new(numbering);

    for block in &blocks {
        match block {
            Block::Paragraph(paragraph) => {
                layout_paragraph(
                    paragraph,
                    &geometry,
                    metrics,
                    &mut pages,
                    &mut cursor,
                    &mut state,
                    None,
                );
            }
            Block::Image(image) => {
                layout_image(image, &geometry, &mut pages, &mut cursor);
            }
            Block::Table(table) => {
                layout_table(
                    table,
                    &geometry,
                    metrics,
                    &mut pages,
                    &mut cursor,
                    &mut state,
                );
            }
        }
    }

    let page = cursor.page;
    let height = page
        .lines
        .iter()
        .map(|line| line.y + line.height)
        .chain(page.images.iter().map(|image| image.rect.bottom()))
        .chain(page.cells.iter().map(|cell| cell.rect.bottom()))
        .fold(0.0_f64, f64::max);

    StoryLayout {
        lines: page.lines,
        images: page.images,
        cells: page.cells,
        height,
    }
}

#[derive(Copy, Clone)]
struct Geometry {
    page_width: f64,
    page_height: f64,
    content_left: f64,
    content_top: f64,
    content_width: f64,
    content_bottom: f64,
}

impl Geometry {
    fn new(section: &Section, options: &LayoutOptions) -> Self {
        match options.view {
            ViewMode::Page => {
                let margins = section.props.margins;
                let page_width = section.props.page.width_pt;
                let page_height = section.props.page.height_pt;
                Self {
                    page_width,
                    page_height,
                    content_left: margins.left_pt,
                    content_top: margins.top_pt,
                    content_width: (page_width - margins.left_pt - margins.right_pt).max(24.0),
                    content_bottom: page_height - margins.bottom_pt,
                }
            }
            ViewMode::Web => {
                let page_width = options.viewport_width_pt.unwrap_or(794.0);
                Self {
                    page_width,
                    page_height: f64::INFINITY,
                    content_left: 48.0,
                    content_top: 48.0,
                    content_width: (page_width - 96.0).max(24.0),
                    content_bottom: f64::INFINITY,
                }
            }
        }
    }
}

struct PageCursor {
    page: PageLayout,
    y: f64,
}

impl PageCursor {
    fn new(geometry: &Geometry) -> Self {
        Self {
            page: PageLayout {
                width_pt: geometry.page_width,
                height_pt: geometry.page_height,
                lines: Vec::new(),
                images: Vec::new(),
                cells: Vec::new(),
            },
            y: geometry.content_top,
        }
    }

    fn ensure_room(&mut self, geometry: &Geometry, needed: f64, pages: &mut Vec<PageLayout>) {
        if self.y + needed > geometry.content_bottom
            && (!self.page.lines.is_empty()
                || !self.page.images.is_empty()
                || !self.page.cells.is_empty())
        {
            let finished = std::mem::replace(
                &mut self.page,
                PageLayout {
                    width_pt: geometry.page_width,
                    height_pt: geometry.page_height,
                    lines: Vec::new(),
                    images: Vec::new(),
                    cells: Vec::new(),
                },
            );
            pages.push(finished);
            self.y = geometry.content_top;
        }
    }
}

/// Per-document list counters.
struct NumberingState<'a> {
    numbering: &'a Numbering,
    counters: BTreeMap<u32, Vec<u32>>,
    last_num: Option<u32>,
}

impl<'a> NumberingState<'a> {
    fn new(numbering: &'a Numbering) -> Self {
        Self {
            numbering,
            counters: BTreeMap::new(),
            last_num: None,
        }
    }

    fn label(&mut self, reference: NumberingRef) -> Option<String> {
        let level = self
            .numbering
            .level(reference.num_id, reference.level)
            .cloned()?;
        let starts: Vec<u32> = (0..9)
            .map(|index| {
                self.numbering
                    .start(reference.num_id, index as u8)
                    .unwrap_or(1)
                    .max(1)
            })
            .collect();

        if self.last_num != Some(reference.num_id) {
            self.counters.insert(
                reference.num_id,
                starts.iter().map(|start| start - 1).collect(),
            );
            self.last_num = Some(reference.num_id);
        }

        let counters = self.counters.entry(reference.num_id).or_default();
        if counters.len() < 9 {
            counters.resize(9, 0);
        }

        let index = reference.level as usize;
        if index >= counters.len() {
            return None;
        }
        counters[index] = counters[index].max(starts[index] - 1) + 1;
        for deeper in index + 1..9 {
            counters[deeper] = starts[deeper] - 1;
        }

        Some(render_label(
            &level,
            counters,
            self.numbering,
            reference.num_id,
        ))
    }
}

fn render_label(
    level: &NumberingLevel,
    counters: &[u32],
    numbering: &Numbering,
    num_id: u32,
) -> String {
    if level.format == NumberFormat::None {
        return String::new();
    }
    if level.format == NumberFormat::Bullet {
        let first = level.text.chars().next().unwrap_or('•');
        return if (first as u32) >= 0xF000 || level.text.is_empty() {
            "•".to_string()
        } else {
            level.text.clone()
        };
    }

    let characters: Vec<char> = level.text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '%' && index + 1 < characters.len() {
            if let Some(digit) = characters[index + 1].to_digit(10) {
                if (1..=9).contains(&digit) {
                    let slot = (digit - 1) as usize;
                    let value = counters.get(slot).copied().unwrap_or(0);
                    let format = numbering
                        .level(num_id, digit as u8 - 1)
                        .map(|level| level.format)
                        .unwrap_or(NumberFormat::Decimal);
                    out.push_str(&format.label(value));
                    index += 2;
                    continue;
                }
            }
        }
        out.push(characters[index]);
        index += 1;
    }
    out
}

fn numbered_paragraph<'a>(
    paragraph: &'a Paragraph,
    numbering: &mut NumberingState,
) -> Cow<'a, Paragraph> {
    let Some(reference) = paragraph.numbering else {
        return Cow::Borrowed(paragraph);
    };
    let Some(level) = numbering
        .numbering
        .level(reference.num_id, reference.level)
        .cloned()
    else {
        return Cow::Borrowed(paragraph);
    };
    let Some(label) = numbering.label(reference) else {
        return Cow::Borrowed(paragraph);
    };
    if label.is_empty() {
        return Cow::Borrowed(paragraph);
    }

    let mut copy = paragraph.clone();
    let props = copy
        .runs
        .first()
        .map(|run| run.props.clone())
        .unwrap_or_default();
    copy.runs.insert(
        0,
        Run::new(
            RunId::new(0),
            format!("{label}{}", level.suffix.text()),
            props,
        ),
    );
    if copy.props.indent_left_pt == 0.0 && copy.props.indent_first_pt == 0.0 {
        copy.props.indent_left_pt = level.indent_left_pt;
        copy.props.indent_first_pt = -level.hanging_pt;
    }
    Cow::Owned(copy)
}

#[allow(clippy::too_many_arguments)]
fn layout_paragraph(
    paragraph: &Paragraph,
    geometry: &Geometry,
    metrics: &dyn Metrics,
    pages: &mut Vec<PageLayout>,
    cursor: &mut PageCursor,
    numbering: &mut NumberingState,
    cache: Option<&mut LayoutCache>,
) {
    let prepared = numbered_paragraph(paragraph, numbering);
    let paragraph = prepared.as_ref();
    let tokens = build_tokens(paragraph, metrics);
    let base_width = geometry.content_width
        - paragraph.props.indent_left_pt
        - paragraph.props.indent_right_pt;
    let first_width = (base_width - paragraph.props.indent_first_pt).max(24.0);
    let rest_width = base_width.max(24.0);

    let drafts: Arc<Vec<DraftLine>> = if paragraph.numbering.is_some() {
        Arc::new(break_paragraph(tokens, first_width, rest_width, metrics))
    } else if let Some(cache) = cache {
        let key = paragraph_cache_key(paragraph, first_width, rest_width, metrics);
        if let Some(drafts) = cache.entries.get(&key) {
            cache.hits += 1;
            drafts.clone()
        } else {
            let drafts = Arc::new(break_paragraph(tokens, first_width, rest_width, metrics));
            cache.misses += 1;
            cache.entries.insert(key, drafts.clone());
            if cache.entries.len() > 4096 {
                cache.entries.clear();
            }
            drafts
        }
    } else {
        Arc::new(break_paragraph(tokens, first_width, rest_width, metrics))
    };
    let line_count = drafts.len();

    if line_count > 0 {
        cursor.y += paragraph.props.space_before_pt;
    }

    for (index, draft) in drafts.iter().enumerate() {
        let is_first = index == 0;
        let is_last = index + 1 == line_count;
        let indent = paragraph.props.indent_left_pt
            + if is_first {
                paragraph.props.indent_first_pt
            } else {
                0.0
            };
        let available = if is_first { first_width } else { rest_width };

        cursor.ensure_room(geometry, draft.height, pages);

        let start_x = geometry.content_left + indent;
        let mut runs = position_runs(draft, paragraph, start_x, available, is_last);

        if paragraph.props.align != Align::Left && !runs.is_empty() {
            let used: f64 = runs.iter().map(|run| run.width).sum();
            let offset = match paragraph.props.align {
                Align::Center => (available - used).max(0.0) / 2.0,
                Align::Right => (available - used).max(0.0),
                Align::Justify | Align::Left => 0.0,
            };
            for run in &mut runs {
                run.x += offset;
            }
        }

        let line = Line {
            page: pages.len(),
            x: start_x,
            y: cursor.y,
            width: draft.width,
            height: draft.height,
            baseline: cursor.y + draft.ascent,
            paragraph: paragraph.id.get(),
            runs,
        };
        cursor.page.lines.push(line);
        cursor.y += draft.height;
    }

    if line_count > 0 {
        cursor.y += paragraph.props.space_after_pt;
    }
}


struct MeasuredCell {
    index: usize,
    column: usize,
    span: usize,
    x: f64,
    width: f64,
    lines: Vec<Line>,
    images: Vec<PlacedImage>,
    height: f64,
}

fn layout_table(
    table: &Table,
    geometry: &Geometry,
    metrics: &dyn Metrics,
    pages: &mut Vec<PageLayout>,
    cursor: &mut PageCursor,
    numbering: &mut NumberingState,
) {
    let columns = table.column_count().max(1);
    let widths = column_widths(table, geometry.content_width);
    let gap = 6.0;

    if !cursor.page.lines.is_empty()
        || !cursor.page.images.is_empty()
        || !cursor.page.cells.is_empty()
    {
        cursor.y += gap;
    }

    for (row_index, row) in table.rows.iter().enumerate() {
        let mut measured: Vec<MeasuredCell> = Vec::new();
        let mut column = 0usize;

        for (index, cell) in row.cells.iter().enumerate() {
            let span = cell.span.max(1) as usize;
            let width: f64 = (0..span)
                .map(|offset| widths.get(column + offset).copied().unwrap_or(0.0))
                .sum();
            let x = geometry.content_left
                + widths.iter().take(column).sum::<f64>();

            let (lines, images, content_height) =
                if matches!(cell.vmerge, VMerge::Continue) {
                    (Vec::new(), Vec::new(), 0.0)
                } else {
                    let inner = (width - 2.0 * cell.padding_pt).max(12.0);
                    layout_cell(cell, x + cell.padding_pt, inner, metrics, numbering)
                };

            let height = (content_height + 2.0 * cell.padding_pt)
                .max(row.height_pt.unwrap_or(0.0))
                .max(12.0);

            measured.push(MeasuredCell {
                index,
                column,
                span,
                x,
                width,
                lines,
                images,
                height,
            });
            column += span;
        }

        let row_height = measured
            .iter()
            .map(|cell| cell.height)
            .fold(0.0_f64, f64::max)
            .max(12.0);

        cursor.ensure_room(geometry, row_height, pages);
        let row_y = cursor.y;
        let page_index = pages.len();
        let last_row = row_index + 1 == table.rows.len();

        let mut new_lines: Vec<Line> = Vec::new();
        let mut new_images: Vec<PlacedImage> = Vec::new();
        let mut new_cells: Vec<PlacedCell> = Vec::new();

        for item in measured {
            let Some(cell) = row.cells.get(item.index) else {
                continue;
            };
            for mut line in item.lines {
                line.page = page_index;
                let offset = row_y + cell.padding_pt;
                line.y += offset;
                line.baseline += offset;
                new_lines.push(line);
            }
            for mut image in item.images {
                image.rect.y += row_y + cell.padding_pt;
                new_images.push(image);
            }

            new_cells.push(PlacedCell {
                rect: Rect::new(item.x, row_y, item.width, row_height),
                borders: effective_borders(
                    table, cell, row_index, item.column, item.span, columns, last_row,
                ),
                shading: cell.shading,
            });
        }

        cursor.page.lines.append(&mut new_lines);
        cursor.page.images.append(&mut new_images);
        cursor.page.cells.append(&mut new_cells);
        cursor.y += row_height;
    }

    cursor.y += gap;
}

fn column_widths(table: &Table, content_width: f64) -> Vec<f64> {
    let columns = table.column_count().max(1);
    let mut widths = vec![0.0; columns];

    if table.grid.len() >= columns {
        widths.copy_from_slice(&table.grid[..columns]);
    } else {
        widths[..table.grid.len()].copy_from_slice(&table.grid);
        let missing = columns - table.grid.len();
        let used: f64 = widths.iter().sum();
        let each = ((content_width - used) / missing as f64).max(24.0);
        widths[table.grid.len()..].fill(each);
    }

    let total: f64 = widths.iter().sum();
    if total > content_width && total > 0.0 {
        let scale = content_width / total;
        for width in &mut widths {
            *width *= scale;
        }
    } else if total <= 0.0 {
        widths.fill(content_width / columns as f64);
    }

    widths
}

fn effective_borders(
    table: &Table,
    cell: &TableCell,
    row_index: usize,
    column: usize,
    span: usize,
    columns: usize,
    last_row: bool,
) -> CellBorders {
    let borders = &table.borders;
    let last_column = column + span >= columns;
    let continue_merge = matches!(cell.vmerge, VMerge::Continue);

    CellBorders {
        top: if continue_merge {
            None
        } else {
            cell.borders.top.or(if row_index == 0 {
                borders.top
            } else {
                borders.inside_horizontal
            })
        },
        bottom: cell
            .borders
            .bottom
            .or(if last_row {
                borders.bottom
            } else {
                borders.inside_horizontal
            }),
        left: cell.borders.left.or(if column == 0 {
            borders.left
        } else {
            borders.inside_vertical
        }),
        right: cell.borders.right.or(if last_column {
            borders.right
        } else {
            borders.inside_vertical
        }),
    }
}

fn layout_cell(
    cell: &TableCell,
    x: f64,
    width: f64,
    metrics: &dyn Metrics,
    numbering: &mut NumberingState,
) -> (Vec<Line>, Vec<PlacedImage>, f64) {
    let mut lines = Vec::new();
    let mut images = Vec::new();
    let mut y = 0.0;

    for block in &cell.blocks {
        match block {
            Block::Paragraph(paragraph) => {
                let prepared = numbered_paragraph(paragraph, numbering);
                let (mut paragraph_lines, height) =
                    layout_paragraph_at(prepared.as_ref(), x, width, y, metrics);
                lines.append(&mut paragraph_lines);
                y += height;
            }
            Block::Image(image) => {
                let scale = (width / image.width_pt).min(1.0);
                let image_width = (image.width_pt * scale).max(1.0);
                let image_height = (image.height_pt * scale).max(1.0);
                if y > 0.0 {
                    y += 4.0;
                }
                images.push(PlacedImage {
                    rect: Rect::new(
                        x + (width - image_width) / 2.0,
                        y,
                        image_width,
                        image_height,
                    ),
                    name: image.name.clone(),
                    mime: image.mime.clone(),
                    data: image.data.clone(),
                });
                y += image_height + 4.0;
            }
            Block::Table(_) => {
                // Nested tables are read but not laid out yet.
            }
        }
    }

    (lines, images, y)
}

fn layout_paragraph_at(
    paragraph: &Paragraph,
    x: f64,
    width: f64,
    y: f64,
    metrics: &dyn Metrics,
) -> (Vec<Line>, f64) {
    let base = (width - paragraph.props.indent_left_pt - paragraph.props.indent_right_pt).max(12.0);
    let first = (base - paragraph.props.indent_first_pt).max(12.0);
    let rest = base.max(12.0);

    let drafts = break_paragraph(build_tokens(paragraph, metrics), first, rest, metrics);
    let count = drafts.len();
    let mut cursor = y + if count > 0 {
        paragraph.props.space_before_pt
    } else {
        0.0
    };
    let mut lines = Vec::with_capacity(count);

    for (index, draft) in drafts.iter().enumerate() {
        let is_first = index == 0;
        let is_last = index + 1 == count;
        let indent = paragraph.props.indent_left_pt
            + if is_first {
                paragraph.props.indent_first_pt
            } else {
                0.0
            };
        let available = if is_first { first } else { rest };
        let start_x = x + indent;
        let mut runs = position_runs(draft, paragraph, start_x, available, is_last);
        let used: f64 = runs.iter().map(|run| run.width).sum();
        let offset = align_offset(paragraph.props.align, used, available);
        for run in &mut runs {
            run.x += offset;
        }

        lines.push(Line {
            page: 0,
            x: start_x,
            y: cursor,
            width: draft.width,
            height: draft.height,
            baseline: cursor + draft.ascent,
            paragraph: paragraph.id.get(),
            runs,
        });
        cursor += draft.height;
    }

    if count > 0 {
        cursor += paragraph.props.space_after_pt;
    }

    (lines, cursor - y)
}

fn align_offset(align: Align, used: f64, available: f64) -> f64 {
    match align {
        Align::Center => (available - used).max(0.0) / 2.0,
        Align::Right => (available - used).max(0.0),
        Align::Justify | Align::Left => 0.0,
    }
}
fn layout_image(
    image: &ImageBlock,
    geometry: &Geometry,
    pages: &mut Vec<PageLayout>,
    cursor: &mut PageCursor,
) {
    let max_width = geometry.content_width.max(24.0);
    let max_height = if geometry.content_bottom.is_finite() {
        (geometry.content_bottom - geometry.content_top).max(24.0)
    } else {
        f64::INFINITY
    };

    let mut scale = (max_width / image.width_pt).min(1.0);
    if max_height.is_finite() {
        scale = scale.min(max_height / image.height_pt);
    }
    let width = (image.width_pt * scale).max(1.0);
    let height = (image.height_pt * scale).max(1.0);

    let gap = 6.0;
    if !cursor.page.lines.is_empty() || !cursor.page.images.is_empty() {
        cursor.y += gap;
    }
    cursor.ensure_room(geometry, height, pages);

    let x = geometry.content_left + (geometry.content_width - width) / 2.0;
    cursor.page.images.push(PlacedImage {
        rect: Rect::new(x, cursor.y, width, height),
        name: image.name.clone(),
        mime: image.mime.clone(),
        data: image.data.clone(),
    });
    cursor.y += height + gap;
}

fn position_runs(
    draft: &DraftLine,
    paragraph: &Paragraph,
    start_x: f64,
    available: f64,
    is_last: bool,
) -> Vec<PlacedRun> {
    let mut runs: Vec<PlacedRun> = Vec::with_capacity(draft.runs.len());

    // Merge adjacent tokens that share formatting so the paint layer emits one
    // text element per visual run.
    for token in &draft.runs {
        if token.text.is_empty() {
            continue;
        }
        if let Some(last) = runs.last_mut() {
            if last.props == token.props {
                last.width += token.width;
                last.text.push_str(&token.text);
                continue;
            }
        }
        runs.push(PlacedRun {
            x: start_x,
            width: token.width,
            text: token.text.clone(),
            props: token.props.clone(),
        });
    }

    let justify = paragraph.props.align == Align::Justify
        && !is_last
        && draft.width > 0.0
        && available > draft.width;

    let mut cursor = start_x;
    if justify && !runs.is_empty() {
        let scale = available / draft.width;
        for run in &mut runs {
            run.x = cursor;
            run.width *= scale;
            cursor += run.width;
        }
    } else {
        for run in &mut runs {
            run.x = cursor;
            cursor += run.width;
        }
    }

    runs
}

fn ch_of(text: &str) -> char {
    text.chars().next().unwrap_or(' ')
}

fn text_style(props: &CharProps) -> TextStyle<'_> {
    TextStyle {
        size_pt: props.size_pt,
        bold: props.bold,
        italic: props.italic,
        family: props.font_family.as_deref(),
        east_asian: props.font_family_east_asian.as_deref(),
    }
}

fn build_tokens(paragraph: &Paragraph, metrics: &dyn Metrics) -> Vec<StyledToken> {
    let mut out = Vec::new();
    for run in &paragraph.runs {
        let style = text_style(&run.props);
        for token in tokenize(&run.text) {
            let (text, space) = match &token {
                Token::Word(word) => (word.clone(), false),
                Token::Char(ch) => (ch.to_string(), false),
                Token::Space => (" ".to_string(), true),
                Token::Tab => ("    ".to_string(), true),
                Token::Break => (String::new(), false),
            };
            let width = match &token {
                Token::Word(_) | Token::Tab => metrics.advance_text(&text, style),
                Token::Char(_) => metrics.advance(ch_of(&text), style),
                Token::Break => 0.0,
                Token::Space => metrics.advance(' ', style),
            };
            out.push(StyledToken {
                kind: token,
                text,
                props: run.props.clone(),
                width,
                space,
            });
        }
    }
    out
}

fn break_paragraph(
    tokens: Vec<StyledToken>,
    first_width: f64,
    rest_width: f64,
    metrics: &dyn Metrics,
) -> Vec<DraftLine> {
    let mut lines: Vec<DraftLine> = Vec::new();
    let mut current = DraftLine::default();
    let mut line_index = 0usize;

    for token in tokens {
        let available = if line_index == 0 {
            first_width
        } else {
            rest_width
        };

        if matches!(token.kind, Token::Break) {
            flush_line(&mut current, &mut lines, metrics);
            line_index += 1;
            continue;
        }

        if token.space && current.is_empty() {
            continue;
        }

        if current.width + token.width > available && !current.is_empty() {
            let first_char = token.text.chars().next();
            let grace = match first_char {
                Some(ch) if is_closing_punct(ch) => token.props.size_pt * 0.9,
                _ => 0.0,
            };
            if current.width + token.width > available + grace {
                flush_line(&mut current, &mut lines, metrics);
                line_index += 1;
            }
        }

        let fits = current.width + token.width <= available;
        if !fits && current.is_empty() && token.width > available {
            // A single token wider than the line: split it by character.
            let style = text_style(&token.props);
            for ch in token.text.chars() {
                let width = metrics.advance(ch, style);
                if current.width + width > available && !current.is_empty() {
                    flush_line(&mut current, &mut lines, metrics);
                    line_index += 1;
                }
                current.height = current.height.max(metrics.line_height(style));
                current.ascent = current.ascent.max(metrics.ascent(style));
                let mut text = String::new();
                text.push(ch);
                current.runs.push(StyledToken {
                    kind: Token::Char(ch),
                    text,
                    props: token.props.clone(),
                    width,
                    space: false,
                });
                current.width += width;
            }
            continue;
        }

        let style = text_style(&token.props);
        current.height = current.height.max(metrics.line_height(style));
        current.ascent = current.ascent.max(metrics.ascent(style));
        current.width += token.width;
        current.runs.push(token);
    }

    if !current.is_empty() || lines.is_empty() {
        flush_line(&mut current, &mut lines, metrics);
    }

    lines
}

fn flush_line(current: &mut DraftLine, lines: &mut Vec<DraftLine>, metrics: &dyn Metrics) {
    trim_trailing_spaces(current);
    if current.height <= 0.0 {
        let style = TextStyle::default();
        current.height = metrics.line_height(style);
        current.ascent = metrics.ascent(style);
    }
    let line = std::mem::take(current);
    lines.push(line);
}

fn trim_trailing_spaces(line: &mut DraftLine) {
    while line.runs.last().is_some_and(|token| token.space) {
        if let Some(token) = line.runs.pop() {
            line.width -= token.width;
        }
    }
}

/// Convenience wrapper using the default synthetic metrics.
pub fn layout_deterministic(document: &Document, options: &LayoutOptions) -> Result<Layout> {
    layout(document, options, &quire_text::DeterministicMetrics)
}

/// Content width in points for a section, exposed for callers and tests.
pub fn content_width(section: &Section) -> Pt {
    Document::content_width_pt(section)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quire_base::{ImageId, TableId};
    use quire_model::{
        AbstractNumbering, FieldKind, HeaderFooter, ImageBlock, NumberFormat, Numbering, NumberingInstance,
        NumberingLevel,
        NumberingRef, PageSetup, Paragraph, Run, Section, Table, TableCell, TableRow,
    };

    #[test]
    fn wraps_wide_characters() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("中".repeat(100)));
        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        assert!(layout.line_count() > 1);
        let line = &layout.pages[0].lines[0];
        let measure = content_width(doc.first_section()).0;
        assert!(line.width <= measure + 0.01);
        assert_eq!(line.runs.len(), 1);
    }

    #[test]
    fn paginates_long_documents() {
        let mut doc = Document::new();
        for _ in 0..120 {
            doc.push_paragraph(Paragraph::plain("A paragraph that occupies a line."));
        }
        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        assert!(layout.page_count() > 1);
    }

    #[test]
    fn empty_paragraph_still_occupies_a_line() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain(""));
        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        assert_eq!(layout.line_count(), 1);
    }

    #[test]
    fn web_mode_is_single_page() {
        let mut doc = Document::new();
        for _ in 0..30 {
            doc.push_paragraph(Paragraph::plain("Line"));
        }
        let layout = layout_deterministic(
            &doc,
            &LayoutOptions {
                view: ViewMode::Web,
                viewport_width_pt: Some(800.0),
            },
        )
        .unwrap();
        assert_eq!(layout.page_count(), 1);
    }

    #[test]
    fn images_are_bounded_by_the_measure() {
        let mut doc = Document::new();
        doc.first_section_mut().push_image(ImageBlock::new(
            ImageId::new(1),
            "wide.png",
            "image/png",
            vec![1, 2, 3],
            1000.0,
            500.0,
        ));
        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        assert_eq!(layout.pages[0].images.len(), 1);
        let image = &layout.pages[0].images[0];
        let measure = content_width(doc.first_section()).0;
        assert!(image.rect.width <= measure + 0.01);
        assert!(image.rect.height > 0.0);
    }

    #[test]
    fn justified_lines_fill_the_measure() {
        let mut doc = Document::new();
        let mut paragraph = Paragraph::plain("word ".repeat(20));
        paragraph.props.align = Align::Justify;
        doc.push_paragraph(paragraph);
        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        if layout.pages[0].lines.len() > 1 {
            let first = &layout.pages[0].lines[0];
            let used: f64 = first.runs.iter().map(|run| run.width).sum();
            assert!(used > 300.0);
        }
    }

    #[test]
    fn lays_out_table_cells() {
        let mut doc = Document::new();
        let mut table = Table::new(TableId::new(1));
        table.grid = vec![100.0, 100.0];

        let mut row = TableRow::default();
        for text in ["left", "right"] {
            let mut cell = TableCell::default();
            cell.blocks.push(Block::Paragraph(Paragraph::plain(text)));
            row.cells.push(cell);
        }
        table.rows.push(row);
        doc.first_section_mut().push_table(table);

        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let page = &layout.pages[0];
        assert_eq!(page.cells.len(), 2);
        assert!(page.cells[0].rect.right() <= page.cells[1].rect.x + 0.01);
        assert!(!page.lines.is_empty());
        for line in &page.lines {
            assert!(line.y >= page.cells[0].rect.y - 0.01);
            assert!(line.y < page.cells[0].rect.bottom());
            assert!(line.baseline >= line.y - 0.01);
            assert!(line.baseline <= line.y + line.height + 0.01);
        }

        let text: String = page
            .lines
            .iter()
            .flat_map(|line| line.runs.iter())
            .map(|run| run.text.as_str())
            .collect();
        assert!(text.contains("left"));
        assert!(text.contains("right"));
    }

    #[test]
    fn table_rows_paginate() {
        let mut doc = Document::new();
        let mut table = Table::new(TableId::new(1));
        table.grid = vec![100.0, 100.0];
        for index in 0..120 {
            let mut row = TableRow::default();
            for column in 0..2 {
                let mut cell = TableCell::default();
                cell.blocks
                    .push(Block::Paragraph(Paragraph::plain(format!("r{index}c{column}"))));
                row.cells.push(cell);
            }
            table.rows.push(row);
        }
        doc.first_section_mut().push_table(table);

        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        assert!(layout.page_count() > 1);
        for page in &layout.pages {
            assert!(page.cells.len() >= 2);
        }
    }

    #[test]
    fn renders_multilevel_list_labels() {
        let mut doc = Document::new();
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
                suffix: quire_model::NumberSuffix::Space,
            },
        );
        abstract_numbering.levels.insert(
            1,
            NumberingLevel {
                level: 1,
                start: 1,
                format: NumberFormat::Decimal,
                text: "%1.%2".to_string(),
                indent_left_pt: 72.0,
                hanging_pt: 18.0,
                suffix: quire_model::NumberSuffix::Space,
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
        *doc.numbering_mut() = numbering;

        for (text, level) in [("Alpha", 0u8), ("Beta", 1), ("Gamma", 0)] {
            let mut paragraph = Paragraph::plain(text);
            paragraph.numbering = Some(NumberingRef {
                num_id: 1,
                level,
            });
            doc.push_paragraph(paragraph);
        }

        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let text: String = layout
            .pages
            .iter()
            .flat_map(|page| page.lines.iter())
            .flat_map(|line| line.runs.iter())
            .map(|run| run.text.as_str())
            .collect();

        assert!(text.contains("1. Alpha") || text.contains("1.  Alpha"));
        assert!(text.contains("1.1 Beta") || text.contains("1.1  Beta"));
        assert!(text.contains("2. Gamma") || text.contains("2.  Gamma"));
    }

    fn page_text(page: &PageLayout) -> String {
        page.lines
            .iter()
            .flat_map(|line| line.runs.iter())
            .map(|run| run.text.as_str())
            .collect()
    }

    #[test]
    fn applies_headers_and_footers() {
        let mut doc = Document::new();
        {
            let section = doc.first_section_mut();
            section.headers.default = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("HEADER"))],
            });
            section.footers.default = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("FOOTER"))],
            });
        }
        doc.push_paragraph(Paragraph::plain("Body"));

        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let page = &layout.pages[0];
        let text = page_text(page);

        assert!(text.contains("HEADER"));
        assert!(text.contains("FOOTER"));

        let header = page
            .lines
            .iter()
            .find(|line| line.runs.iter().any(|run| run.text.contains("HEADER")))
            .unwrap();
        let footer = page
            .lines
            .iter()
            .find(|line| line.runs.iter().any(|run| run.text.contains("FOOTER")))
            .unwrap();
        assert!(header.y < 72.0);
        assert!(footer.y > page.height_pt - 120.0);
    }

    #[test]
    fn paginates_each_section_separately() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Alpha"));
        doc.first_section_mut().headers.default = Some(HeaderFooter {
            blocks: vec![Block::Paragraph(Paragraph::plain("HEAD-A"))],
        });

        let mut second = Section::default();
        second.props.page = PageSetup {
            width_pt: 841.89,
            height_pt: 595.28,
        };
        second.blocks.push(Block::Paragraph(Paragraph::plain("Beta")));
        second.headers.default = Some(HeaderFooter {
            blocks: vec![Block::Paragraph(Paragraph::plain("HEAD-B"))],
        });
        doc.push_section(second);

        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        assert_eq!(layout.pages.len(), 2);

        let first = page_text(&layout.pages[0]);
        assert!(first.contains("HEAD-A"));
        assert!(first.contains("Alpha"));
        assert!(!first.contains("Beta"));

        let second_text = page_text(&layout.pages[1]);
        assert!(second_text.contains("HEAD-B"));
        assert!(second_text.contains("Beta"));
        assert!((layout.pages[1].width_pt - 841.89).abs() < 0.5);
        assert!((layout.pages[0].width_pt - 595.28).abs() < 0.5);
    }

    #[test]
    fn selects_first_and_even_headers() {
        let mut doc = Document::new();
        doc.set_even_and_odd_headers(true);
        {
            let section = doc.first_section_mut();
            section.title_page = true;
            section.headers.default = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("DEFAULT"))],
            });
            section.headers.first = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("FIRST"))],
            });
            section.headers.even = Some(HeaderFooter {
                blocks: vec![Block::Paragraph(Paragraph::plain("EVEN"))],
            });
        }
        for _ in 0..300 {
            doc.push_paragraph(Paragraph::plain("Line of text."));
        }

        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        assert!(layout.page_count() >= 3);

        let first = page_text(&layout.pages[0]);
        let second = page_text(&layout.pages[1]);
        let third = page_text(&layout.pages[2]);
        assert!(first.contains("FIRST"));
        assert!(second.contains("EVEN"));
        assert!(third.contains("DEFAULT"));
    }


    #[test]
    fn resolves_page_fields_in_headers() {
        let mut doc = Document::new();
        let mut header = Paragraph::plain("Page ");
        let mut page_field = Run::plain("1");
        page_field.field = Some(FieldKind::Page);
        header.runs.push(page_field);
        header.push_text(" of ", quire_model::CharProps::default());
        let mut total_field = Run::plain("1");
        total_field.field = Some(FieldKind::NumPages);
        header.runs.push(total_field);

        doc.first_section_mut().headers.default = Some(HeaderFooter {
            blocks: vec![Block::Paragraph(header)],
        });
        for _ in 0..300 {
            doc.push_paragraph(Paragraph::plain("Line of text."));
        }

        let layout = layout_deterministic(&doc, &LayoutOptions::default()).unwrap();
        let total = layout.page_count();
        assert!(total >= 3);

        for (index, page) in layout.pages.iter().enumerate() {
            let text = page_text(page);
            let expected = format!("Page {} of {}", index + 1, total);
            assert!(text.contains(&expected), "page {}: {}", index + 1, text);
        }
    }


    struct TokenMetrics;

    impl Metrics for TokenMetrics {
        fn advance(&self, _ch: char, _style: quire_text::TextStyle<'_>) -> f64 {
            1.0
        }

        fn line_height(&self, _style: quire_text::TextStyle<'_>) -> f64 {
            12.0
        }

        fn ascent(&self, _style: quire_text::TextStyle<'_>) -> f64 {
            10.0
        }

        fn advance_text(&self, text: &str, _style: quire_text::TextStyle<'_>) -> f64 {
            if text == "WIDE" {
                100.0
            } else {
                text.chars().count() as f64
            }
        }
    }

    #[test]
    fn line_breaking_uses_shaped_token_widths() {
        let mut doc = Document::new();
        doc.first_section_mut().props.margins.left_pt = 200.0;
        doc.first_section_mut().props.margins.right_pt = 200.0;
        doc.push_paragraph(Paragraph::plain("WIDE WIDE"));

        let layout = layout(
            &doc,
            &LayoutOptions::default(),
            &TokenMetrics,
        )
        .unwrap();
        assert_eq!(layout.line_count(), 2);
        let first = &layout.pages[0].lines[0];
        assert!((first.width - 100.0).abs() < 1e-9);
    }


    #[test]
    fn layout_cache_reuses_unchanged_paragraphs() {
        let mut doc = Document::new();
        for index in 0..10 {
            doc.push_paragraph(Paragraph::plain(format!("Paragraph {index}")));
        }
        let mut cache = LayoutCache::default();
        layout_with_cache(&doc, &LayoutOptions::default(), &quire_text::DeterministicMetrics, &mut cache)
            .unwrap();
        assert_eq!(cache.misses(), 10);
        assert_eq!(cache.hits(), 0);

        layout_with_cache(&doc, &LayoutOptions::default(), &quire_text::DeterministicMetrics, &mut cache)
            .unwrap();
        assert!(cache.hits() >= 10);
        assert_eq!(cache.misses(), 10);

        if let Some(paragraph) = doc.paragraph_mut(0, 0) {
            paragraph.push_text(" changed", quire_model::CharProps::default());
        }
        layout_with_cache(&doc, &LayoutOptions::default(), &quire_text::DeterministicMetrics, &mut cache)
            .unwrap();
        assert_eq!(cache.misses(), 11);
        assert!(cache.hits() >= 19);
    }

}

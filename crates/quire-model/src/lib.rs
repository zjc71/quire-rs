//! The editable document model.
//!
//! The model is intentionally small and UI-agnostic. It stores *direct*
//! formatting plus an optional style reference. Style resolution happens at
//! the format boundary (see `quire-io`), so layout always sees effective
//! properties and never has to walk a style graph.
//!
//! Everything here is deterministic and `Clone` so the layout engine can work
//! on immutable snapshots.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use quire_base::{DocId, ImageId, ParaId, Pt, Result, Revision, RunId, TableId};

/// Default East Asian body size: 10.5 pt (五号).
pub const DEFAULT_FONT_SIZE_PT: f64 = 10.5;

/// An sRGB colour.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0 };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse a six digit hexadecimal colour such as `1F2937`.
    pub fn from_hex(value: &str) -> Option<Self> {
        let value = value.trim().trim_start_matches('#');
        if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let r = u8::from_str_radix(&value[0..2], 16).ok()?;
        let g = u8::from_str_radix(&value[2..4], 16).ok()?;
        let b = u8::from_str_radix(&value[4..6], 16).ok()?;
        Some(Self { r, g, b })
    }

    pub fn to_hex(self) -> String {
        format!("{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    pub fn to_css(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// Character formatting.
#[derive(Clone, Debug, PartialEq)]
pub struct CharProps {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub size_pt: f64,
    pub color: Color,
    pub font_family: Option<String>,
    pub font_family_east_asian: Option<String>,
    pub size_complex_pt: Option<f64>,
    pub bold_complex: bool,
    pub italic_complex: bool,
    pub language: Option<String>,
    pub language_east_asian: Option<String>,
}

impl Default for CharProps {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            underline: false,
            size_pt: DEFAULT_FONT_SIZE_PT,
            color: Color::BLACK,
            font_family: None,
            font_family_east_asian: None,
            size_complex_pt: None,
            bold_complex: false,
            italic_complex: false,
            language: None,
            language_east_asian: None,
        }
    }
}

impl CharProps {
    pub fn with_size(mut self, size_pt: f64) -> Self {
        self.size_pt = size_pt;
        self
    }
}

/// Paragraph alignment.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// Paragraph formatting.
#[derive(Clone, Debug, PartialEq)]
pub struct ParaProps {
    pub align: Align,
    pub indent_left_pt: f64,
    pub indent_right_pt: f64,
    /// Extra indent applied to the first line only; may be negative.
    pub indent_first_pt: f64,
    pub space_before_pt: f64,
    pub space_after_pt: f64,
    /// Multiplier over the natural line height.
    pub line_spacing: f64,
}

impl Default for ParaProps {
    fn default() -> Self {
        Self {
            align: Align::Left,
            indent_left_pt: 0.0,
            indent_right_pt: 0.0,
            indent_first_pt: 0.0,
            space_before_pt: 0.0,
            space_after_pt: 0.0,
            line_spacing: 1.0,
        }
    }
}

/// A field whose value is produced at layout time.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Page,
    NumPages,
}

impl FieldKind {
    pub fn instruction(self) -> &'static str {
        match self {
            FieldKind::Page => " PAGE ",
            FieldKind::NumPages => " NUMPAGES ",
        }
    }

    pub fn value(self, page_number: u32, total_pages: u32) -> String {
        match self {
            FieldKind::Page => page_number.to_string(),
            FieldKind::NumPages => total_pages.to_string(),
        }
    }
}

/// A maximal span of text with identical character formatting.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub id: RunId,
    pub text: String,
    pub props: CharProps,
    /// Optional named character style.
    pub style: Option<String>,
    /// Optional page field carried by this run.
    pub field: Option<FieldKind>,
}

impl Run {
    pub fn new(id: RunId, text: impl Into<String>, props: CharProps) -> Self {
        Self {
            id,
            text: text.into(),
            props,
            style: None,
            field: None,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(RunId::new(0), text, CharProps::default())
    }

    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }
}

/// A paragraph: formatting plus a sequence of runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Paragraph {
    pub id: ParaId,
    pub props: ParaProps,
    pub runs: Vec<Run>,
    /// Optional named paragraph style.
    pub style: Option<String>,
    /// Optional list membership.
    pub numbering: Option<NumberingRef>,
}

impl Paragraph {
    pub fn new(id: ParaId) -> Self {
        Self {
            id,
            props: ParaProps::default(),
            runs: Vec::new(),
            style: None,
            numbering: None,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        let mut paragraph = Self::new(ParaId::new(0));
        paragraph.push_text(text, CharProps::default());
        paragraph
    }

    pub fn push_text(&mut self, text: impl Into<String>, props: CharProps) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.runs.last_mut() {
            if last.props == props && last.style.is_none() && last.field.is_none() {
                last.text.push_str(&text);
                return;
            }
        }
        let id = RunId::new(self.runs.len() as u32 + 1);
        self.runs.push(Run::new(id, text, props));
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        for run in &self.runs {
            out.push_str(&run.text);
        }
        out
    }

    pub fn char_count(&self) -> usize {
        self.runs.iter().map(Run::char_count).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text.is_empty())
    }

    /// Flatten the paragraph into characters paired with their formatting.
    pub fn char_styles(&self) -> Vec<(char, CharProps)> {
        let mut out = Vec::with_capacity(self.char_count());
        for run in &self.runs {
            for ch in run.text.chars() {
                out.push((ch, run.props.clone()));
            }
        }
        out
    }

    /// Replace the paragraph content with a flattened character stream,
    /// merging adjacent characters that share formatting.
    pub fn set_char_styles(&mut self, chars: &[(char, CharProps)]) {
        self.runs.clear();
        for (ch, props) in chars {
            let mut buf = String::new();
            buf.push(*ch);
            self.push_text(buf, props.clone());
        }
    }

    /// Formatting of the character at `index`, falling back to the nearest run.
    pub fn style_at(&self, index: usize) -> CharProps {
        let mut seen = 0usize;
        for run in &self.runs {
            let count = run.char_count();
            if index < seen + count {
                return run.props.clone();
            }
            seen += count;
        }
        self.runs
            .last()
            .map(|run| run.props.clone())
            .unwrap_or_default()
    }
}

/// An embedded image, already decoded to bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageBlock {
    pub id: ImageId,
    pub name: String,
    pub mime: String,
    pub data: Arc<[u8]>,
    pub width_pt: f64,
    pub height_pt: f64,
}

impl ImageBlock {
    pub fn new(
        id: ImageId,
        name: impl Into<String>,
        mime: impl Into<String>,
        data: impl Into<Arc<[u8]>>,
        width_pt: f64,
        height_pt: f64,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            mime: mime.into(),
            data: data.into(),
            width_pt,
            height_pt,
        }
    }
}

/// A single border line.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Border {
    pub width_pt: f64,
    pub color: Color,
}

impl Border {
    pub const fn new(width_pt: f64, color: Color) -> Self {
        Self { width_pt, color }
    }
}

/// Borders around one table cell.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct CellBorders {
    pub top: Option<Border>,
    pub right: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
}

/// Borders around a table, including inside lines.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct TableBorders {
    pub top: Option<Border>,
    pub right: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
    pub inside_horizontal: Option<Border>,
    pub inside_vertical: Option<Border>,
}

/// Vertical merge state of a cell.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum VMerge {
    #[default]
    None,
    Restart,
    Continue,
}

/// One table cell.
#[derive(Clone, Debug, PartialEq)]
pub struct TableCell {
    pub blocks: Vec<Block>,
    /// Number of grid columns this cell spans.
    pub span: u16,
    pub vmerge: VMerge,
    pub borders: CellBorders,
    pub shading: Option<Color>,
    pub padding_pt: f64,
}

impl Default for TableCell {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            span: 1,
            vmerge: VMerge::None,
            borders: CellBorders::default(),
            shading: None,
            padding_pt: 4.0,
        }
    }
}

impl TableCell {
    pub fn plain_text(&self) -> String {
        blocks_text(&self.blocks).trim_end_matches('\n').to_string()
    }
}

/// One table row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableRow {
    pub height_pt: Option<f64>,
    pub header: bool,
    pub cells: Vec<TableCell>,
}

/// A table block.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub id: TableId,
    pub style: Option<String>,
    /// Column widths in points, in grid order.
    pub grid: Vec<f64>,
    pub borders: TableBorders,
    pub rows: Vec<TableRow>,
}

impl Table {
    pub fn new(id: TableId) -> Self {
        Self {
            id,
            style: None,
            grid: Vec::new(),
            borders: TableBorders::default(),
            rows: Vec::new(),
        }
    }

    pub fn column_count(&self) -> usize {
        self.rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.span.max(1) as usize)
                    .sum()
            })
            .max()
            .unwrap_or(0)
            .max(self.grid.len())
    }

    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for row in &self.rows {
            let cells: Vec<String> = row.cells.iter().map(TableCell::plain_text).collect();
            out.push_str(&cells.join("\t"));
            out.push('\n');
        }
        out
    }
}

/// A block-level document element.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(Paragraph),
    Image(ImageBlock),
    Table(Table),
}

impl Block {
    pub fn as_paragraph(&self) -> Option<&Paragraph> {
        match self {
            Block::Paragraph(paragraph) => Some(paragraph),
            Block::Image(_) | Block::Table(_) => None,
        }
    }

    pub fn as_paragraph_mut(&mut self) -> Option<&mut Paragraph> {
        match self {
            Block::Paragraph(paragraph) => Some(paragraph),
            Block::Image(_) | Block::Table(_) => None,
        }
    }
}

fn blocks_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => out.push_str(&paragraph.text()),
            Block::Image(_) => out.push_str("[image]"),
            Block::Table(table) => {
                out.push_str(table.plain_text().trim_end_matches('\n'));
            }
        }
        out.push('\n');
    }
    out
}

/// Named style kind.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StyleKind {
    Paragraph,
    Character,
    Other,
}

/// A named style with already resolved (effective) properties.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub id: String,
    pub name: String,
    pub kind: StyleKind,
    pub para: Option<ParaProps>,
    pub char: Option<CharProps>,
}

impl Style {
    pub fn new(id: impl Into<String>, name: impl Into<String>, kind: StyleKind) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            para: None,
            char: None,
        }
    }
}

/// Styles available to a document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleSheet {
    styles: BTreeMap<String, Style>,
}

impl StyleSheet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, style: Style) {
        self.styles.insert(style.id.clone(), style);
    }

    pub fn get(&self, id: &str) -> Option<&Style> {
        self.styles.get(id)
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Style> {
        self.styles.values()
    }

    pub fn paragraph_props(&self, id: &str) -> Option<&ParaProps> {
        self.styles
            .get(id)
            .filter(|style| style.kind == StyleKind::Paragraph)
            .and_then(|style| style.para.as_ref())
    }

    pub fn char_props(&self, id: &str) -> Option<&CharProps> {
        self.styles
            .get(id)
            .filter(|style| matches!(style.kind, StyleKind::Paragraph | StyleKind::Character))
            .and_then(|style| style.char.as_ref())
    }
}


/// Number format for a list level.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum NumberFormat {
    #[default]
    Decimal,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    Bullet,
    None,
}

impl NumberFormat {
    pub fn label(self, value: u32) -> String {
        match self {
            NumberFormat::Decimal => value.to_string(),
            NumberFormat::LowerLetter => letters(value, false),
            NumberFormat::UpperLetter => letters(value, true),
            NumberFormat::LowerRoman => roman(value).to_ascii_lowercase(),
            NumberFormat::UpperRoman => roman(value),
            NumberFormat::Bullet => "•".to_string(),
            NumberFormat::None => String::new(),
        }
    }
}

/// What follows a list label.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum NumberSuffix {
    #[default]
    Tab,
    Space,
    Nothing,
}

impl NumberSuffix {
    pub fn text(self) -> &'static str {
        match self {
            NumberSuffix::Tab => "\t",
            NumberSuffix::Space => " ",
            NumberSuffix::Nothing => "",
        }
    }
}

/// One level of a multilevel list.
#[derive(Clone, Debug, PartialEq)]
pub struct NumberingLevel {
    pub level: u8,
    pub start: u32,
    pub format: NumberFormat,
    /// Label template such as %1. or %1.%2.
    pub text: String,
    pub indent_left_pt: f64,
    pub hanging_pt: f64,
    pub suffix: NumberSuffix,
}

impl Default for NumberingLevel {
    fn default() -> Self {
        Self {
            level: 0,
            start: 1,
            format: NumberFormat::Decimal,
            text: "%1.".to_string(),
            indent_left_pt: 36.0,
            hanging_pt: 18.0,
            suffix: NumberSuffix::Tab,
        }
    }
}

/// An abstract numbering definition.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AbstractNumbering {
    pub id: u32,
    pub levels: BTreeMap<u8, NumberingLevel>,
}

/// A concrete numbering instance that points at an abstract definition.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NumberingInstance {
    pub id: u32,
    pub abstract_id: u32,
    pub start_overrides: BTreeMap<u8, u32>,
}

/// All numbering definitions in a document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Numbering {
    pub abstracts: BTreeMap<u32, AbstractNumbering>,
    pub instances: BTreeMap<u32, NumberingInstance>,
}

impl Numbering {
    pub fn is_empty(&self) -> bool {
        self.abstracts.is_empty() && self.instances.is_empty()
    }

    pub fn level(&self, num_id: u32, level: u8) -> Option<&NumberingLevel> {
        let instance = self.instances.get(&num_id)?;
        let abstract_numbering = self.abstracts.get(&instance.abstract_id)?;
        abstract_numbering.levels.get(&level)
    }

    pub fn start(&self, num_id: u32, level: u8) -> Option<u32> {
        let instance = self.instances.get(&num_id)?;
        if let Some(value) = instance.start_overrides.get(&level) {
            return Some(*value);
        }
        self.level(num_id, level).map(|level| level.start)
    }
}

/// A paragraph's list membership.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NumberingRef {
    pub num_id: u32,
    pub level: u8,
}

fn letters(mut value: u32, upper: bool) -> String {
    if value == 0 {
        return String::new();
    }
    let mut out = Vec::new();
    while value > 0 {
        let digit = ((value - 1) % 26) as u8;
        out.push(if upper {
            b'A' + digit
        } else {
            b'a' + digit
        });
        value = (value - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn roman(mut value: u32) -> String {
    const TABLE: &[(u32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value_of, symbol) in TABLE {
        while value >= *value_of {
            out.push_str(symbol);
            value -= value_of;
        }
    }
    out
}

/// Content type declarations from `[Content_Types].xml`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContentTypes {
    pub defaults: BTreeMap<String, String>,
    pub overrides: BTreeMap<String, String>,
}

/// A relationship from a package or part relationship file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageRelationship {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub external: bool,
}

/// Parts and relationships preserved verbatim.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Package {
    pub parts: BTreeMap<String, Vec<u8>>,
    pub content_types: ContentTypes,
    pub document_relationships: Vec<PackageRelationship>,
    pub package_relationships: Vec<PackageRelationship>,
}

/// Content the engine can read but not write back yet.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FidelityFeature {
    Fields,
    Footnotes,
    Endnotes,
    Comments,
    Revisions,
    FloatingShapes,
    LegacyDrawings,
    TextBoxes,
    ContentControls,
    Hyperlinks,
    Symbols,
    Ruby,
    Bookmarks,
    AlternateContent,
    NestedTables,
    UnknownSectionProperties,
    UnknownParagraphProperties,
    UnknownRunProperties,
    UnknownTableProperties,
    MultipleSections,
}

impl FidelityFeature {
    pub fn label(self) -> &'static str {
        match self {
            FidelityFeature::Fields => "fields",
            FidelityFeature::Footnotes => "footnotes",
            FidelityFeature::Endnotes => "endnotes",
            FidelityFeature::Comments => "comments",
            FidelityFeature::Revisions => "tracked revisions",
            FidelityFeature::FloatingShapes => "floating shapes",
            FidelityFeature::LegacyDrawings => "legacy drawings",
            FidelityFeature::TextBoxes => "text boxes",
            FidelityFeature::ContentControls => "content controls",
            FidelityFeature::Hyperlinks => "hyperlinks",
            FidelityFeature::Symbols => "symbol characters",
            FidelityFeature::Ruby => "ruby text",
            FidelityFeature::Bookmarks => "bookmarks",
            FidelityFeature::AlternateContent => "alternate content",
            FidelityFeature::NestedTables => "nested tables",
            FidelityFeature::UnknownSectionProperties => "unsupported section properties",
            FidelityFeature::UnknownParagraphProperties => "unsupported paragraph properties",
            FidelityFeature::UnknownRunProperties => "unsupported run properties",
            FidelityFeature::UnknownTableProperties => "unsupported table properties",
            FidelityFeature::MultipleSections => "multiple sections",
        }
    }
}

/// Which unsupported features a parsed document contains.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FidelityReport {
    features: BTreeSet<FidelityFeature>,
}

impl FidelityReport {
    pub fn note(&mut self, feature: FidelityFeature) {
        self.features.insert(feature);
    }

    pub fn extend(&mut self, other: &Self) {
        self.features.extend(other.features.iter().copied());
    }

    pub fn is_lossless(&self) -> bool {
        self.features.is_empty()
    }

    pub fn features(&self) -> impl Iterator<Item = FidelityFeature> + '_ {
        self.features.iter().copied()
    }

    pub fn summary(&self) -> String {
        self.features
            .iter()
            .map(|feature| feature.label())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Paper size in points.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PageSetup {
    pub width_pt: f64,
    pub height_pt: f64,
}

impl PageSetup {
    /// ISO A4.
    pub const fn a4() -> Self {
        Self {
            width_pt: 595.28,
            height_pt: 841.89,
        }
    }

    /// US Letter.
    pub const fn letter() -> Self {
        Self {
            width_pt: 612.0,
            height_pt: 792.0,
        }
    }
}

impl Default for PageSetup {
    fn default() -> Self {
        Self::a4()
    }
}

/// Page margins in points.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Margins {
    pub top_pt: f64,
    pub bottom_pt: f64,
    pub left_pt: f64,
    pub right_pt: f64,
}

impl Margins {
    pub const fn none() -> Self {
        Self {
            top_pt: 0.0,
            bottom_pt: 0.0,
            left_pt: 0.0,
            right_pt: 0.0,
        }
    }

    /// Chinese default: 2.54 cm top/bottom, 3.18 cm left/right.
    pub const fn chinese_default() -> Self {
        Self {
            top_pt: 72.0,
            bottom_pt: 72.0,
            left_pt: 90.0,
            right_pt: 90.0,
        }
    }
}

impl Default for Margins {
    fn default() -> Self {
        Self::chinese_default()
    }
}

/// Section properties.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub struct SectionProps {
    pub page: PageSetup,
    pub margins: Margins,
}


/// Header or footer content for one page variant.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderFooter {
    pub blocks: Vec<Block>,
}

/// Default, first-page and even-page header or footer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderFooterSet {
    pub default: Option<HeaderFooter>,
    pub first: Option<HeaderFooter>,
    pub even: Option<HeaderFooter>,
}

impl HeaderFooterSet {
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.first.is_none() && self.even.is_none()
    }

    pub fn for_page(
        &self,
        page_number: u32,
        title_page: bool,
        even_and_odd: bool,
    ) -> Option<&HeaderFooter> {
        if page_number == 1 && title_page {
            return self.first.as_ref().or(self.default.as_ref());
        }
        if even_and_odd && page_number % 2 == 0 {
            return self.even.as_ref().or(self.default.as_ref());
        }
        self.default.as_ref()
    }
}
/// A section is the unit that owns page geometry and a block sequence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Section {
    pub props: SectionProps,
    pub blocks: Vec<Block>,
    pub headers: HeaderFooterSet,
    pub footers: HeaderFooterSet,
    pub title_page: bool,
}

impl Section {
    pub fn paragraph_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| matches!(block, Block::Paragraph(_)))
            .count()
    }

    pub fn table_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| matches!(block, Block::Table(_)))
            .count()
    }

    pub fn push_table(&mut self, table: Table) {
        self.blocks.push(Block::Table(table));
    }

    pub fn paragraphs(&self) -> impl Iterator<Item = &Paragraph> {
        self.blocks.iter().filter_map(Block::as_paragraph)
    }

    pub fn paragraphs_mut(&mut self) -> impl Iterator<Item = &mut Paragraph> {
        self.blocks.iter_mut().filter_map(Block::as_paragraph_mut)
    }

    pub fn paragraph(&self, index: usize) -> Option<&Paragraph> {
        self.paragraphs().nth(index)
    }

    pub fn paragraph_mut(&mut self, index: usize) -> Option<&mut Paragraph> {
        self.paragraphs_mut().nth(index)
    }

    pub fn push_paragraph_block(&mut self, paragraph: Paragraph) {
        self.blocks.push(Block::Paragraph(paragraph));
    }

    pub fn push_image(&mut self, image: ImageBlock) {
        self.blocks.push(Block::Image(image));
    }
}

/// Location of a container that owns a paragraph sequence.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ContainerId {
    Body { section: usize },
    TableCell { table: TableId, row: usize, cell: usize },
}

/// The document root.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    doc_id: DocId,
    revision: Revision,
    sections: Vec<Section>,
    styles: StyleSheet,
    numbering: Numbering,
    even_and_odd_headers: bool,
    package: Package,
    fidelity: FidelityReport,
    next_para_id: u32,
    next_image_id: u32,
    next_table_id: u32,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        Self {
            doc_id: DocId::new(1),
            revision: Revision::ZERO,
            sections: vec![Section::default()],
            styles: StyleSheet::new(),
            numbering: Numbering::default(),
            even_and_odd_headers: false,
            package: Package::default(),
            fidelity: FidelityReport::default(),
            next_para_id: 1,
            next_image_id: 1,
            next_table_id: 1,
        }
    }

    pub fn from_text(text: &str) -> Self {
        let mut doc = Self::new();
        for line in text.split('\n') {
            doc.push_paragraph(Paragraph::plain(line));
        }
        doc
    }

    pub fn doc_id(&self) -> DocId {
        self.doc_id
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    pub fn sections_mut(&mut self) -> &mut [Section] {
        &mut self.sections
    }

    pub fn first_section(&self) -> &Section {
        &self.sections[0]
    }

    pub fn first_section_mut(&mut self) -> &mut Section {
        &mut self.sections[0]
    }

    pub fn section_mut(&mut self, index: usize) -> Option<&mut Section> {
        self.sections.get_mut(index)
    }

    pub fn clear_sections(&mut self) {
        self.sections.clear();
    }

    pub fn push_section(&mut self, section: Section) {
        self.sections.push(section);
    }

    pub fn styles(&self) -> &StyleSheet {
        &self.styles
    }

    pub fn styles_mut(&mut self) -> &mut StyleSheet {
        &mut self.styles
    }

    pub fn numbering(&self) -> &Numbering {
        &self.numbering
    }

    pub fn numbering_mut(&mut self) -> &mut Numbering {
        &mut self.numbering
    }

    pub fn even_and_odd_headers(&self) -> bool {
        self.even_and_odd_headers
    }

    pub fn set_even_and_odd_headers(&mut self, value: bool) {
        self.even_and_odd_headers = value;
    }

    pub fn package(&self) -> &Package {
        &self.package
    }

    pub fn package_mut(&mut self) -> &mut Package {
        &mut self.package
    }

    pub fn fidelity(&self) -> &FidelityReport {
        &self.fidelity
    }

    pub fn set_fidelity(&mut self, report: FidelityReport) {
        self.fidelity = report;
    }

    pub fn note_fidelity(&mut self, feature: FidelityFeature) {
        self.fidelity.note(feature);
    }

    pub fn next_para_id(&mut self) -> ParaId {
        let id = ParaId::new(self.next_para_id);
        self.next_para_id = self.next_para_id.saturating_add(1);
        id
    }

    pub fn next_image_id(&mut self) -> ImageId {
        let id = ImageId::new(self.next_image_id);
        self.next_image_id = self.next_image_id.saturating_add(1);
        id
    }

    pub fn next_table_id(&mut self) -> TableId {
        let id = TableId::new(self.next_table_id);
        self.next_table_id = self.next_table_id.saturating_add(1);
        id
    }

    pub fn push_paragraph(&mut self, mut paragraph: Paragraph) {
        if paragraph.id.get() == 0 {
            paragraph.id = self.next_para_id();
        }
        if self.sections.is_empty() {
            self.sections.push(Section::default());
        }
        self.sections[0].push_paragraph_block(paragraph);
        self.bump_revision();
    }

    pub fn paragraph_count(&self) -> usize {
        self.sections.iter().map(Section::paragraph_count).sum()
    }

    pub fn block_count(&self) -> usize {
        self.sections.iter().map(|section| section.blocks.len()).sum()
    }

    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.sections.iter().flat_map(|section| section.blocks.iter())
    }

    pub fn paragraphs(&self) -> impl Iterator<Item = &Paragraph> {
        self.blocks().filter_map(Block::as_paragraph)
    }

    pub fn paragraphs_mut(&mut self) -> impl Iterator<Item = &mut Paragraph> {
        self.sections
            .iter_mut()
            .flat_map(|section| section.blocks.iter_mut())
            .filter_map(Block::as_paragraph_mut)
    }

    pub fn paragraph(&self, section: usize, index: usize) -> Option<&Paragraph> {
        self.sections.get(section).and_then(|s| s.paragraph(index))
    }

    /// Block index of the paragraph with the given ordinal inside a section.
    pub fn paragraph_block_index(&self, section: usize, index: usize) -> Option<usize> {
        let section = self.sections.get(section)?;
        let mut ordinal = 0;
        for (block_index, block) in section.blocks.iter().enumerate() {
            if matches!(block, Block::Paragraph(_)) {
                if ordinal == index {
                    return Some(block_index);
                }
                ordinal += 1;
            }
        }
        None
    }

    /// Insert a block immediately after a top-level paragraph.
    pub fn insert_block_after_paragraph(
        &mut self,
        section: usize,
        paragraph_index: usize,
        block: Block,
    ) -> Result<()> {
        let block_index = self
            .paragraph_block_index(section, paragraph_index)
            .ok_or_else(|| quire_base::QuireError::edit("paragraph not found"))?;
        self.sections[section].blocks.insert(block_index + 1, block);
        Ok(())
    }

    /// Insert a paragraph before the given paragraph ordinal, or at the end.
    pub fn insert_paragraph_at(
        &mut self,
        section: usize,
        index: usize,
        paragraph: Paragraph,
    ) -> Result<()> {
        let count = self
            .sections
            .get(section)
            .map(Section::paragraph_count)
            .ok_or_else(|| quire_base::QuireError::edit("no such section"))?;
        if index > count {
            return Err(quire_base::QuireError::edit(
                "paragraph index is out of range",
            ));
        }
        let block_index = if index == count {
            self.sections[section].blocks.len()
        } else {
            self.paragraph_block_index(section, index)
                .ok_or_else(|| quire_base::QuireError::edit("paragraph not found"))?
        };
        self.sections[section]
            .blocks
            .insert(block_index, Block::Paragraph(paragraph));
        Ok(())
    }

    /// Remove the half-open paragraph ordinal range `[start, end)`.
    pub fn remove_paragraph_range(
        &mut self,
        section: usize,
        start: usize,
        end: usize,
    ) -> Result<()> {
        let count = self
            .sections
            .get(section)
            .map(Section::paragraph_count)
            .ok_or_else(|| quire_base::QuireError::edit("no such section"))?;
        if start > end || end > count {
            return Err(quire_base::QuireError::edit(
                "paragraph range is out of bounds",
            ));
        }
        if start == end {
            return Ok(());
        }
        let blocks = &mut self.sections[section].blocks;
        let mut ordinal = 0usize;
        let mut remove = Vec::new();
        for (block_index, block) in blocks.iter().enumerate() {
            if matches!(block, Block::Paragraph(_)) {
                if ordinal >= start && ordinal < end {
                    remove.push(block_index);
                }
                ordinal += 1;
            }
        }
        for block_index in remove.into_iter().rev() {
            blocks.remove(block_index);
        }
        Ok(())
    }

    pub fn paragraph_mut(&mut self, section: usize, index: usize) -> Option<&mut Paragraph> {
        self.sections
            .get_mut(section)
            .and_then(|s| s.paragraph_mut(index))
    }

    pub fn find_paragraph_mut(&mut self, id: ParaId) -> Option<&mut Paragraph> {
        self.paragraphs_mut().find(|p| p.id == id)
    }

    pub fn plain_text(&self) -> String {
        self.sections
            .iter()
            .map(|section| blocks_text(&section.blocks))
            .collect()
    }

    pub fn char_count(&self) -> usize {
        self.sections
            .iter()
            .map(|section| blocks_char_count(&section.blocks))
            .sum()
    }

    pub fn total_paragraph_count(&self) -> usize {
        self.sections
            .iter()
            .map(|section| blocks_paragraph_count(&section.blocks))
            .sum()
    }

    pub fn bump_revision(&mut self) {
        self.revision = self.revision.next();
    }

    /// Reassign paragraph, image and table identifiers in document order.
    /// Format readers call this after parsing so identifiers are dense and
    /// stable.
    pub fn renumber(&mut self) {
        let mut para = 1;
        let mut image = 1;
        let mut table = 1;
        for section in &mut self.sections {
            renumber_blocks(&mut section.blocks, &mut para, &mut image, &mut table);
            renumber_set(&mut section.headers, &mut para, &mut image, &mut table);
            renumber_set(&mut section.footers, &mut para, &mut image, &mut table);
        }
        self.next_para_id = para;
        self.next_image_id = image;
        self.next_table_id = table;
    }

    /// Validate structural invariants used by later layers.
    pub fn validate(&self) -> Result<()> {
        if self.sections.is_empty() {
            return Err(quire_base::QuireError::format(
                "document must contain at least one section",
            ));
        }
        for section in &self.sections {
            if section.props.page.width_pt <= 0.0 || section.props.page.height_pt <= 0.0 {
                return Err(quire_base::QuireError::format(
                    "page size must be positive",
                ));
            }
            validate_blocks(&section.blocks)?;
        }
        Ok(())
    }

    /// Number of editable paragraphs, including table cells, in document order.
    pub fn editable_paragraph_count(&self) -> usize {
        let mut counter = 0;
        for (section_index, section) in self.sections.iter().enumerate() {
            read_blocks(
                &section.blocks,
                ContainerId::Body {
                    section: section_index,
                },
                &mut counter,
                usize::MAX,
            );
        }
        counter
    }

    /// Location of an editable paragraph by global index.
    pub fn paragraph_location(&self, index: usize) -> Option<(ContainerId, usize)> {
        let mut counter = 0;
        for (section_index, section) in self.sections.iter().enumerate() {
            if let Some((_, container, local)) = read_blocks(
                &section.blocks,
                ContainerId::Body {
                    section: section_index,
                },
                &mut counter,
                index,
            ) {
                return Some((container, local));
            }
        }
        None
    }

    pub fn paragraph_at_global(&self, index: usize) -> Option<&Paragraph> {
        let mut counter = 0;
        for (section_index, section) in self.sections.iter().enumerate() {
            if let Some((paragraph, _, _)) = read_blocks(
                &section.blocks,
                ContainerId::Body {
                    section: section_index,
                },
                &mut counter,
                index,
            ) {
                return Some(paragraph);
            }
        }
        None
    }

    pub fn paragraph_at_global_mut(&mut self, index: usize) -> Option<&mut Paragraph> {
        let mut counter = 0;
        for section in &mut self.sections {
            if let Some(paragraph) = write_blocks(&mut section.blocks, &mut counter, index, true) {
                return Some(paragraph);
            }
        }
        None
    }

    pub fn global_index_of_paragraph(&self, id: ParaId) -> Option<usize> {
        let mut counter = 0;
        for section in &self.sections {
            if find_paragraph_index(&section.blocks, &mut counter, id, true) {
                return Some(counter);
            }
        }
        None
    }

    pub fn table_by_id(&self, id: TableId) -> Option<&Table> {
        self.sections
            .iter()
            .flat_map(|section| section.blocks.iter())
            .find_map(|block| match block {
                Block::Table(table) if table.id == id => Some(table),
                _ => None,
            })
    }

    pub fn remove_table(&mut self, id: TableId) -> bool {
        for section in &mut self.sections {
            if let Some(index) = section.blocks.iter().position(|block| {
                matches!(block, Block::Table(table) if table.id == id)
            }) {
                section.blocks.remove(index);
                return true;
            }
        }
        false
    }

    pub fn table_by_id_mut(&mut self, id: TableId) -> Option<&mut Table> {
        self.sections
            .iter_mut()
            .flat_map(|section| section.blocks.iter_mut())
            .find_map(|block| match block {
                Block::Table(table) if table.id == id => Some(table),
                _ => None,
            })
    }

    pub fn container_paragraph_count(&self, container: ContainerId) -> Option<usize> {
        match container {
            ContainerId::Body { section } => {
                self.sections.get(section).map(Section::paragraph_count)
            }
            ContainerId::TableCell { table, row, cell } => self
                .table_by_id(table)
                .and_then(|table| table.rows.get(row))
                .and_then(|row| row.cells.get(cell))
                .map(|cell| {
                    cell.blocks
                        .iter()
                        .filter(|block| matches!(block, Block::Paragraph(_)))
                        .count()
                }),
        }
    }

    pub fn paragraph_id_at_location(
        &self,
        container: ContainerId,
        local: usize,
    ) -> Option<ParaId> {
        match container {
            ContainerId::Body { section } => self
                .sections
                .get(section)
                .and_then(|section| section.paragraph(local))
                .map(|paragraph| paragraph.id),
            ContainerId::TableCell { table, row, cell } => self
                .table_by_id(table)
                .and_then(|table| table.rows.get(row))
                .and_then(|row| row.cells.get(cell))
                .and_then(|cell| {
                    cell.blocks
                        .iter()
                        .filter_map(Block::as_paragraph)
                        .nth(local)
                        .map(|paragraph| paragraph.id)
                }),
        }
    }

    pub fn insert_paragraph_after_location(
        &mut self,
        container: ContainerId,
        local: usize,
        paragraph: Paragraph,
    ) -> Result<()> {
        match container {
            ContainerId::Body { section } => {
                let count = self
                    .sections
                    .get(section)
                    .map(Section::paragraph_count)
                    .ok_or_else(|| quire_base::QuireError::edit("no such section"))?;
                if local >= count {
                    return Err(quire_base::QuireError::edit(
                        "paragraph location is out of range",
                    ));
                }
                self.insert_paragraph_at(section, local + 1, paragraph)
            }
            ContainerId::TableCell { table, row, cell } => {
                let target = self
                    .table_by_id_mut(table)
                    .and_then(|table| table.rows.get_mut(row))
                    .and_then(|row| row.cells.get_mut(cell))
                    .ok_or_else(|| quire_base::QuireError::edit("no such table cell"))?;
                let mut ordinal = 0usize;
                let mut block_index = None;
                for (index, block) in target.blocks.iter().enumerate() {
                    if matches!(block, Block::Paragraph(_)) {
                        if ordinal == local {
                            block_index = Some(index);
                            break;
                        }
                        ordinal += 1;
                    }
                }
                let index = block_index.ok_or_else(|| {
                    quire_base::QuireError::edit("paragraph location is out of range")
                })?;
                target
                    .blocks
                    .insert(index + 1, Block::Paragraph(paragraph));
                Ok(())
            }
        }
    }

    pub fn remove_paragraph_at_location(
        &mut self,
        container: ContainerId,
        local: usize,
    ) -> Result<()> {
        match container {
            ContainerId::Body { section } => {
                self.remove_paragraph_range(section, local, local + 1)
            }
            ContainerId::TableCell { table, row, cell } => {
                let target = self
                    .table_by_id_mut(table)
                    .and_then(|table| table.rows.get_mut(row))
                    .and_then(|row| row.cells.get_mut(cell))
                    .ok_or_else(|| quire_base::QuireError::edit("no such table cell"))?;
                let mut ordinal = 0usize;
                let mut block_index = None;
                for (index, block) in target.blocks.iter().enumerate() {
                    if matches!(block, Block::Paragraph(_)) {
                        if ordinal == local {
                            block_index = Some(index);
                            break;
                        }
                        ordinal += 1;
                    }
                }
                let index = block_index.ok_or_else(|| {
                    quire_base::QuireError::edit("paragraph location is out of range")
                })?;
                target.blocks.remove(index);
                Ok(())
            }
        }
    }

    /// Content width of a section in points.
    pub fn content_width_pt(section: &Section) -> Pt {
        Pt(section.props.page.width_pt
            - section.props.margins.left_pt
            - section.props.margins.right_pt)
    }
}

fn renumber_blocks(blocks: &mut [Block], para: &mut u32, image: &mut u32, table: &mut u32) {
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => {
                paragraph.id = ParaId::new(*para);
                *para += 1;
                for (index, run) in paragraph.runs.iter_mut().enumerate() {
                    run.id = RunId::new(index as u32 + 1);
                }
            }
            Block::Image(item) => {
                item.id = ImageId::new(*image);
                *image += 1;
            }
            Block::Table(item) => {
                item.id = TableId::new(*table);
                *table += 1;
                for row in &mut item.rows {
                    for cell in &mut row.cells {
                        renumber_blocks(&mut cell.blocks, para, image, table);
                    }
                }
            }
        }
    }
}

fn renumber_set(
    set: &mut HeaderFooterSet,
    para: &mut u32,
    image: &mut u32,
    table: &mut u32,
) {
    for item in [&mut set.default, &mut set.first, &mut set.even]
        .into_iter()
        .flatten()
    {
        renumber_blocks(&mut item.blocks, para, image, table);
    }
}

fn read_blocks<'a>(
    blocks: &'a [Block],
    container: ContainerId,
    counter: &mut usize,
    target: usize,
) -> Option<(&'a Paragraph, ContainerId, usize)> {
    let mut local = 0usize;
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => {
                if *counter == target {
                    return Some((paragraph, container, local));
                }
                *counter += 1;
                local += 1;
            }
            Block::Table(table) => {
                if matches!(container, ContainerId::TableCell { .. }) {
                    continue;
                }
                for (row_index, row) in table.rows.iter().enumerate() {
                    for (cell_index, cell) in row.cells.iter().enumerate() {
                        let cell_container = ContainerId::TableCell {
                            table: table.id,
                            row: row_index,
                            cell: cell_index,
                        };
                        if let Some(found) =
                            read_blocks(&cell.blocks, cell_container, counter, target)
                        {
                            return Some(found);
                        }
                    }
                }
            }
            Block::Image(_) => {}
        }
    }
    None
}

fn write_blocks<'a>(
    blocks: &'a mut [Block],
    counter: &mut usize,
    target: usize,
    allow_tables: bool,
) -> Option<&'a mut Paragraph> {
    for block in blocks.iter_mut() {
        match block {
            Block::Paragraph(paragraph) => {
                if *counter == target {
                    return Some(paragraph);
                }
                *counter += 1;
            }
            Block::Table(table) => {
                if !allow_tables {
                    continue;
                }
                for row in table.rows.iter_mut() {
                    for cell in row.cells.iter_mut() {
                        if let Some(found) =
                            write_blocks(&mut cell.blocks, counter, target, false)
                        {
                            return Some(found);
                        }
                    }
                }
            }
            Block::Image(_) => {}
        }
    }
    None
}

fn find_paragraph_index(
    blocks: &[Block],
    counter: &mut usize,
    id: ParaId,
    allow_tables: bool,
) -> bool {
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => {
                if paragraph.id == id {
                    return true;
                }
                *counter += 1;
            }
            Block::Table(table) => {
                if !allow_tables {
                    continue;
                }
                for row in &table.rows {
                    for cell in &row.cells {
                        if find_paragraph_index(&cell.blocks, counter, id, false) {
                            return true;
                        }
                    }
                }
            }
            Block::Image(_) => {}
        }
    }
    false
}

fn blocks_char_count(blocks: &[Block]) -> usize {
    let mut total = 0;
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => total += paragraph.char_count(),
            Block::Image(_) => {}
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        total += blocks_char_count(&cell.blocks);
                    }
                }
            }
        }
    }
    total
}

fn blocks_paragraph_count(blocks: &[Block]) -> usize {
    let mut total = 0;
    for block in blocks {
        match block {
            Block::Paragraph(_) => total += 1,
            Block::Image(_) => {}
            Block::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        total += blocks_paragraph_count(&cell.blocks);
                    }
                }
            }
        }
    }
    total
}

fn validate_blocks(blocks: &[Block]) -> Result<()> {
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => {
                for run in &paragraph.runs {
                    if !run.props.size_pt.is_finite() || run.props.size_pt <= 0.0 {
                        return Err(quire_base::QuireError::format(
                            "font size must be finite and positive",
                        ));
                    }
                }
            }
            Block::Image(image) => {
                if image.data.is_empty() {
                    return Err(quire_base::QuireError::format(
                        "image data must not be empty",
                    ));
                }
                if image.width_pt <= 0.0 || image.height_pt <= 0.0 {
                    return Err(quire_base::QuireError::format(
                        "image size must be positive",
                    ));
                }
                if image.mime.is_empty() {
                    return Err(quire_base::QuireError::format(
                        "image mime type must not be empty",
                    ));
                }
            }
            Block::Table(table) => {
                if table.rows.is_empty() {
                    return Err(quire_base::QuireError::format("table must have rows"));
                }
                for width in &table.grid {
                    if !width.is_finite() || *width < 0.0 {
                        return Err(quire_base::QuireError::format(
                            "table grid width must be finite and non-negative",
                        ));
                    }
                }
                for row in &table.rows {
                    if let Some(height) = row.height_pt {
                        if !height.is_finite() || height < 0.0 {
                            return Err(quire_base::QuireError::format(
                                "table row height must be finite and non-negative",
                            ));
                        }
                    }
                    for cell in &row.cells {
                        if cell.span == 0 {
                            return Err(quire_base::QuireError::format(
                                "table cell span must be at least one",
                            ));
                        }
                        if !cell.padding_pt.is_finite() || cell.padding_pt < 0.0 {
                            return Err(quire_base::QuireError::format(
                                "table cell padding must be finite and non-negative",
                            ));
                        }
                        validate_blocks(&cell.blocks)?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_text_and_length() {
        let mut p = Paragraph::plain("你好");
        p.push_text(" world", CharProps::default());
        assert_eq!(p.text(), "你好 world");
        assert_eq!(p.char_count(), 8);
    }

    #[test]
    fn runs_merge_when_formatting_matches() {
        let mut p = Paragraph::new(ParaId::new(1));
        p.push_text("a", CharProps::default());
        p.push_text("b", CharProps::default());
        p.push_text("c", CharProps::default().with_size(20.0));
        assert_eq!(p.runs.len(), 2);
        assert_eq!(p.runs[0].text, "ab");
    }

    #[test]
    fn hex_colors_round_trip() {
        let c = Color::from_hex("#FF8800").unwrap();
        assert_eq!(c, Color::rgb(255, 136, 0));
        assert_eq!(c.to_hex(), "FF8800");
    }

    #[test]
    fn renumber_is_dense() {
        let mut doc = Document::from_text("a\nb\nc");
        doc.renumber();
        let ids: Vec<u32> = doc.paragraphs().map(|p| p.id.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn validate_rejects_empty_page() {
        let mut doc = Document::new();
        doc.sections[0].props.page.width_pt = 0.0;
        assert!(doc.validate().is_err());
    }

    #[test]
    fn images_are_blocks() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("before"));
        doc.first_section_mut().push_image(ImageBlock::new(
            ImageId::new(1),
            "pixel.png",
            "image/png",
            vec![1u8, 2, 3],
            10.0,
            10.0,
        ));
        doc.push_paragraph(Paragraph::plain("after"));

        assert_eq!(doc.block_count(), 3);
        assert_eq!(doc.paragraph_count(), 2);
        assert_eq!(doc.plain_text(), "before\n[image]\nafter\n");
    }

    #[test]
    fn styles_are_looked_up_by_kind() {
        let mut sheet = StyleSheet::new();
        let mut style = Style::new("Body", "Body Text", StyleKind::Paragraph);
        style.para = Some(ParaProps::default());
        style.char = Some(CharProps::default());
        sheet.insert(style);

        assert!(sheet.paragraph_props("Body").is_some());
        assert!(sheet.char_props("Body").is_some());
        assert!(sheet.get("Missing").is_none());
    }
}

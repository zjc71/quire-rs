#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

//! Native editor shell.
//!
//! This implements the five-zone layout from the UI specification with an
//! original visual language: warm paper canvas, vermilion accent, a single-row
//! command deck, page rail, inspector and status bar.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use eframe::egui;
use egui::{
    Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Frame, Margin, RichText,
    Stroke, TextureHandle, TextureOptions, Vec2,
};
use quire_base::TableId;
use quire_edit::{Editor, TextMatch};
use quire_io::{SheetGrid, read_document, read_sheets, write_document, write_sheets};
use quire_layout::{layout_with_cache, Layout, LayoutCache, LayoutOptions};
use quire_model::{Align, CharProps, ContainerId, Document, ParaProps};
use quire_paint::{render_svg_page, SvgOptions};
use quire_text::SystemMetrics;
use quire_ui::Rasterizer;

#[derive(Parser)]
#[command(name = "quire-view", version, about = "Quire editor shell")]
struct Args {
    input: Option<PathBuf>,
    /// Initial 1-based page number.
    #[arg(long, default_value_t = 1)]
    page: usize,
    /// Headless self-test: layout and rasterize the first page, then exit.
    #[arg(long, hide = true)]
    selftest: bool,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Deck {
    Start,
    Insert,
    Layout,
    Reference,
    Review,
    View,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Inspector {
    Styles,
    Format,
    Outline,
    Comments,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct DocPos {
    paragraph: usize,
    character: usize,
}

struct HitLine {
    page: usize,
    paragraph: usize,
    start: usize,
    end: usize,
    rect: egui::Rect,
    runs: Vec<quire_layout::PlacedRun>,
}

struct HitIndex {
    lines: Vec<HitLine>,
    by_paragraph: HashMap<usize, Vec<usize>>,
    by_page: HashMap<usize, Vec<usize>>,
}

impl HitIndex {
    fn build(layout: &Layout, document: &Document) -> Self {
        let mut id_to_index = HashMap::new();
        let mut texts = Vec::new();
        for index in 0..document.editable_paragraph_count() {
            if let Some(paragraph) = document.paragraph_at_global(index) {
                id_to_index.insert(paragraph.id.get(), index);
                texts.push(paragraph.text());
            }
        }
        let mut cursors = vec![0usize; texts.len()];
        let mut lines = Vec::new();
        let mut by_paragraph: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut by_page: HashMap<usize, Vec<usize>> = HashMap::new();

        for (page_index, page) in layout.pages.iter().enumerate() {
            for line in &page.lines {
                let Some(&paragraph) = id_to_index.get(&line.paragraph) else {
                    continue;
                };
                let line_text: String = line.runs.iter().map(|run| run.text.as_str()).collect();
                let text: Vec<char> = texts[paragraph].chars().collect();
                let line_chars: Vec<char> = line_text.chars().collect();
                let from = cursors[paragraph];
                let start = if line_chars.is_empty() {
                    from
                } else {
                    if from + line_chars.len() > text.len() {
                        continue;
                    }
                    let mut found = None;
                    for index in from..=text.len() - line_chars.len() {
                        if text[index..index + line_chars.len()] == line_chars[..] {
                            found = Some(index);
                            break;
                        }
                    }
                    match found {
                        Some(start) => start,
                        None => continue,
                    }
                };
                let end = start + line_chars.len();
                cursors[paragraph] = end;

                let index = lines.len();
                lines.push(HitLine {
                    page: page_index,
                    paragraph,
                    start,
                    end,
                    rect: egui::Rect::from_min_size(
                        egui::pos2(line.x as f32, line.y as f32),
                        egui::vec2(line.width.max(1.0) as f32, line.height as f32),
                    ),
                    runs: line.runs.clone(),
                });
                by_paragraph.entry(paragraph).or_default().push(index);
                by_page.entry(page_index).or_default().push(index);
            }
        }

        Self {
            lines,
            by_paragraph,
            by_page,
        }
    }

    fn line_at(&self, page: usize, point: egui::Pos2) -> Option<usize> {
        let indices = self.by_page.get(&page)?;
        let mut best: Option<(usize, f32, f32)> = None;
        for &index in indices {
            let line = &self.lines[index];
            let vertical = if point.y < line.rect.top() {
                line.rect.top() - point.y
            } else if point.y > line.rect.bottom() {
                point.y - line.rect.bottom()
            } else {
                0.0
            };
            let horizontal = if point.x < line.rect.left() {
                line.rect.left() - point.x
            } else if point.x > line.rect.right() {
                point.x - line.rect.right()
            } else {
                0.0
            };
            let score = vertical * 1000.0 + horizontal * 0.001;
            if best.map(|(_, best_score, _)| score < best_score).unwrap_or(true) {
                best = Some((index, score, vertical));
            }
        }
        best.map(|(index, _, _)| index)
    }

    fn position_at(
        &self,
        page: usize,
        point: egui::Pos2,
        metrics: &dyn quire_text::Metrics,
    ) -> Option<DocPos> {
        let index = self.line_at(page, point)?;
        let line = &self.lines[index];
        Some(DocPos {
            paragraph: line.paragraph,
            character: self.offset_for_x(line, point.x, metrics),
        })
    }

    fn offset_for_x(
        &self,
        line: &HitLine,
        x: f32,
        metrics: &dyn quire_text::Metrics,
    ) -> usize {
        if line.runs.is_empty() {
            return line.start;
        }
        let mut cursor = line.start;
        for run in &line.runs {
            if x <= run.x as f32 {
                return cursor;
            }
            if x <= (run.x + run.width) as f32 {
                let style = text_style(&run.props);
                let mut pen = run.x as f32;
                for (index, ch) in run.text.chars().enumerate() {
                    let advance = metrics.advance(ch, style) as f32;
                    if x < pen + advance / 2.0 {
                        return cursor + index;
                    }
                    pen += advance;
                }
                return cursor + run.text.chars().count();
            }
            cursor += run.text.chars().count();
        }
        line.end
    }

    fn x_for_offset(
        &self,
        line: &HitLine,
        offset: usize,
        metrics: &dyn quire_text::Metrics,
    ) -> f32 {
        let mut cursor = line.start;
        for run in &line.runs {
            let count = run.text.chars().count();
            if offset <= cursor + count {
                let style = text_style(&run.props);
                let mut x = run.x as f32;
                for (index, ch) in run.text.chars().enumerate() {
                    if cursor + index >= offset {
                        return x;
                    }
                    x += metrics.advance(ch, style) as f32;
                }
                return x;
            }
            cursor += count;
        }
        line.runs
            .last()
            .map(|run| (run.x + run.width) as f32)
            .unwrap_or(line.rect.left())
    }

    fn line_index_for(&self, position: DocPos) -> Option<usize> {
        let indices = self.by_paragraph.get(&position.paragraph)?;
        let mut chosen = None;
        for &index in indices {
            let line = &self.lines[index];
            if position.character >= line.start && position.character <= line.end {
                chosen = Some(index);
            }
        }
        chosen.or_else(|| indices.last().copied())
    }

    fn range_rects(
        &self,
        page: usize,
        start: DocPos,
        end: DocPos,
        metrics: &dyn quire_text::Metrics,
    ) -> Vec<egui::Rect> {
        let mut rects = Vec::new();
        if start.paragraph > end.paragraph
            || (start.paragraph == end.paragraph && start.character >= end.character)
        {
            return rects;
        }
        let Some(indices) = self.by_page.get(&page) else {
            return rects;
        };
        for &line_index in indices {
            let line = &self.lines[line_index];
            if line.paragraph < start.paragraph || line.paragraph > end.paragraph {
                continue;
            }
            let from = if line.paragraph == start.paragraph {
                line.start.max(start.character)
            } else {
                line.start
            };
            let to = if line.paragraph == end.paragraph {
                line.end.min(end.character)
            } else {
                line.end
            };
            if from >= to {
                continue;
            }
            let x1 = self.x_for_offset(line, from, metrics);
            let x2 = self.x_for_offset(line, to, metrics);
            rects.push(egui::Rect::from_min_size(
                egui::pos2(x1, line.rect.top()),
                egui::vec2((x2 - x1).max(1.0), line.rect.height()),
            ));
        }
        rects
    }

    fn caret_rect(
        &self,
        position: DocPos,
        metrics: &dyn quire_text::Metrics,
    ) -> Option<(usize, egui::Rect)> {
        let index = self.line_index_for(position)?;
        let line = &self.lines[index];
        let x = self.x_for_offset(line, position.character, metrics);
        Some((
            line.page,
            egui::Rect::from_min_size(
                egui::pos2(x, line.rect.top()),
                egui::vec2(1.5, line.rect.height()),
            ),
        ))
    }
}

fn text_style(props: &CharProps) -> quire_text::TextStyle<'_> {
    quire_text::TextStyle {
        size_pt: props.size_pt,
        bold: props.bold,
        italic: props.italic,
        family: props.font_family.as_deref(),
        east_asian: props.font_family_east_asian.as_deref(),
    }
}

#[derive(Clone, Debug)]
struct SheetEdit {
    row: usize,
    column: usize,
    text: String,
    focus: bool,
}

struct SheetState {
    path: Option<PathBuf>,
    sheets: Vec<SheetGrid>,
    active: usize,
    selected: (usize, usize),
    range_anchor: (usize, usize),
    edit: Option<SheetEdit>,
    dirty: bool,
    status: Option<String>,
    scroll_to: Option<(usize, usize)>,
    zoom: f32,
    undo: Vec<Vec<SheetGrid>>,
    redo: Vec<Vec<SheetGrid>>,
    find_open: bool,
    find_query: String,
    find_matches: Vec<(usize, usize)>,
    find_index: usize,
    find_focus: bool,
}

#[derive(Copy, Clone)]
enum SheetAction {
    EditCell,
    CutCell,
    ClearSelection,
    Undo,
    Redo,
    InsertRow,
    DeleteRow,
    InsertColumn,
    DeleteColumn,
}

impl SheetState {
    fn open(path: &Path) -> Result<Self> {
        let sheets = read_sheets(path)?;
        if sheets.is_empty() {
            return Err(anyhow!("工作簿里没有工作表"));
        }
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let direct_save = extension == "csv";
        let status = if direct_save {
            None
        } else {
            Some("已读取单元格缓存值；保存时会新建工作簿".to_string())
        };
        Ok(Self {
            path: direct_save.then(|| path.to_path_buf()),
            sheets,
            active: 0,
            selected: (0, 0),
            range_anchor: (0, 0),
            edit: None,
            dirty: false,
            status,
            scroll_to: None,
            zoom: 1.0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_open: false,
            find_query: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_focus: false,
        })
    }

    fn checkpoint(&mut self) {
        self.undo.push(self.sheets.clone());
        if self.undo.len() > 32 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.sheets, previous);
        self.redo.push(current);
        if self.redo.len() > 32 {
            self.redo.remove(0);
        }
        self.after_history();
        true
    }

    fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.sheets, next);
        self.undo.push(current);
        if self.undo.len() > 32 {
            self.undo.remove(0);
        }
        self.after_history();
        true
    }

    fn after_history(&mut self) {
        self.edit = None;
        self.dirty = true;
        self.active = self.active.min(self.sheets.len().saturating_sub(1));
        let (rows, columns) = self.dimensions();
        self.selected = (
            self.selected.0.min(rows.saturating_sub(1)),
            self.selected.1.min(columns.saturating_sub(1)),
        );
        self.range_anchor = (
            self.range_anchor.0.min(rows.saturating_sub(1)),
            self.range_anchor.1.min(columns.saturating_sub(1)),
        );
        self.status = Some("已撤销 / 重做".to_string());
    }

    fn refresh_find(&mut self) {
        self.find_matches.clear();
        self.find_index = 0;
        if self.find_query.is_empty() {
            return;
        }
        let needle = self.find_query.to_lowercase();
        let rows = self.active_sheet().row_count();
        let columns = self.active_sheet().column_count();
        for row in 0..rows {
            for column in 0..columns {
                if self
                    .active_sheet()
                    .display_cell(row, column)
                    .to_lowercase()
                    .contains(&needle)
                    || self
                        .active_sheet()
                        .cell(row, column)
                        .to_lowercase()
                        .contains(&needle)
                {
                    self.find_matches.push((row, column));
                }
            }
        }
    }

    fn select_find(&mut self, index: usize) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_index = index % self.find_matches.len();
        let (row, column) = self.find_matches[self.find_index];
        self.range_anchor = (row, column);
        self.selected = (row, column);
        self.scroll_to = Some((row, column));
        self.status = Some(format!(
            "找到 {} 个，第 {} 个",
            self.find_matches.len(),
            self.find_index + 1
        ));
    }

    fn find_next(&mut self, forward: bool) {
        self.refresh_find();
        if self.find_matches.is_empty() {
            self.status = Some("没有匹配".to_string());
            return;
        }
        let index = if forward {
            (self.find_index + 1) % self.find_matches.len()
        } else {
            (self.find_index + self.find_matches.len() - 1) % self.find_matches.len()
        };
        self.select_find(index);
    }

    fn active_sheet(&self) -> &SheetGrid {
        &self.sheets[self.active.min(self.sheets.len().saturating_sub(1))]
    }

    fn active_sheet_mut(&mut self) -> &mut SheetGrid {
        let index = self.active.min(self.sheets.len().saturating_sub(1));
        &mut self.sheets[index]
    }

    fn dimensions(&self) -> (usize, usize) {
        let sheet = self.active_sheet();
        (
            sheet.row_count().max(200),
            sheet.column_count().max(26),
        )
    }

    fn display_cell(&self, row: usize, column: usize) -> String {
        self.active_sheet().display_cell(row, column)
    }

    fn cell(&self, row: usize, column: usize) -> &str {
        self.active_sheet().cell(row, column)
    }

    fn ensure_cell(&mut self, row: usize, column: usize) {
        let sheet = self.active_sheet_mut();
        while sheet.rows.len() <= row {
            sheet.rows.push(Vec::new());
        }
        for line in &mut sheet.rows {
            while line.len() <= column {
                line.push(String::new());
            }
        }
    }

    fn set_cell(&mut self, row: usize, column: usize, value: String) {
        self.ensure_cell(row, column);
        let sheet = self.active_sheet_mut();
        if let Some(line) = sheet.rows.get_mut(row) {
            if let Some(cell) = line.get_mut(column) {
                if *cell != value {
                    *cell = value;
                    self.dirty = true;
                }
            }
        }
    }

    fn set_selected(&mut self, row: usize, column: usize, extend: bool) {
        if !extend {
            self.range_anchor = (row, column);
        }
        self.selected = (row, column);
        self.scroll_to = Some((row, column));
    }

    fn range_bounds(&self) -> ((usize, usize), (usize, usize)) {
        (
            (
                self.range_anchor.0.min(self.selected.0),
                self.range_anchor.1.min(self.selected.1),
            ),
            (
                self.range_anchor.0.max(self.selected.0),
                self.range_anchor.1.max(self.selected.1),
            ),
        )
    }

    fn selection_reference(&self) -> String {
        let (start, end) = self.range_bounds();
        if start == end {
            sheet_cell_reference(start.0, start.1)
        } else {
            format!(
                "{}:{}",
                sheet_cell_reference(start.0, start.1),
                sheet_cell_reference(end.0, end.1)
            )
        }
    }

    fn selection_text(&self) -> String {
        let (start, end) = self.range_bounds();
        let mut lines = Vec::new();
        for row in start.0..=end.0 {
            let mut values = Vec::new();
            for column in start.1..=end.1 {
                values.push(self.display_cell(row, column));
            }
            lines.push(values.join("\t"));
        }
        lines.join("\n")
    }

    fn clear_range(&mut self) {
        let (start, end) = self.range_bounds();
        let mut any = false;
        for row in start.0..=end.0 {
            if let Some(line) = self.active_sheet().rows.get(row) {
                if (start.1..=end.1)
                    .any(|column| line.get(column).is_some_and(|value| !value.is_empty()))
                {
                    any = true;
                    break;
                }
            }
        }
        if !any {
            return;
        }
        self.checkpoint();
        let sheet = self.active_sheet_mut();
        for row in start.0..=end.0 {
            let Some(line) = sheet.rows.get_mut(row) else {
                continue;
            };
            for column in start.1..=end.1 {
                if let Some(cell) = line.get_mut(column) {
                    cell.clear();
                }
            }
        }
        self.dirty = true;
    }

    fn paste_text(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let start = self.selected;
        self.checkpoint();
        for (row_offset, line) in normalized.split('\n').enumerate() {
            let row = start.0 + row_offset;
            for (column_offset, value) in line.split('\t').enumerate() {
                self.set_cell(row, start.1 + column_offset, value.to_string());
            }
        }
        let rows = normalized.split('\n').count();
        let columns = normalized
            .split('\n')
            .map(|line| line.split('\t').count())
            .max()
            .unwrap_or(1);
        self.range_anchor = start;
        self.selected = (
            start.0 + rows.saturating_sub(1),
            start.1 + columns.saturating_sub(1),
        );
        self.scroll_to = Some(self.selected);
        self.dirty = true;
        self.status = Some("已粘贴".to_string());
    }

    fn begin_edit(&mut self, initial: Option<String>) {
        let (row, column) = self.selected;
        let text = initial.unwrap_or_else(|| self.cell(row, column).to_string());
        self.edit = Some(SheetEdit {
            row,
            column,
            text,
            focus: true,
        });
    }

    fn commit_edit(&mut self) {
        if let Some(edit) = self.edit.take() {
            if self.cell(edit.row, edit.column) != edit.text {
                self.checkpoint();
            }
            self.set_cell(edit.row, edit.column, edit.text);
        }
    }

    fn cancel_edit(&mut self) {
        self.edit = None;
    }

    fn move_selection(&mut self, row_delta: isize, column_delta: isize) {
        self.move_selection_extended(row_delta, column_delta, false);
    }

    fn move_selection_extended(
        &mut self,
        row_delta: isize,
        column_delta: isize,
        extend: bool,
    ) {
        let (rows, columns) = self.dimensions();
        let next_row = (self.selected.0 as isize + row_delta)
            .clamp(0, rows.saturating_sub(1) as isize) as usize;
        let next_column = (self.selected.1 as isize + column_delta)
            .clamp(0, columns.saturating_sub(1) as isize) as usize;
        if !extend {
            self.range_anchor = (next_row, next_column);
        }
        self.selected = (next_row, next_column);
        self.scroll_to = Some(self.selected);
    }

    fn insert_row(&mut self) {
        self.checkpoint();
        let row = self.selected.0;
        let sheet = self.active_sheet_mut();
        sheet.rows.insert(row.min(sheet.rows.len()), Vec::new());
        self.dirty = true;
        self.selected = (row, self.selected.1);
        self.scroll_to = Some(self.selected);
    }

    fn delete_row(&mut self) {
        if self.active_sheet().rows.is_empty() {
            return;
        }
        self.checkpoint();
        let row = self.selected.0;
        let rows = {
            let sheet = self.active_sheet_mut();
            if sheet.rows.is_empty() {
                return;
            }
            sheet.rows.remove(row.min(sheet.rows.len() - 1));
            sheet.rows.len().max(1)
        };
        self.dirty = true;
        self.selected = (row.min(rows - 1), self.selected.1);
    }

    fn insert_column(&mut self) {
        self.checkpoint();
        let column = self.selected.1;
        let sheet = self.active_sheet_mut();
        for line in &mut sheet.rows {
            if column <= line.len() {
                line.insert(column, String::new());
            } else {
                while line.len() < column {
                    line.push(String::new());
                }
                line.push(String::new());
            }
        }
        self.dirty = true;
        self.scroll_to = Some(self.selected);
    }

    fn delete_column(&mut self) {
        if self.active_sheet().column_count() == 0 {
            return;
        }
        self.checkpoint();
        let column = self.selected.1;
        let sheet = self.active_sheet_mut();
        let columns = sheet
            .rows
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0);
        if columns == 0 {
            return;
        }
        for line in &mut sheet.rows {
            if column < line.len() {
                line.remove(column);
            }
        }
        self.dirty = true;
        let columns = columns.saturating_sub(1).max(1);
        self.selected = (self.selected.0, column.min(columns - 1));
    }
}

struct QuireApp {
    title: String,
    editor: Editor,
    layout: Layout,
    hit: HitIndex,
    caret: DocPos,
    anchor: DocPos,
    path: Option<PathBuf>,
    welcome: bool,
    canvas_id: egui::Id,
    composition: String,
    status_message: Option<String>,
    dirty: bool,
    find_open: bool,
    find_query: String,
    replace_query: String,
    find_case: bool,
    find_matches: Vec<TextMatch>,
    find_index: usize,
    find_selected: bool,
    find_focus: bool,
    show_rail: bool,
    show_inspector: bool,
    focus_mode: bool,
    metrics: SystemMetrics,
    system_fonts: bool,
    page: usize,
    zoom: f32,
    last_zoom: f32,
    fit: bool,
    rasterizer: Rasterizer,
    layout_cache: LayoutCache,
    page_fingerprints: Vec<u64>,
    page_textures: HashMap<(usize, u32), (TextureHandle, u64)>,
    thumbnails: HashMap<(usize, u32), (TextureHandle, u64)>,
    cache_tick: u64,
    pending_scroll: Option<usize>,
    ime_caret_screen: Option<egui::Rect>,
    deck: Deck,
    inspector: Inspector,
    dark: bool,
    palette_open: bool,
    palette_focus: bool,
    palette_query: String,
    table_dialog_open: bool,
    table_rows: usize,
    table_columns: usize,
    sheet: Option<SheetState>,
}

const PAPER: Color32 = Color32::from_rgb(0xF3, 0xEF, 0xE8);
const SURFACE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
const RAISED: Color32 = Color32::from_rgb(0xFB, 0xF9, 0xF5);
const INK: Color32 = Color32::from_rgb(0x1C, 0x1B, 0x18);
const MUTED: Color32 = Color32::from_rgb(0x5E, 0x5A, 0x52);
const LINE: Color32 = Color32::from_rgb(0xE4, 0xDE, 0xD3);
const ACCENT: Color32 = Color32::from_rgb(0xD9, 0x48, 0x0F);
const ACCENT_SOFT: Color32 = Color32::from_rgb(0xFD, 0xEB, 0xE2);

impl QuireApp {
    #[allow(clippy::too_many_arguments)]
    fn new(
        context: &egui::Context,
        title: String,
        path: Option<PathBuf>,
    welcome: bool,
        editor: Editor,
        layout: Layout,
        metrics: SystemMetrics,
        layout_cache: LayoutCache,
        system_fonts: bool,
        page: usize,
    ) -> Self {
        configure_fonts(context);
        let hit = HitIndex::build(&layout, editor.document());
        let page_fingerprints = page_fingerprints(&layout);
        let app = Self {
            title,
            editor,
            layout,
            hit,
            caret: DocPos::default(),
            anchor: DocPos::default(),
            path,
            welcome,
            canvas_id: egui::Id::new("quire-canvas"),
            composition: String::new(),
            status_message: None,
            dirty: false,
            find_open: false,
            find_query: String::new(),
            replace_query: String::new(),
            find_case: false,
            find_matches: Vec::new(),
            find_index: 0,
            find_selected: false,
            find_focus: false,
            show_rail: true,
            show_inspector: true,
            focus_mode: false,
            metrics,
            system_fonts,
            page,
            zoom: 1.0,
            last_zoom: 1.0,
            fit: true,
            rasterizer: Rasterizer::new(),
            layout_cache,
            page_fingerprints,
            page_textures: HashMap::new(),
            thumbnails: HashMap::new(),
            cache_tick: 0,
            pending_scroll: Some(page),
            ime_caret_screen: None,
            deck: Deck::Start,
            inspector: Inspector::Styles,
            dark: false,
            palette_open: false,
            palette_focus: false,
            palette_query: String::new(),
            table_dialog_open: false,
            table_rows: 3,
            table_columns: 3,
            sheet: None,
        };
        app.apply_theme(context);
        app
    }

    fn apply_theme(&self, context: &egui::Context) {
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let (canvas, surface, ink, muted, line) = if self.dark {
            (
                Color32::from_rgb(0x14, 0x13, 0x11),
                Color32::from_rgb(0x1E, 0x1C, 0x19),
                Color32::from_rgb(0xED, 0xE8, 0xDF),
                Color32::from_rgb(0xB3, 0xAC, 0xA0),
                Color32::from_rgb(0x33, 0x2F, 0x29),
            )
        } else {
            (PAPER, SURFACE, INK, MUTED, LINE)
        };
        visuals.panel_fill = canvas;
        visuals.window_fill = surface;
        visuals.window_stroke = Stroke::new(1.0_f32, line);
        visuals.extreme_bg_color = RAISED;
        visuals.override_text_color = Some(ink);
        visuals.hyperlink_color = ACCENT;
        visuals.selection.bg_fill = ACCENT_SOFT;
        visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
        visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xF1, 0xEC, 0xE3);
        visuals.widgets.active.weak_bg_fill = ACCENT_SOFT;
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, ink);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, muted);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
        visuals.widgets.active.corner_radius = CornerRadius::same(6);
        context.set_visuals(visuals);

        let mut style = (*context.style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(10.0, 6.0);
        style.spacing.menu_margin = Margin::same(8);
        context.set_style(style);
    }

    fn page_count(&self) -> usize {
        self.layout.pages.len().max(1)
    }

    fn open_document(&mut self, path: &Path) -> Result<()> {
        if is_sheet_path(path) {
            return self.open_spreadsheet(path);
        }
        let document = read_document(path)?;
        let editor = Editor::new(document);
        let mut cache = LayoutCache::default();
        let layout = layout_with_cache(
            editor.document(),
            &LayoutOptions::default(),
            &self.metrics,
            &mut cache,
        )
        .map_err(|error| anyhow!("layout failed: {error}"))?;
        let hit = HitIndex::build(&layout, editor.document());
        let fingerprints = page_fingerprints(&layout);

        self.sheet = None;
        self.editor = editor;
        self.layout = layout;
        self.layout_cache = cache;
        self.hit = hit;
        self.page_fingerprints = fingerprints;
        self.page = 0;
        self.pending_scroll = Some(0);
        self.caret = DocPos::default();
        self.anchor = self.caret;
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let direct_save = matches!(
            extension.as_str(),
            "docx" | "docm" | "dotx" | "rtf" | "txt" | "md"
        );
        self.path = if direct_save {
            Some(path.to_path_buf())
        } else {
            None
        };
        self.welcome = false;
        self.title = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "document".to_string());
        self.dirty = false;
        self.page_textures.clear();
        self.thumbnails.clear();
        self.find_open = false;
        self.find_matches.clear();
        self.table_dialog_open = false;
        self.status_message = if direct_save {
            None
        } else {
            Some("已导入；保存时请选择 DOCX / RTF / TXT".to_string())
        };
        self.ime_caret_screen = None;
        Ok(())
    }

    fn handle_drop(&mut self, context: &egui::Context) {
        let dropped = context.input(|input| {
            input
                .raw
                .dropped_files
                .first()
                .and_then(|file| file.path.clone())
        });
        if let Some(path) = dropped {
            if path
                .extension()
                .map(|extension| {
                    let extension = extension.to_string_lossy().to_ascii_lowercase();
                    matches!(
                        extension.as_str(),
                        "docx" | "docm" | "dotx" | "rtf" | "odt" | "txt" | "md" | "xlsx"
                            | "xlsm" | "csv"
                    )
                })
                .unwrap_or(false)
            {
                if let Err(error) = self.open_document(&path) {
                    if let Some(sheet) = &mut self.sheet {
                        sheet.status = Some(format!("打开失败: {error}"));
                    } else {
                        self.status_message = Some(format!("打开失败: {error}"));
                    }
                }
            }
        }
    }

    fn welcome_screen(&mut self, context: &egui::Context) {
        let canvas = if self.dark {
            Color32::from_rgb(0x14, 0x13, 0x11)
        } else {
            PAPER
        };
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(canvas))
            .show(context, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(72.0);
                    ui.label(RichText::new("Q").size(64.0).strong().color(ACCENT));
                    ui.label(RichText::new("Quire").size(34.0).strong());
                    ui.label(
                        RichText::new("Rust 文档编辑器 · DOCX / RTF / ODT / TXT · XLSX/CSV 导入 · 无登录 · 无广告")
                            .size(14.0)
                            .color(MUTED),
                    );
                    ui.add_space(28.0);
                    if ui
                        .add_sized([220.0, 38.0], egui::Button::new("打开文档"))
                        .clicked()
                    {
                        if let Some(path) = pick_document() {
                            if let Err(error) = self.open_document(&path) {
                                self.status_message = Some(format!("打开失败: {error}"));
                            }
                        }
                    }
                    ui.add_space(8.0);
                    if ui
                        .add_sized([220.0, 38.0], egui::Button::new("新建空白文档"))
                        .clicked()
                    {
                        self.new_document();
                    }
                    ui.add_space(18.0);
                    ui.label(
                        RichText::new("也可以把 DOCX / RTF / ODT / TXT / XLSX / CSV 拖到窗口，或拖到 Quire.exe 上")
                            .size(12.0)
                            .color(MUTED),
                    );
                });
            });
    }

    fn insert_image_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("插入图片")
            .add_filter("图片", &["png", "jpg", "jpeg", "gif", "bmp", "webp"])
            .pick_file()
        else {
            return;
        };
        let Ok(data) = std::fs::read(&path) else {
            self.status_message = Some("无法读取图片".to_string());
            return;
        };
        let Ok((width, height)) = image::image_dimensions(&path) else {
            self.status_message = Some("无法识别图片尺寸".to_string());
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "image.png".to_string());
        let mime = mime_for_extension(
            &path
                .extension()
                .map(|extension| extension.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
        );
        let width_pt = f64::from(width) * 72.0 / 96.0;
        let height_pt = f64::from(height) * 72.0 / 96.0;
        let paragraph = self.paragraph_range().1;
        if matches!(
            self.editor.document().paragraph_location(paragraph),
            Some((ContainerId::TableCell { .. }, _))
        ) {
            self.status_message = Some("表格内暂不支持插入图片".to_string());
            return;
        }
        if let Some((ContainerId::Body { section }, local)) =
            self.editor.document().paragraph_location(paragraph)
        {
            if self
                .editor
                .insert_image(section, local, &name, mime, data, width_pt, height_pt)
                .is_ok()
            {
                self.after_edit();
                self.status_message = Some("已插入图片".to_string());
            }
        }
    }

    fn table_context(&self) -> Option<(TableId, usize, usize)> {
        let (container, _) = self
            .editor
            .document()
            .paragraph_location(self.caret.paragraph)?;
        match container {
            ContainerId::TableCell { table, row, cell } => Some((table, row, cell)),
            ContainerId::Body { .. } => None,
        }
    }

    fn set_caret_to_table_cell(&mut self, table: TableId, row: usize, column: usize) {
        let Some(id) = self
            .editor
            .document()
            .table_by_id(table)
            .and_then(|table| table.rows.get(row))
            .and_then(|row| row.cells.get(column))
            .and_then(|cell| {
                cell.blocks.iter().find_map(|block| match block {
                    quire_model::Block::Paragraph(paragraph) => Some(paragraph.id),
                    _ => None,
                })
            })
        else {
            return;
        };
        if let Some(index) = self.editor.document().global_index_of_paragraph(id) {
            self.caret = DocPos {
                paragraph: index,
                character: 0,
            };
            self.anchor = self.caret;
        }
    }

    fn move_table_cell(&mut self, backwards: bool) {
        let Some((table, row, column)) = self.table_context() else {
            return;
        };
        let Some((rows, columns)) = self
            .editor
            .document()
            .table_by_id(table)
            .map(|table| (table.rows.len(), table.grid.len()))
        else {
            return;
        };
        if columns == 0 || rows == 0 {
            return;
        }
        let mut added_row = false;
        let (target_row, target_column) = if backwards {
            if column > 0 {
                (row, column - 1)
            } else if row > 0 {
                (row - 1, columns - 1)
            } else {
                return;
            }
        } else if column + 1 < columns {
            (row, column + 1)
        } else if row + 1 < rows {
            (row + 1, 0)
        } else {
            if self.editor.table_insert_row(table, rows - 1).is_err() {
                return;
            }
            added_row = true;
            (rows, 0)
        };
        self.set_caret_to_table_cell(table, target_row, target_column);
        if added_row {
            self.status_message = Some("已在表格末尾添加一行".to_string());
            self.after_edit();
        }
    }

    fn toggle_table_header(&mut self) {
        let Some((table, _, _)) = self.table_context() else {
            return;
        };
        match self.editor.table_toggle_header_shading(table) {
            Ok(()) => {
                self.after_edit();
                self.status_message = Some("已切换表头底色".to_string());
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn delete_current_table(&mut self) {
        let Some((table, _, _)) = self.table_context() else {
            return;
        };
        match self.editor.table_delete(table) {
            Ok(()) => {
                self.caret = self.clamp_pos(DocPos::default());
                self.anchor = self.caret;
                self.after_edit();
                self.status_message = Some("已删除表格".to_string());
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn add_table_row(&mut self) {
        let Some((table, row, column)) = self.table_context() else {
            return;
        };
        match self.editor.table_insert_row(table, row) {
            Ok(()) => {
                self.set_caret_to_table_cell(table, row + 1, column);
                self.after_edit();
                self.status_message = Some("已在下方插入行".to_string());
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn add_table_row_above(&mut self) {
        let Some((table, row, column)) = self.table_context() else {
            return;
        };
        match self.editor.table_insert_row_before(table, row) {
            Ok(()) => {
                self.set_caret_to_table_cell(table, row, column);
                self.after_edit();
                self.status_message = Some("已在上方插入行".to_string());
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn add_table_column(&mut self) {
        let Some((table, row, column)) = self.table_context() else {
            return;
        };
        match self.editor.table_insert_column(table, column) {
            Ok(()) => {
                self.set_caret_to_table_cell(table, row, column + 1);
                self.after_edit();
                self.status_message = Some("已在右侧插入列".to_string());
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn add_table_column_left(&mut self) {
        let Some((table, row, column)) = self.table_context() else {
            return;
        };
        match self.editor.table_insert_column_before(table, column) {
            Ok(()) => {
                self.set_caret_to_table_cell(table, row, column);
                self.after_edit();
                self.status_message = Some("已在左侧插入列".to_string());
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn remove_table_row(&mut self) {
        let Some((table, row, column)) = self.table_context() else {
            return;
        };
        match self.editor.table_delete_row(table, row) {
            Ok(()) => {
                let rows = self
                    .editor
                    .document()
                    .table_by_id(table)
                    .map(|table| table.rows.len())
                    .unwrap_or(1);
                self.set_caret_to_table_cell(
                    table,
                    row.min(rows.saturating_sub(1)),
                    column,
                );
                self.after_edit();
                self.status_message = Some("已删除行".to_string());
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn remove_table_column(&mut self) {
        let Some((table, row, column)) = self.table_context() else {
            return;
        };
        match self.editor.table_delete_column(table, column) {
            Ok(()) => {
                let (rows, columns) = self
                    .editor
                    .document()
                    .table_by_id(table)
                    .map(|table| (table.rows.len(), table.grid.len()))
                    .unwrap_or((1, 1));
                self.set_caret_to_table_cell(
                    table,
                    row.min(rows.saturating_sub(1)),
                    column.min(columns.saturating_sub(1)),
                );
                self.after_edit();
                self.status_message = Some("已删除列".to_string());
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn insert_table_at_caret(&mut self) {
        let Some((container, local)) =
            self.editor.document().paragraph_location(self.caret.paragraph)
        else {
            self.status_message = Some("请先把光标放在正文段落中".to_string());
            return;
        };
        let ContainerId::Body { section } = container else {
            self.status_message = Some("表格内暂不支持插入嵌套表格".to_string());
            return;
        };
        let rows = self.table_rows.clamp(1, 20);
        let columns = self.table_columns.clamp(1, 12);
        match self.editor.insert_table(section, local, rows, columns) {
            Ok(table) => {
                self.set_caret_to_table_cell(table, 0, 0);
                self.after_edit();
                self.status_message =
                    Some("已插入表格；单击单元格编辑，表格工具在“插入”面板".to_string());
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn table_dialog(&mut self, context: &egui::Context) {
        if !self.table_dialog_open {
            return;
        }
        let mut open = true;
        let mut insert = false;
        egui::Window::new("插入表格")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 160.0])
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label("行");
                    ui.add(egui::DragValue::new(&mut self.table_rows).range(1..=20));
                    ui.label("列");
                    ui.add(
                        egui::DragValue::new(&mut self.table_columns).range(1..=12),
                    );
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{} 行 × {} 列",
                        self.table_rows, self.table_columns
                    ))
                    .size(12.0)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                if ui.button("插入表格").clicked() {
                    insert = true;
                }
            });
        if insert {
            self.table_dialog_open = false;
            self.insert_table_at_caret();
            context.memory_mut(|memory| memory.request_focus(self.canvas_id));
        }
        if !open {
            self.table_dialog_open = false;
        }
    }

    fn open_spreadsheet(&mut self, path: &Path) -> Result<()> {
        let sheet = SheetState::open(path)?;
        self.title = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "工作表".to_string());
        self.sheet = Some(sheet);
        self.welcome = false;
        self.find_open = false;
        self.status_message = None;
        Ok(())
    }

    fn save_sheet(&mut self) {
        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        sheet.commit_edit();
        let path = match &sheet.path {
            Some(path) => path.clone(),
            None => {
                let default_name = Path::new(&self.title)
                    .with_extension("xlsx")
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_else(|| "工作簿.xlsx".to_string());
                let Some(mut path) = rfd::FileDialog::new()
                    .set_title("保存表格")
                    .add_filter("Excel 工作簿", &["xlsx"])
                    .add_filter("CSV", &["csv"])
                    .set_file_name(default_name)
                    .save_file()
                else {
                    return;
                };
                if path.extension().is_none() {
                    path.set_extension("xlsx");
                }
                path
            }
        };
        let csv_multi = path
            .extension()
            .map(|value| value.to_string_lossy().eq_ignore_ascii_case("csv"))
            .unwrap_or(false)
            && sheet.sheets.len() > 1;
        match write_sheets(&path, &sheet.sheets) {
            Ok(()) => {
                sheet.path = Some(path.clone());
                sheet.dirty = false;
                sheet.status = Some(if csv_multi {
                    "已保存 CSV（只写了第一个工作表）".to_string()
                } else {
                    "已保存（仅单元格值）".to_string()
                });
                self.title = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "工作表".to_string());
            }
            Err(error) => sheet.status = Some(format!("保存失败: {error}")),
        }
    }

    fn sheet_title_bar(&mut self, context: &egui::Context) {
        let dirty = self.sheet.as_ref().map(|sheet| sheet.dirty).unwrap_or(false);
        egui::TopBottomPanel::top("sheet-title")
            .exact_height(44.0)
            .frame(Frame::NONE.fill(RAISED).inner_margin(Margin::symmetric(16, 8)))
            .show(context, |ui| {
                ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("Q").size(22.0).strong().color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new(&self.title).size(14.0).strong());
                    ui.label(
                        RichText::new(if dirty { "· 已修改" } else { "· 已保存" })
                            .size(12.0)
                            .color(if dirty { ACCENT } else { MUTED }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("深色 / 浅色").clicked() {
                            self.dark = !self.dark;
                            self.apply_theme(context);
                        }
                        if ui.button("保存").clicked() {
                            self.save_sheet();
                        }
                        if ui.button("打开").clicked() {
                            if let Some(path) = pick_document() {
                                if let Err(error) = self.open_document(&path) {
                                    if let Some(sheet) = &mut self.sheet {
                                        sheet.status = Some(format!("打开失败: {error}"));
                                    }
                                }
                            }
                        }
                    });
                });
            });
    }

    fn sheet_find_bar(&mut self, context: &egui::Context) {
        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        if !sheet.find_open {
            return;
        }
        let mut refresh = false;
        let mut next = false;
        let mut previous = false;
        let mut close = false;
        egui::TopBottomPanel::top("sheet-find")
            .exact_height(38.0)
            .frame(Frame::NONE.fill(RAISED).inner_margin(Margin::symmetric(16, 5)))
            .show(context, |ui| {
                ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal_centered(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut sheet.find_query)
                            .hint_text("查找内容")
                            .desired_width(220.0),
                    );
                    if sheet.find_focus {
                        response.request_focus();
                        sheet.find_focus = false;
                    }
                    if response.changed() {
                        refresh = true;
                    }
                    if response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        next = true;
                    }
                    if ui.button("上一个").clicked() {
                        previous = true;
                    }
                    if ui.button("下一个").clicked() {
                        next = true;
                    }
                    let count = if sheet.find_matches.is_empty() {
                        "0".to_string()
                    } else {
                        format!(
                            "{} / {}",
                            sheet.find_index + 1,
                            sheet.find_matches.len()
                        )
                    };
                    ui.label(RichText::new(count).size(12.0).color(MUTED));
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });
            });
        if refresh {
            sheet.refresh_find();
            if !sheet.find_matches.is_empty() {
                sheet.select_find(0);
            }
        }
        if previous {
            sheet.find_next(false);
        }
        if next {
            sheet.find_next(true);
        }
        if close {
            sheet.find_open = false;
        }
    }

    fn sheet_toolbar(&mut self, context: &egui::Context) {
        let Some(sheet) = self.sheet.as_ref() else {
            return;
        };
        let (row, column) = sheet.selected;
        let reference = sheet_cell_reference(row, column);
        let sheet_name = sheet.active_sheet().name.clone();
        let rows = sheet.active_sheet().row_count();
        let columns = sheet.active_sheet().column_count();
        let mut value = sheet.cell(row, column).to_string();
        let can_undo = sheet.can_undo();
        let can_redo = sheet.can_redo();
        let cell_value = sheet.selection_text();
        let mut copy_value = None;
        let mut changed = None;
        let mut action = None;
        let mut move_down = false;
        let mut formula_focus = false;
        let mut open_find = false;
        let mut select_all = false;
        let mut jump_home = false;
        let mut zoom_in = false;
        let mut zoom_out = false;
        egui::TopBottomPanel::top("sheet-toolbar")
            .exact_height(76.0)
            .frame(Frame::NONE.fill(SURFACE).inner_margin(Margin::symmetric(16, 6)))
            .show(context, |ui| {
                ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal_centered(|ui| {
                    Frame::NONE
                        .fill(RAISED)
                        .stroke(Stroke::new(1.0_f32, LINE))
                        .corner_radius(6)
                        .inner_margin(Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            ui.label(RichText::new(reference.clone()).monospace().size(13.0));
                        });
                    ui.label(RichText::new(sheet_name).size(12.0).color(MUTED));
                    let formula_width = (ui.available_width() - 140.0).max(180.0);
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut value)
                            .hint_text("输入单元格内容")
                            .desired_width(formula_width),
                    );
                    if response.gained_focus() {
                        formula_focus = true;
                    }
                    if response.changed() {
                        changed = Some(value.clone());
                    }
                    if response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        move_down = true;
                    }
                    ui.label(
                        RichText::new(format!("{rows} 行 × {columns} 列"))
                            .size(11.0)
                            .color(MUTED),
                    );
                });
                ui.add_space(4.0);
                ui.horizontal_centered(|ui| {
                    if ui
                        .add_enabled(can_undo, egui::Button::new("撤销"))
                        .clicked()
                    {
                        action = Some(SheetAction::Undo);
                    }
                    if ui
                        .add_enabled(can_redo, egui::Button::new("重做"))
                        .clicked()
                    {
                        action = Some(SheetAction::Redo);
                    }
                    if ui.button("复制").clicked() {
                        copy_value = Some(cell_value.clone());
                    }
                    if ui.button("剪切").clicked() {
                        copy_value = Some(cell_value.clone());
                        action = Some(SheetAction::CutCell);
                    }
                    if ui.button("编辑").clicked() {
                        action = Some(SheetAction::EditCell);
                    }
                    ui.separator();
                    if ui.button("插入行").clicked() {
                        action = Some(SheetAction::InsertRow);
                    }
                    if ui.button("删除行").clicked() {
                        action = Some(SheetAction::DeleteRow);
                    }
                    if ui.button("插入列").clicked() {
                        action = Some(SheetAction::InsertColumn);
                    }
                    if ui.button("删除列").clicked() {
                        action = Some(SheetAction::DeleteColumn);
                    }
                    if ui.button("清空").clicked() {
                        action = Some(SheetAction::ClearSelection);
                    }
                    ui.separator();
                    if ui.button("查找").clicked() {
                        open_find = true;
                    }
                    if ui.button("全选").clicked() {
                        select_all = true;
                    }
                    if ui.button("A1").clicked() {
                        jump_home = true;
                    }
                    ui.separator();
                    if ui.button("放大").clicked() {
                        zoom_in = true;
                    }
                    if ui.button("缩小").clicked() {
                        zoom_out = true;
                    }
                });
            });
        if open_find {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.find_open = true;
                sheet.find_focus = true;
                sheet.refresh_find();
            }
        }
        if let Some(value) = copy_value {
            context.copy_text(value);
        }
        if formula_focus {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.checkpoint();
            }
        }
        if let Some(value) = changed {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.commit_edit();
                sheet.set_cell(row, column, value);
            }
        }
        if move_down {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.move_selection(1, 0);
            }
            context.memory_mut(|memory| {
                memory.request_focus(egui::Id::new("quire-sheet-grid"));
            });
        }
        if let Some(action) = action {
            self.apply_sheet_action(action);
            context.memory_mut(|memory| {
                memory.request_focus(egui::Id::new("quire-sheet-grid"));
            });
        }
        if select_all {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.commit_edit();
                let (rows, columns) = sheet.dimensions();
                sheet.range_anchor = (0, 0);
                sheet.selected = (rows.saturating_sub(1), columns.saturating_sub(1));
                sheet.scroll_to = Some((0, 0));
            }
        }
        if jump_home {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.commit_edit();
                sheet.range_anchor = (0, 0);
                sheet.selected = (0, 0);
                sheet.scroll_to = Some((0, 0));
            }
        }
        if zoom_in {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.zoom = (sheet.zoom * 1.15).min(3.0);
            }
        }
        if zoom_out {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.zoom = (sheet.zoom / 1.15).max(0.4);
            }
        }
    }

    fn apply_sheet_action(&mut self, action: SheetAction) {
        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        sheet.commit_edit();
        match action {
            SheetAction::Undo => {
                sheet.undo();
                return;
            }
            SheetAction::Redo => {
                sheet.redo();
                return;
            }
            SheetAction::EditCell => sheet.begin_edit(None),
            SheetAction::CutCell => sheet.clear_range(),
            SheetAction::ClearSelection => sheet.clear_range(),
            SheetAction::InsertRow => sheet.insert_row(),
            SheetAction::DeleteRow => sheet.delete_row(),
            SheetAction::InsertColumn => sheet.insert_column(),
            SheetAction::DeleteColumn => sheet.delete_column(),
        }
        sheet.range_anchor = sheet.selected;
        sheet.status = Some(match action {
            SheetAction::EditCell | SheetAction::Undo | SheetAction::Redo => return,
            SheetAction::CutCell => "已剪切选区".to_string(),
            SheetAction::ClearSelection => "已清空选区".to_string(),
            SheetAction::InsertRow => "已插入行".to_string(),
            SheetAction::DeleteRow => "已删除行".to_string(),
            SheetAction::InsertColumn => "已插入列".to_string(),
            SheetAction::DeleteColumn => "已删除列".to_string(),
        });
    }

    fn sheet_tabs(&mut self, context: &egui::Context) {
        let Some(sheet) = self.sheet.as_ref() else {
            return;
        };
        let mut switch = None;
        let mut add_sheet = false;
        let mut remove_sheet = false;
        let rows = sheet.active_sheet().row_count();
        let columns = sheet.active_sheet().column_count();
        let dirty = sheet.dirty;
        let status = sheet.status.clone();
        let reference = sheet.selection_reference();
        let active = sheet.active;
        egui::TopBottomPanel::bottom("sheet-tabs")
            .exact_height(34.0)
            .frame(Frame::NONE.fill(RAISED).inner_margin(Margin::symmetric(12, 4)))
            .show(context, |ui| {
            ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("工作表").size(11.0).color(MUTED));
                    for (index, item) in sheet.sheets.iter().enumerate() {
                        if ui
                            .selectable_label(active == index, &item.name)
                            .clicked()
                        {
                            switch = Some(index);
                        }
                    }
                    ui.add_space(6.0);
                    if ui.small_button("+ 新表").clicked() {
                        add_sheet = true;
                    }
                    if ui
                        .add_enabled(sheet.sheets.len() > 1, egui::Button::new("删除表").small())
                        .clicked()
                    {
                        remove_sheet = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(if dirty {
                                "未保存".to_string()
                            } else if let Some(status) = &status {
                                status.clone()
                            } else {
                                "已保存".to_string()
                            })
                            .size(11.0)
                            .color(MUTED),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{reference} · {rows}×{columns} · {:.0}%",
                                sheet.zoom * 100.0
                            ))
                            .size(11.0)
                            .color(MUTED),
                        );
                    });
                });
            });
        if let Some(index) = switch {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.commit_edit();
                sheet.active = index;
                sheet.selected = (0, 0);
                sheet.range_anchor = (0, 0);
                sheet.status = None;
                sheet.scroll_to = Some((0, 0));
                if sheet.find_open {
                    sheet.refresh_find();
                }
            }
            context.memory_mut(|memory| {
                memory.request_focus(egui::Id::new("quire-sheet-grid"));
            });
        }
        if add_sheet {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.commit_edit();
                sheet.checkpoint();
                let index = sheet.sheets.len() + 1;
                sheet.sheets.push(SheetGrid {
                    name: format!("Sheet{index}"),
                    rows: vec![Vec::new()],
                });
                sheet.active = sheet.sheets.len() - 1;
                sheet.selected = (0, 0);
                sheet.range_anchor = (0, 0);
                sheet.dirty = true;
                sheet.status = Some("已新建工作表".to_string());
                sheet.scroll_to = Some((0, 0));
                if sheet.find_open {
                    sheet.refresh_find();
                }
            }
            context.memory_mut(|memory| {
                memory.request_focus(egui::Id::new("quire-sheet-grid"));
            });
        }
        if remove_sheet {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.commit_edit();
                if sheet.sheets.len() > 1 {
                    sheet.checkpoint();
                    let index = sheet.active.min(sheet.sheets.len() - 1);
                    sheet.sheets.remove(index);
                    sheet.active = index.min(sheet.sheets.len() - 1);
                    sheet.selected = (0, 0);
                    sheet.range_anchor = (0, 0);
                    sheet.dirty = true;
                    sheet.status = Some("已删除工作表".to_string());
                    sheet.scroll_to = Some((0, 0));
                    if sheet.find_open {
                        sheet.refresh_find();
                    }
                }
            }
            context.memory_mut(|memory| {
                memory.request_focus(egui::Id::new("quire-sheet-grid"));
            });
        }
    }

    fn sheet_grid(&mut self, context: &egui::Context) {
        let grid_id = egui::Id::new("quire-sheet-grid");
        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        let (rows, columns) = sheet.dimensions();
        let mut typed = String::new();
        let mut commit_and_move: Option<(isize, isize)> = None;
        let mut lost_commit = false;
        let mut cancel_edit = false;
        let mut copy_text: Option<String> = None;

        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(SURFACE))
            .show(context, |ui| {
                let (control, zoom_delta, smooth_scroll, raw_scroll) =
                    context.input(|input| {
                        (
                            input.modifiers.command,
                            input.zoom_delta(),
                            input.smooth_scroll_delta.y,
                            input.raw_scroll_delta.y,
                        )
                    });
                let scroll = if smooth_scroll.abs() > 0.01 {
                    smooth_scroll
                } else {
                    raw_scroll
                };
                let over_grid = ui.rect_contains_pointer(ui.max_rect());
                let zoom_gesture = (zoom_delta - 1.0).abs() > 0.001;
                let factor = if over_grid && zoom_gesture {
                    zoom_delta
                } else if over_grid && control && scroll.abs() > 0.1 {
                    (1.0015_f32).powf(scroll)
                } else {
                    1.0
                };
                if (factor - 1.0).abs() > 0.0001 {
                    sheet.zoom = (sheet.zoom * factor).clamp(0.5, 2.0);
                    context.input_mut(|input| {
                        input.smooth_scroll_delta = Vec2::ZERO;
                        input.raw_scroll_delta = Vec2::ZERO;
                    });
                }
                let zoom = sheet.zoom;
                let column_width = 96.0 * zoom;
                let row_height = 24.0 * zoom;
                let header_width = 48.0 * zoom;
                let header_height = 24.0 * zoom;
                let content_width = header_width + columns as f32 * column_width;
                let content_height = header_height + rows as f32 * row_height;
                let grid_focused = context.memory(|memory| memory.has_focus(grid_id));
                if grid_focused && sheet.edit.is_none() {
                    let events = context.input(|input| input.events.clone());
                    for event in events {
                        match event {
                            egui::Event::Text(text) => typed.push_str(&text),
                            egui::Event::Paste(text) => {
                                sheet.paste_text(&text);
                            }
                            egui::Event::Copy => {
                                copy_text = Some(sheet.selection_text());
                            }
                            egui::Event::Cut => {
                                copy_text = Some(sheet.selection_text());
                                sheet.clear_range();
                            }
                            egui::Event::Key {
                                key,
                                pressed: true,
                                modifiers,
                                ..
                            } => match key {
                                egui::Key::ArrowUp => {
                                    sheet.move_selection_extended(-1, 0, modifiers.shift)
                                }
                                egui::Key::ArrowDown => {
                                    sheet.move_selection_extended(1, 0, modifiers.shift)
                                }
                                egui::Key::ArrowLeft => {
                                    sheet.move_selection_extended(0, -1, modifiers.shift)
                                }
                                egui::Key::ArrowRight => {
                                    sheet.move_selection_extended(0, 1, modifiers.shift)
                                }
                                egui::Key::Enter => sheet.move_selection(1, 0),
                                egui::Key::Tab => {
                                    if modifiers.shift {
                                        sheet.move_selection(0, -1);
                                    } else {
                                        sheet.move_selection(0, 1);
                                    }
                                }
                                egui::Key::Delete | egui::Key::Backspace => {
                                    sheet.clear_range();
                                }
                                egui::Key::F2 => sheet.begin_edit(None),
                                egui::Key::A if modifiers.command => {
                                    let rows = sheet.active_sheet().row_count().max(1);
                                    let columns =
                                        sheet.active_sheet().column_count().max(1);
                                    sheet.range_anchor = (0, 0);
                                    sheet.selected = (rows - 1, columns - 1);
                                    sheet.scroll_to = Some(sheet.selected);
                                }
                                egui::Key::Z if modifiers.command => {
                                    if modifiers.shift {
                                        sheet.redo();
                                    } else {
                                        sheet.undo();
                                    }
                                }
                                egui::Key::Y if modifiers.command => {
                                    sheet.redo();
                                }
                                egui::Key::Escape => sheet.edit = None,
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                    if !typed.is_empty() {
                        if sheet.range_bounds().0 != sheet.range_bounds().1 {
                            sheet.clear_range();
                            sheet.range_anchor = sheet.selected;
                        }
                        sheet.begin_edit(Some(typed));
                    }
                    context.input_mut(|input| {
                        for modifiers in [
                            egui::Modifiers::NONE,
                            egui::Modifiers::SHIFT,
                        ] {
                            for key in [
                                egui::Key::Tab,
                                egui::Key::Enter,
                                egui::Key::ArrowUp,
                                egui::Key::ArrowDown,
                                egui::Key::ArrowLeft,
                                egui::Key::ArrowRight,
                            ] {
                                input.consume_key(modifiers, key);
                            }
                        }
                    });
                }

                egui::ScrollArea::both()
                    .id_salt("sheet-grid-scroll")
                    .auto_shrink([false, false])
                    .drag_to_scroll(false)
                    .show_viewport(ui, |ui, viewport| {
                        ui.set_min_size(egui::vec2(content_width, content_height));
                        let origin = ui.min_rect().min;
                        let painter = ui.painter().clone();
                        let clip = ui.clip_rect();
                        let first_column = ((viewport.min.x - header_width).max(0.0)
                            / column_width)
                            .floor() as usize;
                        let last_column = (((viewport.max.x - header_width).max(0.0)
                            / column_width)
                            .ceil() as usize)
                            .min(columns.saturating_sub(1));
                        let first_row = ((viewport.min.y - header_height).max(0.0)
                            / row_height)
                            .floor() as usize;
                        let last_row = (((viewport.max.y - header_height).max(0.0)
                            / row_height)
                            .ceil() as usize)
                            .min(rows.saturating_sub(1));

                        let selected = sheet.selected;
                        let (range_start, range_end) = sheet.range_bounds();
                        for row in first_row..=last_row {
                            for column in first_column..=last_column {
                                let rect = egui::Rect::from_min_size(
                                    origin
                                        + egui::vec2(
                                            header_width + column as f32 * column_width,
                                            header_height + row as f32 * row_height,
                                        ),
                                    egui::vec2(column_width, row_height),
                                );
                                let in_range = row >= range_start.0
                                    && row <= range_end.0
                                    && column >= range_start.1
                                    && column <= range_end.1;
                                let editing_here = sheet
                                    .edit
                                    .as_ref()
                                    .map(|edit| edit.row == row && edit.column == column)
                                    .unwrap_or(false);
                                painter.rect_filled(
                                    rect,
                                    CornerRadius::ZERO,
                                    if in_range { ACCENT_SOFT } else { SURFACE },
                                );
                                if !editing_here {
                                    let value = sheet.display_cell(row, column);
                                    if !value.is_empty() {
                                        let text_painter = painter
                                            .with_clip_rect(rect.shrink(2.0).intersect(clip));
                                        let numeric = value.parse::<f64>().is_ok();
                                        if numeric {
                                            text_painter.text(
                                                rect.right_center() - egui::vec2(4.0, 0.0),
                                                Align2::RIGHT_CENTER,
                                                value,
                                                egui::FontId::proportional((12.0 * zoom).max(8.0)),
                                                INK,
                                            );
                                        } else {
                                            text_painter.text(
                                                rect.left_center() + egui::vec2(4.0, 0.0),
                                                Align2::LEFT_CENTER,
                                                value,
                                                egui::FontId::proportional((12.0 * zoom).max(8.0)),
                                                INK,
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        for column in first_column..=last_column {
                            let x = origin.x + header_width + column as f32 * column_width;
                            let rect = egui::Rect::from_min_size(
                                egui::pos2(x, origin.y),
                                egui::vec2(column_width, header_height),
                            );
                            painter.rect_filled(
                                rect,
                                CornerRadius::ZERO,
                                if column == selected.1 {
                                    ACCENT_SOFT
                                } else {
                                    RAISED
                                },
                            );
                            painter.text(
                                rect.center(),
                                Align2::CENTER_CENTER,
                                sheet_column_name(column),
                                egui::FontId::proportional((11.0 * zoom).max(8.0)),
                                if column == selected.1 { ACCENT } else { MUTED },
                            );
                        }
                        for row in first_row..=last_row {
                            let y = origin.y + header_height + row as f32 * row_height;
                            let rect = egui::Rect::from_min_size(
                                egui::pos2(origin.x, y),
                                egui::vec2(header_width, row_height),
                            );
                            painter.rect_filled(
                                rect,
                                CornerRadius::ZERO,
                                if row == selected.0 {
                                    ACCENT_SOFT
                                } else {
                                    RAISED
                                },
                            );
                            painter.text(
                                rect.center(),
                                Align2::CENTER_CENTER,
                                format!("{}", row + 1),
                                egui::FontId::proportional((11.0 * zoom).max(8.0)),
                                if row == selected.0 { ACCENT } else { MUTED },
                            );
                        }
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                origin,
                                egui::vec2(header_width, header_height),
                            ),
                            CornerRadius::ZERO,
                            RAISED,
                        );
                        painter.text(
                            origin + egui::vec2(header_width / 2.0, header_height / 2.0),
                            Align2::CENTER_CENTER,
                            "◢",
                            egui::FontId::proportional(10.0),
                            MUTED,
                        );

                        for row in first_row..=last_row + 1 {
                            let y = origin.y
                                + header_height
                                + row as f32 * row_height;
                            painter.line_segment(
                                [
                                    egui::pos2(origin.x + header_width, y),
                                    egui::pos2(origin.x + content_width, y),
                                ],
                                Stroke::new(0.5_f32, LINE),
                            );
                        }
                        for column in first_column..=last_column + 1 {
                            let x = origin.x
                                + header_width
                                + column as f32 * column_width;
                            painter.line_segment(
                                [
                                    egui::pos2(x, origin.y + header_height),
                                    egui::pos2(x, origin.y + content_height),
                                ],
                                Stroke::new(0.5_f32, LINE),
                            );
                        }

                        let selected_rect = egui::Rect::from_min_size(
                            origin
                                + egui::vec2(
                                    header_width
                                        + selected.1 as f32 * column_width,
                                    header_height
                                        + selected.0 as f32 * row_height,
                                ),
                            egui::vec2(column_width, row_height),
                        );
                        painter.rect_stroke(
                            selected_rect,
                            CornerRadius::ZERO,
                            Stroke::new(2.0_f32, ACCENT),
                            egui::StrokeKind::Inside,
                        );
                        if range_start != range_end {
                            let range_rect = egui::Rect::from_min_size(
                                origin
                                    + egui::vec2(
                                        header_width
                                            + range_start.1 as f32 * column_width,
                                        header_height
                                            + range_start.0 as f32 * row_height,
                                    ),
                                egui::vec2(
                                    (range_end.1 - range_start.1 + 1) as f32
                                        * column_width,
                                    (range_end.0 - range_start.0 + 1) as f32
                                        * row_height,
                                ),
                            );
                            painter.rect_stroke(
                                range_rect,
                                CornerRadius::ZERO,
                                Stroke::new(1.5_f32, ACCENT),
                                egui::StrokeKind::Inside,
                            );
                        }

                        let response = ui.interact(
                            ui.min_rect(),
                            grid_id,
                            egui::Sense::click_and_drag(),
                        );
                        if response.drag_started() || response.dragged() {
                            if let Some(pointer) =
                                context.input(|input| input.pointer.interact_pos())
                            {
                                let local = pointer - origin;
                                if local.x >= header_width && local.y >= header_height {
                                    let column = ((local.x - header_width) / column_width)
                                        .floor() as usize;
                                    let row = ((local.y - header_height) / row_height)
                                        .floor() as usize;
                                    if row < rows && column < columns {
                                        if response.drag_started() {
                                            if let Some(edit) = sheet.edit.take() {
                                                sheet.set_cell(
                                                    edit.row,
                                                    edit.column,
                                                    edit.text,
                                                );
                                            }
                                            sheet.set_selected(row, column, false);
                                        } else {
                                            sheet.selected = (row, column);
                                        }
                                        sheet.scroll_to = Some((row, column));
                                    }
                                }
                            }
                        }
                        if response.clicked() || response.double_clicked() {
                            response.request_focus();
                            context.memory_mut(|memory| memory.request_focus(grid_id));
                            if let Some(pointer) =
                                context.input(|input| input.pointer.interact_pos())
                            {
                                let local = pointer - origin;
                                if local.x >= header_width && local.y >= header_height {
                                    let column = ((local.x - header_width) / column_width)
                                        .floor() as usize;
                                    let row = ((local.y - header_height) / row_height)
                                        .floor() as usize;
                                    if row < rows && column < columns {
                                        if let Some(edit) = sheet.edit.take() {
                                            sheet.set_cell(
                                                edit.row,
                                                edit.column,
                                                edit.text,
                                            );
                                        }
                                        let extend = context
                                            .input(|input| input.modifiers.shift);
                                        let same_cell = sheet.selected == (row, column)
                                            && sheet.range_anchor == sheet.selected;
                                        sheet.set_selected(row, column, extend);
                                        sheet.scroll_to = None;
                                        if !extend
                                            && (response.double_clicked()
                                                || (response.clicked() && same_cell))
                                        {
                                            sheet.begin_edit(None);
                                        }
                                    }
                                }
                            }
                        }

                        if let Some(edit) = &mut sheet.edit {
                            let edit_rect = egui::Rect::from_min_size(
                                origin
                                    + egui::vec2(
                                        header_width
                                            + edit.column as f32 * column_width,
                                        header_height
                                            + edit.row as f32 * row_height,
                                    ),
                                egui::vec2(column_width, row_height),
                            );
                            let response = ui.put(
                                edit_rect,
                                egui::TextEdit::singleline(&mut edit.text)
                                    .font(egui::FontId::proportional((12.0 * zoom).max(8.0)))
                                    .margin(Margin::symmetric(3, 1)),
                            );
                            if edit.focus {
                                response.request_focus();
                                if response.has_focus() {
                                    edit.focus = false;
                                }
                            }
                            if response.lost_focus() {
                                lost_commit = true;
                            }
                            if context.input(|input| input.key_pressed(egui::Key::Escape)) {
                                cancel_edit = true;
                            }
                            if context.input(|input| input.key_pressed(egui::Key::Enter)) {
                                commit_and_move = Some((1, 0));
                            }
                            if context.input(|input| input.key_pressed(egui::Key::Tab)) {
                                commit_and_move = Some((0, 1));
                            }
                        }

                        if response.secondary_clicked() {
                            response.request_focus();
                            context.memory_mut(|memory| memory.request_focus(grid_id));
                            if let Some(pointer) =
                                context.input(|input| input.pointer.interact_pos())
                            {
                                let local = pointer - origin;
                                if local.x >= header_width && local.y >= header_height {
                                    let column = ((local.x - header_width) / column_width)
                                        .floor() as usize;
                                    let row = ((local.y - header_height) / row_height)
                                        .floor() as usize;
                                    if row < rows && column < columns {
                                        if let Some(edit) = sheet.edit.take() {
                                            sheet.set_cell(
                                                edit.row,
                                                edit.column,
                                                edit.text,
                                            );
                                        }
                                        sheet.set_selected(row, column, false);
                                        sheet.scroll_to = None;
                                    }
                                }
                            }
                        }

                        response.context_menu(|ui| {
                            if ui
                                .add_enabled(sheet.can_undo(), egui::Button::new("撤销"))
                                .clicked()
                            {
                                sheet.undo();
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(sheet.can_redo(), egui::Button::new("重做"))
                                .clicked()
                            {
                                sheet.redo();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("复制").clicked() {
                                let text = sheet.selection_text();
                                if !text.is_empty() {
                                    ui.ctx().copy_text(text);
                                }
                                ui.close_menu();
                            }
                            if ui.button("剪切").clicked() {
                                let text = sheet.selection_text();
                                if !text.is_empty() {
                                    ui.ctx().copy_text(text);
                                    sheet.clear_range();
                                }
                                ui.close_menu();
                            }
                            if ui.button("编辑").clicked() {
                                sheet.begin_edit(None);
                                ui.close_menu();
                            }
                            if ui.button("清空").clicked() {
                                sheet.clear_range();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("插入行").clicked() {
                                sheet.insert_row();
                                ui.close_menu();
                            }
                            if ui.button("删除行").clicked() {
                                sheet.delete_row();
                                ui.close_menu();
                            }
                            if ui.button("插入列").clicked() {
                                sheet.insert_column();
                                ui.close_menu();
                            }
                            if ui.button("删除列").clicked() {
                                sheet.delete_column();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("查找...").clicked() {
                                sheet.find_open = true;
                                sheet.find_focus = true;
                                sheet.refresh_find();
                                ui.close_menu();
                            }
                        });

                        if let Some((row, column)) = sheet.scroll_to.take() {
                            if row < first_row
                                || row > last_row
                                || column < first_column
                                || column > last_column
                            {
                                let rect = egui::Rect::from_min_size(
                                    origin
                                        + egui::vec2(
                                            header_width
                                                + column as f32 * column_width,
                                            header_height
                                                + row as f32 * row_height,
                                        ),
                                    egui::vec2(column_width, row_height),
                                );
                                ui.scroll_to_rect(rect, Some(egui::Align::Center));
                            }
                        }
                    });
            });

        if let Some(text) = copy_text {
            context.copy_text(text);
        }
        if cancel_edit {
            lost_commit = false;
        }
        if lost_commit {
            sheet.commit_edit();
        }
        if let Some((row_delta, column_delta)) = commit_and_move {
            sheet.commit_edit();
            sheet.move_selection(row_delta, column_delta);
            context.memory_mut(|memory| memory.request_focus(grid_id));
        }
        if cancel_edit {
            sheet.cancel_edit();
            context.memory_mut(|memory| memory.request_focus(grid_id));
        }
    }

    fn sheet_ui(&mut self, context: &egui::Context) {
        let (save, open, find, reset_zoom) = context.input(|input| {
            (
                input.modifiers.command
                    && input.key_pressed(egui::Key::S),
                input.modifiers.command
                    && input.key_pressed(egui::Key::O),
                input.modifiers.command
                    && input.key_pressed(egui::Key::F),
                input.modifiers.command
                    && input.key_pressed(egui::Key::Num0),
            )
        });
        if save {
            self.save_sheet();
        }
        if open {
            if let Some(path) = pick_document() {
                if let Err(error) = self.open_document(&path) {
                    if let Some(sheet) = &mut self.sheet {
                        sheet.status = Some(format!("打开失败: {error}"));
                    }
                }
            }
        }
        if find {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.find_open = true;
                sheet.find_focus = true;
                sheet.refresh_find();
            }
        }
        if reset_zoom {
            if let Some(sheet) = self.sheet.as_mut() {
                sheet.zoom = 1.0;
            }
        }
        if self.sheet.is_none() {
            return;
        }
        self.sheet_title_bar(context);
        self.sheet_find_bar(context);
        self.sheet_toolbar(context);
        self.sheet_tabs(context);
        self.sheet_grid(context);
    }

    fn focus_overlay(&mut self, context: &egui::Context) {
        egui::Area::new(egui::Id::new("focus-exit"))
            .anchor(Align2::RIGHT_TOP, [-16.0, 16.0])
            .order(egui::Order::Foreground)
            .show(context, |ui| {
            ui.style_mut().visuals.override_text_color = Some(INK);
                Frame::NONE
                    .fill(RAISED)
                    .stroke(Stroke::new(1.0_f32, LINE))
                    .corner_radius(12)
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        if ui.button("退出专注").clicked() {
                            self.focus_mode = false;
                        }
                    });
            });
    }

    fn clamp_pos(&self, position: DocPos) -> DocPos {
        let count = self.editor.document().editable_paragraph_count();
        if count == 0 {
            return DocPos::default();
        }
        let paragraph = position.paragraph.min(count - 1);
        let length = self.editor.paragraph_len_global(paragraph).unwrap_or(0);
        DocPos {
            paragraph,
            character: position.character.min(length),
        }
    }

    fn selection(&self) -> (DocPos, DocPos) {
        if self.anchor <= self.caret {
            (self.anchor, self.caret)
        } else {
            (self.caret, self.anchor)
        }
    }

    fn typing_selection(&self) -> (DocPos, DocPos) {
        let (start, end) = self.selection();
        if start == end {
            return (start, end);
        }
        let start_container = self
            .editor
            .document()
            .paragraph_location(start.paragraph)
            .map(|(container, _)| container);
        let end_container = self
            .editor
            .document()
            .paragraph_location(end.paragraph)
            .map(|(container, _)| container);
        if start_container != end_container {
            (self.caret, self.caret)
        } else {
            (start, end)
        }
    }

    fn relayout(&mut self) {
        if let Ok(new_layout) = layout_with_cache(
            self.editor.document(),
            &LayoutOptions::default(),
            &self.metrics,
            &mut self.layout_cache,
        ) {
            let fingerprints = page_fingerprints(&new_layout);
            let old = std::mem::replace(&mut self.page_fingerprints, fingerprints.clone());
            self.layout = new_layout;
            self.hit = HitIndex::build(&self.layout, self.editor.document());
            self.page_textures.retain(|(page, _), _| {
                fingerprints
                    .get(*page)
                    .map(|fingerprint| old.get(*page) == Some(fingerprint))
                    .unwrap_or(false)
            });
            self.thumbnails.retain(|(page, _), _| {
                fingerprints
                    .get(*page)
                    .map(|fingerprint| old.get(*page) == Some(fingerprint))
                    .unwrap_or(false)
            });
            self.page = self.page.min(self.page_count() - 1);
            self.caret = self.clamp_pos(self.caret);
            self.anchor = self.clamp_pos(self.anchor);
        }
    }

    fn after_edit(&mut self) {
        self.dirty = true;
        let previous_page = self.page;
        self.relayout();
        if let Some((page, _)) = self.hit.caret_rect(self.caret, &self.metrics) {
            if page != previous_page {
                self.page = page;
                self.pending_scroll = Some(page);
            }
        }
        if self.find_open {
            self.refresh_matches();
            self.find_selected = false;
        }
    }

    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let (start, end) = self.typing_selection();
        if start != end {
            if let Err(error) = self.editor.clear_across_global(
                start.paragraph,
                start.character,
                end.paragraph,
                end.character,
            ) {
                self.status_message = Some(error.to_string());
                return;
            }
        }
        match self
            .editor
            .insert_text_global(start.paragraph, start.character, text)
        {
            Ok(character) => {
                self.caret = DocPos {
                    paragraph: start.paragraph,
                    character,
                };
                self.anchor = self.caret;
                self.after_edit();
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn delete_selection(&mut self) -> bool {
        let (start, end) = self.selection();
        if start == end {
            return false;
        }
        if let Err(error) = self.editor.clear_across_global(
            start.paragraph,
            start.character,
            end.paragraph,
            end.character,
        ) {
            self.status_message = Some(error.to_string());
            return false;
        }
        self.caret = start;
        self.anchor = start;
        self.after_edit();
        true
    }

    fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let paragraph = self.caret.paragraph;
        let character = self.caret.character;
        if character > 0 {
            if self
                .editor
                .delete_range_global(paragraph, character - 1, character)
                .is_ok()
            {
                self.caret.character -= 1;
                self.anchor = self.caret;
                self.after_edit();
            }
        } else if paragraph > 0 {
            let previous_length = self.editor.paragraph_len_global(paragraph - 1).unwrap_or(0);
            if self
                .editor
                .delete_across_global(paragraph - 1, previous_length, paragraph, 0)
                .is_ok()
            {
                self.caret = DocPos {
                    paragraph: paragraph - 1,
                    character: previous_length,
                };
                self.anchor = self.caret;
                self.after_edit();
            }
        }
    }

    fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let paragraph = self.caret.paragraph;
        let character = self.caret.character;
        let length = self.editor.paragraph_len_global(paragraph).unwrap_or(0);
        if character < length {
            if self
                .editor
                .delete_range_global(paragraph, character, character + 1)
                .is_ok()
            {
                self.anchor = self.caret;
                self.after_edit();
            }
        } else if paragraph + 1 < self.editor.document().editable_paragraph_count()
            && self
                .editor
                .delete_across_global(paragraph, length, paragraph + 1, 0)
                .is_ok()
        {
            self.anchor = self.caret;
            self.after_edit();
        }
    }

    fn split_paragraph(&mut self) {
        self.delete_selection();
        let paragraph = self.caret.paragraph;
        let character = self.caret.character;
        match self.editor.split_global(paragraph, character) {
            Ok(index) => {
                self.caret = DocPos {
                    paragraph: index,
                    character: 0,
                };
                self.anchor = self.caret;
                self.after_edit();
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn move_horizontal(&mut self, backwards: bool, extend: bool) {
        let mut position = self.caret;
        if backwards {
            if position.character > 0 {
                position.character -= 1;
            } else if position.paragraph > 0 {
                position.paragraph -= 1;
                position.character = self
                    .editor
                    .paragraph_len_global(position.paragraph)
                    .unwrap_or(0);
            }
        } else {
            let length = self
                .editor
                .paragraph_len_global(position.paragraph)
                .unwrap_or(0);
            if position.character < length {
                position.character += 1;
            } else if position.paragraph + 1 < self.editor.document().editable_paragraph_count() {
                position.paragraph += 1;
                position.character = 0;
            }
        }
        self.caret = position;
        if !extend {
            self.anchor = position;
        }
    }

    fn move_vertical(&mut self, down: bool, extend: bool) {
        let Some(current_index) = self.hit.line_index_for(self.caret) else {
            return;
        };
        let current = &self.hit.lines[current_index];
        let x = self
            .hit
            .x_for_offset(current, self.caret.character, &self.metrics);
        let target_index = if down {
            current_index + 1
        } else {
            current_index.checked_sub(1).unwrap_or(current_index)
        };
        let Some(line) = self.hit.lines.get(target_index) else {
            return;
        };
        let character = self.hit.offset_for_x(line, x, &self.metrics);
        self.caret = DocPos {
            paragraph: line.paragraph,
            character,
        };
        if !extend {
            self.anchor = self.caret;
        }
    }

    fn move_to_edge(&mut self, end: bool, document: bool, extend: bool) {
        let position = if document {
            let count = self.editor.document().editable_paragraph_count();
            if count == 0 {
                DocPos::default()
            } else if end {
                let paragraph = count - 1;
                DocPos {
                    paragraph,
                    character: self.editor.paragraph_len_global(paragraph).unwrap_or(0),
                }
            } else {
                DocPos::default()
            }
        } else if end {
            DocPos {
                paragraph: self.caret.paragraph,
                character: self
                    .editor
                    .paragraph_len_global(self.caret.paragraph)
                    .unwrap_or(0),
            }
        } else {
            DocPos {
                paragraph: self.caret.paragraph,
                character: 0,
            }
        };
        self.caret = position;
        if !extend {
            self.anchor = position;
        }
    }

    fn undo(&mut self) {
        if self.editor.undo() {
            self.dirty = true;
            self.relayout();
            self.caret = self.clamp_pos(self.caret);
            self.anchor = self.caret;
        }
    }

    fn redo(&mut self) {
        if self.editor.redo() {
            self.dirty = true;
            self.relayout();
            self.caret = self.clamp_pos(self.caret);
            self.anchor = self.caret;
        }
    }

    fn save(&mut self) {
        if self.sheet.is_some() {
            self.save_sheet();
            return;
        }
        if self.welcome {
            return;
        }
        let path = match &self.path {
            Some(path) => path.clone(),
            None => match rfd::FileDialog::new()
                .set_title("保存文档")
                .add_filter("Word 文档", &["docx"])
                .add_filter("OpenDocument 文本", &["odt"])
                .add_filter("RTF", &["rtf"])
                .add_filter("纯文本", &["txt"])
                .set_file_name("未命名.docx")
                .save_file()
            {
                Some(path) => path,
                None => return,
            },
        };
        match write_document(&path, self.editor.document()) {
            Ok(()) => {
                self.title = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "document".to_string());
                self.path = Some(path);
                self.dirty = false;
                self.status_message = Some("已保存".to_string());
            }
            Err(error) => {
                self.status_message = Some(format!("保存失败: {error}"));
            }
        }
    }

    fn new_document(&mut self) {
        let editor = Editor::new(Document::from_text(""));
        let mut cache = LayoutCache::default();
        let Ok(layout) = layout_with_cache(
            editor.document(),
            &LayoutOptions::default(),
            &self.metrics,
            &mut cache,
        ) else {
            return;
        };
        self.layout_cache = cache;
        self.hit = HitIndex::build(&layout, editor.document());
        self.page_fingerprints = page_fingerprints(&layout);
        self.editor = editor;
        self.layout = layout;
        self.sheet = None;
        self.path = None;
        self.title = "未命名".to_string();
        self.welcome = false;
        self.page = 0;
        self.pending_scroll = Some(0);
        self.caret = DocPos::default();
        self.anchor = self.caret;
        self.dirty = false;
        self.page_textures.clear();
        self.thumbnails.clear();
        self.find_open = false;
        self.find_matches.clear();
        self.table_dialog_open = false;
        self.status_message = Some("已新建空白文档".to_string());
        self.ime_caret_screen = None;
    }

    fn selection_props(&self) -> Option<quire_model::CharProps> {
        let (start, end) = self.selection();
        if start == end {
            return None;
        }
        self.editor
            .char_props_at_global(start.paragraph, start.character)
    }

    fn selected_text(&self) -> String {
        let (start, end) = self.selection();
        if start == end {
            return String::new();
        }
        let mut out = String::new();
        for paragraph_index in start.paragraph..=end.paragraph {
            let text = self
                .editor
                .document()
                .paragraph_at_global(paragraph_index)
                .map(quire_model::Paragraph::text)
                .unwrap_or_default();
            let chars: Vec<char> = text.chars().collect();
            let from = if paragraph_index == start.paragraph {
                start.character.min(chars.len())
            } else {
                0
            };
            let to = if paragraph_index == end.paragraph {
                end.character.min(chars.len())
            } else {
                chars.len()
            };
            if from < to {
                out.extend(&chars[from..to]);
            }
            if paragraph_index < end.paragraph {
                out.push('\n');
            }
        }
        out
    }

    fn apply_char_format(&mut self, apply: impl Fn(&mut CharProps)) {
        let (start, end) = self.selection();
        if start == end {
            return;
        }
        if self
            .editor
            .format_range_global(start.paragraph,
                start.character,
                end.paragraph,
                end.character,
                apply,
            )
            .is_ok()
        {
            self.after_edit();
        }
    }

    fn toggle_bold(&mut self) {
        let bold = self
            .selection_props()
            .map(|props| props.bold)
            .unwrap_or(false);
        self.apply_char_format(move |props| props.bold = !bold);
    }

    fn toggle_italic(&mut self) {
        let italic = self
            .selection_props()
            .map(|props| props.italic)
            .unwrap_or(false);
        self.apply_char_format(move |props| props.italic = !italic);
    }

    fn toggle_underline(&mut self) {
        let underline = self
            .selection_props()
            .map(|props| props.underline)
            .unwrap_or(false);
        self.apply_char_format(move |props| props.underline = !underline);
    }

    fn set_font_size(&mut self, size: f64) {
        self.apply_char_format(move |props| props.size_pt = size);
    }

    fn apply_alignment(&mut self, align: Align) {
        let (start, end) = self.selection();
        let (first, last) = if start == end {
            (self.caret.paragraph, self.caret.paragraph)
        } else {
            (start.paragraph, end.paragraph)
        };
        if self.editor.set_alignment_global(first, last, align).is_ok() {
            self.after_edit();
        }
    }

    fn paragraph_range(&self) -> (usize, usize) {
        let (start, end) = self.selection();
        if start == end {
            (self.caret.paragraph, self.caret.paragraph)
        } else {
            (start.paragraph, end.paragraph)
        }
    }

    fn paragraph_props(&self) -> Option<ParaProps> {
        self.editor
            .paragraph_props_at_global(self.caret.paragraph)
    }

    fn apply_paragraph_format(&mut self, apply: impl Fn(&mut ParaProps)) {
        let (start, end) = self.paragraph_range();
        if self
            .editor
            .set_paragraph_format_global(start, end, apply)
            .is_ok()
        {
            self.after_edit();
        }
    }

    fn adjust_left_indent(&mut self, delta: f64) {
        let current = self
            .paragraph_props()
            .map(|props| props.indent_left_pt)
            .unwrap_or(0.0);
        let value = (current + delta).max(0.0);
        self.apply_paragraph_format(move |props| props.indent_left_pt = value);
    }

    fn adjust_first_indent(&mut self, delta: f64) {
        let current = self
            .paragraph_props()
            .map(|props| props.indent_first_pt)
            .unwrap_or(0.0);
        let value = current + delta;
        self.apply_paragraph_format(move |props| props.indent_first_pt = value);
    }

    fn adjust_right_indent(&mut self, delta: f64) {
        let current = self
            .paragraph_props()
            .map(|props| props.indent_right_pt)
            .unwrap_or(0.0);
        let value = (current + delta).max(0.0);
        self.apply_paragraph_format(move |props| props.indent_right_pt = value);
    }

    fn set_line_spacing(&mut self, value: f64) {
        self.apply_paragraph_format(move |props| props.line_spacing = value);
    }

    fn apply_style(&mut self, style_id: &str) {
        let (start, end) = self.paragraph_range();
        if self
            .editor
            .apply_paragraph_style_global(start, end, style_id)
            .is_ok()
        {
            self.after_edit();
        }
    }

    fn insert_plain_text(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if text.is_empty() {
            return;
        }
        let (start, end) = self.typing_selection();
        if start != end
            && self
                .editor
                .clear_across_global(start.paragraph, start.character, end.paragraph, end.character)
                .is_err()
        {
            self.status_message = Some("无法删除选区".to_string());
            return;
        }
        let mut paragraph = start.paragraph;
        let mut character = start.character;
        for (index, line) in text.split('\n').enumerate() {
            if index > 0 {
                match self.editor.split_global(paragraph, character) {
                    Ok(next) => {
                        paragraph = next;
                        character = 0;
                    }
                    Err(error) => {
                        self.status_message = Some(error.to_string());
                        return;
                    }
                }
            }
            if !line.is_empty() {
                match self.editor.insert_text_global(paragraph, character, line) {
                    Ok(next) => character = next,
                    Err(error) => {
                        self.status_message = Some(error.to_string());
                        return;
                    }
                }
            }
        }
        self.caret = DocPos {
            paragraph,
            character,
        };
        self.anchor = self.caret;
        self.after_edit();
    }

    fn selection_pill(&mut self, context: &egui::Context) {
        let (start, end) = self.selection();
        if start == end || !self.composition.is_empty() {
            return;
        }
        let Some(caret) = self.ime_caret_screen else {
            return;
        };
        if !context.memory(|memory| memory.has_focus(self.canvas_id)) {
            return;
        }
        let position = egui::pos2(
            caret.center().x - 80.0,
            (caret.top() - 46.0).max(8.0),
        );
        let mut clicked = false;
        egui::Area::new(egui::Id::new("selection-pill"))
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .show(context, |ui| {
            ui.style_mut().visuals.override_text_color = Some(INK);
                Frame::NONE
                    .fill(RAISED)
                    .stroke(Stroke::new(1.0_f32, LINE))
                    .corner_radius(18)
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let bold = self
                                .selection_props()
                                .map(|props| props.bold)
                                .unwrap_or(false);
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("B").strong())
                                        .selected(bold)
                                        .frame(false),
                                )
                                .clicked()
                            {
                                self.toggle_bold();
                                clicked = true;
                            }
                            let italic = self
                                .selection_props()
                                .map(|props| props.italic)
                                .unwrap_or(false);
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("I").italics())
                                        .selected(italic)
                                        .frame(false),
                                )
                                .clicked()
                            {
                                self.toggle_italic();
                                clicked = true;
                            }
                            let underline = self
                                .selection_props()
                                .map(|props| props.underline)
                                .unwrap_or(false);
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("U").underline())
                                        .selected(underline)
                                        .frame(false),
                                )
                                .clicked()
                            {
                                self.toggle_underline();
                                clicked = true;
                            }
                        });
                    });
            });
        if clicked {
            context.memory_mut(|memory| memory.request_focus(self.canvas_id));
        }
    }

    fn refresh_matches(&mut self) {
        self.find_matches = if self.find_open && !self.find_query.is_empty() {
            self.editor.find_all_global(&self.find_query, self.find_case)
        } else {
            Vec::new()
        };
        if self.find_index >= self.find_matches.len() {
            self.find_index = 0;
        }
    }

    fn select_match(&mut self, index: usize) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_index = index % self.find_matches.len();
        let target = self.find_matches[self.find_index];
        self.anchor = DocPos {
            paragraph: target.paragraph,
            character: target.start,
        };
        self.caret = DocPos {
            paragraph: target.paragraph,
            character: target.end,
        };
        if let Some((page, _)) = self.hit.caret_rect(self.caret, &self.metrics) {
            self.page = page;
            self.pending_scroll = Some(page);
        }
        self.find_selected = true;
    }

    fn find_next(&mut self, forward: bool) {
        self.refresh_matches();
        if self.find_matches.is_empty() {
            self.find_selected = false;
            return;
        }
        let index = if !self.find_selected {
            if forward {
                0
            } else {
                self.find_matches.len() - 1
            }
        } else if forward {
            (self.find_index + 1) % self.find_matches.len()
        } else {
            (self.find_index + self.find_matches.len() - 1) % self.find_matches.len()
        };
        self.select_match(index);
    }

    fn replace_current(&mut self) {
        self.refresh_matches();
        if self.find_matches.is_empty() {
            return;
        }
        let index = self.find_index.min(self.find_matches.len() - 1);
        let target = self.find_matches[index];
        let replacement = self.replace_query.clone();
        match self.editor.replace_match_global(target, &replacement) {
            Ok(()) => {
                self.after_edit();
                self.status_message = Some("已替换 1 处".to_string());
                if !self.find_matches.is_empty() {
                    self.select_match(index.min(self.find_matches.len() - 1));
                }
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn replace_all_matches(&mut self) {
        let query = self.find_query.clone();
        let replacement = self.replace_query.clone();
        match self
            .editor
            .replace_all_global(&query, &replacement, self.find_case)
        {
            Ok(0) => self.status_message = Some("没有匹配项".to_string()),
            Ok(count) => {
                self.after_edit();
                self.find_selected = false;
                self.status_message = Some(format!("已替换 {count} 处"));
            }
            Err(error) => self.status_message = Some(error.to_string()),
        }
    }

    fn find_bar(&mut self, context: &egui::Context) {
        if !self.find_open {
            return;
        }
        egui::TopBottomPanel::top("find")
            .exact_height(44.0)
            .frame(Frame::NONE.fill(RAISED).inner_margin(Margin::symmetric(16, 8)))
            .show(context, |ui| {
            ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.find_query)
                            .hint_text("查找")
                            .desired_width(180.0),
                    );
                    if self.find_focus {
                        response.request_focus();
                        self.find_focus = false;
                    }
                    if response.changed() {
                        self.find_index = 0;
                        self.find_selected = false;
                        self.refresh_matches();
                    }
                    if response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        let backwards = ui.input(|input| input.modifiers.shift);
                        self.find_next(!backwards);
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.replace_query)
                            .hint_text("替换为")
                            .desired_width(180.0),
                    );
                    if ui.button("上一个").clicked() {
                        self.find_next(false);
                    }
                    if ui.button("下一个").clicked() {
                        self.find_next(true);
                    }
                    if ui.button("替换").clicked() {
                        self.replace_current();
                    }
                    if ui.button("全部替换").clicked() {
                        self.replace_all_matches();
                    }
                    if ui.checkbox(&mut self.find_case, "区分大小写").changed() {
                        self.find_index = 0;
                        self.find_selected = false;
                        self.refresh_matches();
                    }
                    let count = if self.find_matches.is_empty() {
                        "0".to_string()
                    } else {
                        format!("{} / {}", self.find_index + 1, self.find_matches.len())
                    };
                    ui.label(RichText::new(count).size(12.0).color(MUTED));
                    if ui.button("关闭").clicked() {
                        self.find_open = false;
                        self.find_matches.clear();
                    }
                });
            });
    }

    fn editing(&mut self, context: &egui::Context) {
        if !context.memory(|memory| memory.has_focus(self.canvas_id)) {
            self.composition.clear();
            self.ime_caret_screen = None;
            return;
        }

        let events = context.input(|input| input.events.clone());
        for event in events {
            match event {
                egui::Event::Text(text) => {
                    if self.composition.is_empty() {
                        self.insert_text(&text);
                    }
                }
                egui::Event::Paste(text) => self.insert_plain_text(&text),
                egui::Event::Copy => {
                    let text = self.selected_text();
                    if !text.is_empty() {
                        context.copy_text(text);
                    }
                }
                egui::Event::Cut => {
                    let text = self.selected_text();
                    if !text.is_empty() {
                        context.copy_text(text);
                        self.delete_selection();
                    }
                }
                egui::Event::Ime(ime) => match ime {
                    egui::ImeEvent::Preedit(text) => self.composition = text,
                    egui::ImeEvent::Commit(text) => {
                        self.composition.clear();
                        self.insert_text(&text);
                    }
                    egui::ImeEvent::Enabled => {}
                    egui::ImeEvent::Disabled => self.composition.clear(),
                },
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let extend = modifiers.shift;
                    let command = modifiers.command;
                    match key {
                        egui::Key::Backspace => self.backspace(),
                        egui::Key::Delete => self.delete_forward(),
                        egui::Key::Enter => self.split_paragraph(),
                        egui::Key::Tab => {
                            if self.table_context().is_some() {
                                self.move_table_cell(extend);
                            } else {
                                self.insert_text("\t");
                            }
                        }
                        egui::Key::ArrowLeft => self.move_horizontal(true, extend),
                        egui::Key::ArrowRight => self.move_horizontal(false, extend),
                        egui::Key::ArrowUp => self.move_vertical(false, extend),
                        egui::Key::ArrowDown => self.move_vertical(true, extend),
                        egui::Key::Home => self.move_to_edge(false, command, extend),
                        egui::Key::End => self.move_to_edge(true, command, extend),
                        egui::Key::PageUp => {
                            self.previous_page();
                        }
                        egui::Key::PageDown => {
                            self.next_page();
                        }
                        egui::Key::Escape => {
                            self.anchor = self.caret;
                        }
                        egui::Key::A if command => {
                            let count = self.editor.document().editable_paragraph_count();
                            self.anchor = DocPos::default();
                            if count > 0 {
                                let paragraph = count - 1;
                                self.caret = DocPos {
                                    paragraph,
                                    character: self
                                        .editor
                                        .paragraph_len_global(paragraph)
                                        .unwrap_or(0),
                                };
                            }
                        }
                        egui::Key::Z if command => self.undo(),
                        egui::Key::Y if command => self.redo(),
                        egui::Key::B if command => self.toggle_bold(),
                        egui::Key::I if command => self.toggle_italic(),
                        egui::Key::U if command => self.toggle_underline(),
                        egui::Key::S if command => self.save(),
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        if let Some(rect) = self.ime_caret_screen {
            context.output_mut(|output| {
                output.ime = Some(egui::output::IMEOutput {
                    rect,
                    cursor_rect: rect,
                });
            });
        }
        context.request_repaint();
    }

    fn go_to(&mut self, page: usize) {
        self.page = page.min(self.page_count() - 1);
        self.pending_scroll = Some(self.page);
    }

    fn next_page(&mut self) {
        self.go_to(self.page + 1);
    }

    fn previous_page(&mut self) {
        self.go_to(self.page.saturating_sub(1));
    }

    fn fit_zoom(&self, available_width: f32) -> f32 {
        let page = &self.layout.pages[self.page.min(self.page_count() - 1)];
        ((available_width - 96.0) / page.width_pt as f32).clamp(0.2, 3.0)
    }

    fn page_texture(
        &mut self,
        context: &egui::Context,
        index: usize,
        scale: f32,
    ) -> Option<TextureHandle> {
        let key = (index, (scale * 100.0).round() as u32);
        self.cache_tick += 1;
        let tick = self.cache_tick;
        if let Some((texture, used)) = self.page_textures.get_mut(&key) {
            *used = tick;
            return Some(texture.clone());
        }
        let options = SvgOptions {
            fit_text_to_layout: !self.system_fonts,
            draw_canvas_background: false,
            viewport_margin_pt: 0.0,
            page_gap_pt: 0.0,
            ..SvgOptions::default()
        };
        let svg = render_svg_page(&self.layout, index, &options)?;
        let raster = self.rasterizer.rasterize(&svg, scale).ok()?;
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [raster.width as usize, raster.height as usize],
            &raster.rgba,
        );
        let texture = context.load_texture(
            format!("page-{index}-{}", key.1),
            image,
            TextureOptions::LINEAR,
        );
        self.page_textures.insert(key, (texture.clone(), tick));
        evict(&mut self.page_textures, 16, tick);
        Some(texture)
    }

    fn thumbnail(
        &mut self,
        context: &egui::Context,
        index: usize,
        scale: f32,
    ) -> Option<TextureHandle> {
        let key = (index, (scale * 100.0).round() as u32);
        self.cache_tick += 1;
        let tick = self.cache_tick;
        if let Some((texture, used)) = self.thumbnails.get_mut(&key) {
            *used = tick;
            return Some(texture.clone());
        }
        let options = SvgOptions {
            fit_text_to_layout: !self.system_fonts,
            draw_canvas_background: false,
            viewport_margin_pt: 0.0,
            page_gap_pt: 0.0,
            ..SvgOptions::default()
        };
        let svg = render_svg_page(&self.layout, index, &options)?;
        let raster = self.rasterizer.rasterize(&svg, scale).ok()?;
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [raster.width as usize, raster.height as usize],
            &raster.rgba,
        );
        let texture = context.load_texture(
            format!("thumb-{index}-{}", key.1),
            image,
            TextureOptions::LINEAR,
        );
        self.thumbnails.insert(key, (texture.clone(), tick));
        evict(&mut self.thumbnails, 80, tick);
        Some(texture)
    }

    fn keyboard(&mut self, context: &egui::Context) {
        let editing = context.memory(|memory| memory.has_focus(self.canvas_id));
        let palette = context.input(|input| {
            input.modifiers.command && input.key_pressed(egui::Key::K)
        });
        if palette {
            self.palette_open = !self.palette_open;
            self.palette_focus = self.palette_open;
            self.palette_query.clear();
        }
        let find = context.input(|input| input.modifiers.command && input.key_pressed(egui::Key::F));
        if find {
            self.find_open = !self.find_open;
            self.find_focus = self.find_open;
            self.find_index = 0;
            self.find_selected = false;
            if self.find_open {
                self.refresh_matches();
            } else {
                self.find_matches.clear();
            }
        }
        if self.find_open
            && context.input(|input| input.key_pressed(egui::Key::Escape))
        {
            self.find_open = false;
            self.find_matches.clear();
            context.memory_mut(|memory| memory.request_focus(self.canvas_id));
        }
        let open = context.input(|input| input.modifiers.command && input.key_pressed(egui::Key::O));
        if open {
            if let Some(path) = pick_document() {
                if let Err(error) = self.open_document(&path) {
                    self.status_message = Some(format!("打开失败: {error}"));
                }
            }
        }
        let new = context.input(|input| input.modifiers.command && input.key_pressed(egui::Key::N));
        if new {
            self.new_document();
        }
        if context.input(|input| input.key_pressed(egui::Key::F11)) {
            self.focus_mode = !self.focus_mode;
        }
        if context.input(|input| {
            input.modifiers.command && input.modifiers.shift && input.key_pressed(egui::Key::L)
        }) {
            self.show_rail = !self.show_rail;
        }
        if context.input(|input| {
            input.modifiers.command && input.modifiers.shift && input.key_pressed(egui::Key::R)
        }) {
            self.show_inspector = !self.show_inspector;
        }
        if self.focus_mode
            && context.input(|input| input.key_pressed(egui::Key::Escape))
        {
            self.focus_mode = false;
            return;
        }
        if editing {
            return;
        }

        let (next, previous, home, end, zoom_in, zoom_out, fit) = context.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowRight)
                    || input.key_pressed(egui::Key::PageDown)
                    || input.key_pressed(egui::Key::Space),
                input.key_pressed(egui::Key::ArrowLeft) || input.key_pressed(egui::Key::PageUp),
                input.key_pressed(egui::Key::Home),
                input.key_pressed(egui::Key::End),
                input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals),
                input.key_pressed(egui::Key::Minus),
                input.key_pressed(egui::Key::Num0),
            )
        });
        if next {
            self.next_page();
        }
        if previous {
            self.previous_page();
        }
        if home {
            self.go_to(0);
        }
        if end {
            self.go_to(self.page_count() - 1);
        }
        if zoom_in {
            let current = self.current_zoom();
            self.fit = false;
            self.zoom = (current * 1.15).min(4.0);
        }
        if zoom_out {
            let current = self.current_zoom();
            self.fit = false;
            self.zoom = (current / 1.15).max(0.2);
        }
        if fit {
            self.fit = true;
        }
    }

    fn title_bar(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::top("title")
            .exact_height(44.0)
            .frame(Frame::NONE.fill(RAISED).inner_margin(Margin::symmetric(16, 8)))
            .show(context, |ui| {
            ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("Q").size(22.0).strong().color(ACCENT));
                    ui.add_space(4.0);
                    ui.label(RichText::new(&self.title).size(14.0).strong());
                    ui.label(
                        RichText::new(if self.dirty { "· 已修改" } else { "· 已保存" })
                            .size(12.0)
                            .color(if self.dirty { ACCENT } else { MUTED }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new("左栏").selected(self.show_rail))
                            .clicked()
                        {
                            self.show_rail = !self.show_rail;
                        }
                        if ui
                            .add(egui::Button::new("右栏").selected(self.show_inspector))
                            .clicked()
                        {
                            self.show_inspector = !self.show_inspector;
                        }
                        if ui.button("专注").clicked() {
                            self.focus_mode = true;
                        }
                        if ui.button("打开").clicked() {
                            if let Some(path) = pick_document() {
                                if let Err(error) = self.open_document(&path) {
                                    self.status_message = Some(format!("打开失败: {error}"));
                                }
                            }
                        }
                        if !self.welcome && ui.button("查找").clicked() {
                            self.find_open = true;
                            self.find_focus = true;
                            self.refresh_matches();
                        }
                        if !self.welcome && ui.button("保存").clicked() {
                            self.save();
                        }
                        if ui.button("深色 / 浅色").clicked() {
                            self.dark = !self.dark;
                            self.apply_theme(context);
                        }
                        if ui
                            .button(RichText::new("命令  Ctrl+K").size(12.0))
                            .clicked()
                        {
                            self.palette_open = true;
                        }
                    });
                });
            });
    }

    fn deck(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::top("deck")
            .exact_height(78.0)
            .frame(Frame::NONE.fill(SURFACE).inner_margin(Margin::symmetric(16, 8)))
            .show(context, |ui| {
            ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal(|ui| {
                    for (deck, label) in [
                        (Deck::Start, "开始"),
                        (Deck::Insert, "插入"),
                        (Deck::Layout, "布局"),
                        (Deck::Reference, "引用"),
                        (Deck::Review, "审阅"),
                        (Deck::View, "视图"),
                    ] {
                        let selected = self.deck == deck;
                        if ui.selectable_label(selected, label).clicked() {
                            self.deck = deck;
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("quire")
                            .size(11.0)
                            .monospace()
                            .color(MUTED),
                    );
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    self.deck_controls(ui);
                });
            });
    }

    fn deck_controls(&mut self, ui: &mut egui::Ui) {
        match self.deck {
            Deck::Start => {
                let can_undo = self.editor.can_undo();
                let can_redo = self.editor.can_redo();
                if ui
                    .add_enabled(can_undo, egui::Button::new("撤销"))
                    .clicked()
                {
                    self.undo();
                }
                if ui
                    .add_enabled(can_redo, egui::Button::new("重做"))
                    .clicked()
                {
                    self.redo();
                }
                ui.separator();
                let selected = self.selection().0 != self.selection().1;
                let props = self.selection_props();
                let bold = props.as_ref().map(|props| props.bold).unwrap_or(false);
                let italic = props.as_ref().map(|props| props.italic).unwrap_or(false);
                let underline = props.as_ref().map(|props| props.underline).unwrap_or(false);
                if ui
                    .add_enabled(
                        selected,
                        egui::Button::new(RichText::new("B").strong())
                            .selected(bold)
                            .min_size(Vec2::new(30.0, 28.0)),
                    )
                    .clicked()
                {
                    self.toggle_bold();
                }
                if ui
                    .add_enabled(
                        selected,
                        egui::Button::new(RichText::new("I").italics())
                            .selected(italic)
                            .min_size(Vec2::new(30.0, 28.0)),
                    )
                    .clicked()
                {
                    self.toggle_italic();
                }
                if ui
                    .add_enabled(
                        selected,
                        egui::Button::new(RichText::new("U").underline())
                            .selected(underline)
                            .min_size(Vec2::new(30.0, 28.0)),
                    )
                    .clicked()
                {
                    self.toggle_underline();
                }
                let current_size = props.as_ref().map(|props| props.size_pt).unwrap_or(10.5);
                egui::ComboBox::from_id_salt("font-size")
                    .width(76.0)
                    .selected_text(format!("{current_size:.1}"))
                    .show_ui(ui, |ui| {
                        for candidate in [9.0_f64, 10.5, 11.0, 12.0, 14.0, 16.0, 18.0, 24.0] {
                            if ui
                                .selectable_label(
                                    (candidate - current_size).abs() < 0.01,
                                    format!("{candidate:.1}"),
                                )
                                .clicked()
                            {
                                self.set_font_size(candidate);
                            }
                        }
                    });
                if ui
                    .add_enabled(selected, egui::Button::new("A−").min_size(Vec2::new(30.0, 28.0)))
                    .clicked()
                {
                    self.set_font_size((current_size - 1.0).max(4.0));
                }
                if ui
                    .add_enabled(selected, egui::Button::new("A+").min_size(Vec2::new(30.0, 28.0)))
                    .clicked()
                {
                    self.set_font_size((current_size + 1.0).min(96.0));
                }
                if ui
                    .add_enabled(selected, egui::Button::new("清除格式"))
                    .clicked()
                {
                    self.apply_char_format(|props| *props = CharProps::default());
                }
                ui.separator();
                let align = self.paragraph_props().map(|props| props.align).unwrap_or_default();
                for (label, candidate) in [
                    ("左", Align::Left),
                    ("中", Align::Center),
                    ("右", Align::Right),
                    ("两端", Align::Justify),
                ] {
                    if ui.selectable_label(align == candidate, label).clicked() {
                        self.apply_alignment(candidate);
                    }
                }
                ui.separator();
                let spacing = self
                    .paragraph_props()
                    .map(|props| props.line_spacing)
                    .unwrap_or(1.0);
                egui::ComboBox::from_id_salt("line-spacing")
                    .width(84.0)
                    .selected_text(format!("行距 {spacing:.2}"))
                    .show_ui(ui, |ui| {
                        for candidate in [1.0_f64, 1.15, 1.5, 2.0, 2.5, 3.0] {
                            if ui
                                .selectable_label(
                                    (spacing - candidate).abs() < 0.01,
                                    format!("{candidate:.2}"),
                                )
                                .clicked()
                            {
                                self.set_line_spacing(candidate);
                            }
                        }
                    });
                let styles: Vec<(String, String)> = self
                    .editor
                    .document()
                    .styles()
                    .iter()
                    .take(24)
                    .map(|style| (style.id.clone(), style.name.clone()))
                    .collect();
                let current_style = self
                    .editor
                    .document()
                    .paragraph_at_global(self.caret.paragraph)
                    .and_then(|paragraph| paragraph.style.clone());
                if styles.is_empty() {
                    ui.label(RichText::new("无命名样式").size(12.0).color(MUTED));
                } else {
                    let current_label = current_style
                        .as_ref()
                        .and_then(|id| {
                            styles
                                .iter()
                                .find(|(style_id, _)| style_id == id)
                                .map(|(_, name)| name.clone())
                        })
                        .unwrap_or_else(|| "正文".to_string());
                    egui::ComboBox::from_id_salt("paragraph-style")
                        .width(104.0)
                        .selected_text(current_label)
                        .show_ui(ui, |ui| {
                            for (id, name) in &styles {
                                let selected = current_style.as_deref() == Some(id.as_str());
                                if ui.selectable_label(selected, name).clicked() {
                                    self.apply_style(id);
                                }
                            }
                        });
                }
            }
            Deck::Insert => {
                if ui.button("图片...").clicked() {
                    self.insert_image_dialog();
                }
                if ui.button("表格...").clicked() {
                    self.table_dialog_open = true;
                }
                if self.table_context().is_some() {
                    ui.separator();
                    if ui.small_button("上+行").clicked() {
                        self.add_table_row_above();
                    }
                    if ui.small_button("下+行").clicked() {
                        self.add_table_row();
                    }
                    if ui.small_button("左+列").clicked() {
                        self.add_table_column_left();
                    }
                    if ui.small_button("右+列").clicked() {
                        self.add_table_column();
                    }
                    if ui.small_button("删行").clicked() {
                        self.remove_table_row();
                    }
                    if ui.small_button("删列").clicked() {
                        self.remove_table_column();
                    }
                    if ui.small_button("表头").clicked() {
                        self.toggle_table_header();
                    }
                    if ui.small_button("删表").clicked() {
                        self.delete_current_table();
                    }
                }
            }
            Deck::Layout => {
                if ui.button("左缩进 +18").clicked() {
                    self.adjust_left_indent(18.0);
                }
                if ui.button("左缩进 −18").clicked() {
                    self.adjust_left_indent(-18.0);
                }
                if ui.button("右缩进 +18").clicked() {
                    self.adjust_right_indent(18.0);
                }
                if ui.button("右缩进 −18").clicked() {
                    self.adjust_right_indent(-18.0);
                }
                ui.separator();
                if ui.button("首行 +18").clicked() {
                    self.adjust_first_indent(18.0);
                }
                if ui.button("首行 −18").clicked() {
                    self.adjust_first_indent(-18.0);
                }
                if ui.button("取消首行").clicked() {
                    self.apply_paragraph_format(|props| props.indent_first_pt = 0.0);
                }
                ui.separator();
                let spacing = self
                    .paragraph_props()
                    .map(|props| props.line_spacing)
                    .unwrap_or(1.0);
                for candidate in [1.0_f64, 1.15, 1.5, 2.0] {
                    if ui
                        .selectable_label(
                            (spacing - candidate).abs() < 0.01,
                            format!("{candidate:.2}"),
                        )
                        .clicked()
                    {
                        self.set_line_spacing(candidate);
                    }
                }
                ui.separator();
                if ui.button("段前 6").clicked() {
                    self.apply_paragraph_format(|props| props.space_before_pt = 6.0);
                }
                if ui.button("段后 6").clicked() {
                    self.apply_paragraph_format(|props| props.space_after_pt = 6.0);
                }
                if ui.button("段前 0").clicked() {
                    self.apply_paragraph_format(|props| props.space_before_pt = 0.0);
                }
                if ui.button("段后 0").clicked() {
                    self.apply_paragraph_format(|props| props.space_after_pt = 0.0);
                }
                ui.separator();
                if ui.button("清除段落格式").clicked() {
                    self.apply_paragraph_format(|props| *props = ParaProps::default());
                }
            }
            Deck::Reference => {
                if ui.button("回到文首").clicked() {
                    self.move_to_edge(false, true, false);
                }
                if ui.button("回到文末").clicked() {
                    self.move_to_edge(true, true, false);
                }
                ui.separator();
                if ui.button("查找…").clicked() {
                    self.find_open = true;
                    self.find_focus = true;
                    self.refresh_matches();
                }
                ui.separator();
                ui.label(
                    RichText::new("目录、题注、交叉引用尚未实现")
                        .size(12.0)
                        .color(MUTED),
                );
            }
            Deck::Review => {
                ui.label(
                    RichText::new("批注、修订、比较尚未实现")
                        .size(12.0)
                        .color(MUTED),
                );
            }
            Deck::View => {
                if ui.selectable_label(self.fit, "适宽").clicked() {
                    self.fit = true;
                }
                for candidate in [0.75_f32, 1.0, 1.25, 1.5] {
                    let selected = !self.fit && (self.zoom - candidate).abs() < 0.01;
                    if ui
                        .selectable_label(selected, format!("{:.0}%", candidate * 100.0))
                        .clicked()
                    {
                        self.fit = false;
                        self.zoom = candidate;
                    }
                }
                ui.separator();
                if ui.button("上一页").clicked() {
                    self.previous_page();
                }
                if ui.button("下一页").clicked() {
                    self.next_page();
                }
                ui.separator();
                ui.label(
                    RichText::new(format!("第 {} / {} 页", self.page + 1, self.page_count()))
                        .size(12.0)
                        .color(MUTED),
                );
                ui.label(
                    RichText::new(format!(
                        "{} 段 · {} 字",
                        self.editor.document().total_paragraph_count(),
                        self.editor.document().char_count()
                    ))
                    .size(12.0)
                    .color(MUTED),
                );
            }
        }
    }

    fn rail(&mut self, context: &egui::Context) {
        egui::SidePanel::left("rail")
            .exact_width(220.0)
            .resizable(false)
            .frame(Frame::NONE.fill(RAISED).inner_margin(Margin::symmetric(12, 12)))
            .show(context, |ui| {
            ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("页面").size(12.0).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            self.show_rail = false;
                        }
                    });
                });
                ui.add_space(4.0);
                let rail_width = ui.available_width();
                let thumb_width = 120.0_f32;
                let row_height = 190.0_f32;
                let pixels_per_point = context.pixels_per_point();

                egui::ScrollArea::vertical()
                    .id_salt("pages")
                    .max_height(420.0)
                    .show_viewport(ui, |ui, viewport| {
                        let total = self.page_count() as f32 * row_height;
                        ui.set_min_size(Vec2::new(rail_width, total));
                        let origin = ui.min_rect().min;
                        let painter = ui.painter().clone();

                        for index in 0..self.page_count() {
                            let content = egui::Rect::from_min_size(
                                egui::pos2(0.0, index as f32 * row_height),
                                Vec2::new(rail_width, row_height),
                            );
                            if !content.intersects(viewport) {
                                continue;
                            }
                            let screen = content.translate(origin.to_vec2());
                            if self.page == index {
                                painter.rect_filled(
                                    screen.shrink(2.0),
                                    CornerRadius::same(8),
                                    ACCENT_SOFT,
                                );
                            }

                            let page = &self.layout.pages[index];
                            let page_width = page.width_pt as f32;
                            let page_height = page.height_pt as f32;
                            let ratio = (page_height / page_width).max(0.1);
                            let display_height = (thumb_width * ratio).min(row_height - 34.0);
                            let display_width = display_height / ratio;
                            let thumb_rect = egui::Rect::from_min_size(
                                screen.center_top()
                                    + Vec2::new(-display_width / 2.0, 10.0),
                                Vec2::new(display_width, display_height),
                            );
                            let scale = (display_width / page_width) * pixels_per_point;

                            match self.thumbnail(context, index, scale) {
                                Some(texture) => {
                                    painter.image(
                                        texture.id(),
                                        thumb_rect,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        Color32::WHITE,
                                    );
                                }
                                None => {
                                    painter.rect_filled(
                                        thumb_rect,
                                        CornerRadius::ZERO,
                                        SURFACE,
                                    );
                                }
                            }
                            painter.rect_stroke(
                                thumb_rect,
                                CornerRadius::ZERO,
                                Stroke::new(1.0_f32, LINE),
                                egui::StrokeKind::Inside,
                            );
                            painter.text(
                                screen.center_bottom() + Vec2::new(0.0, -8.0),
                                Align2::CENTER_BOTTOM,
                                format!("第 {} 页", index + 1),
                                egui::FontId::proportional(11.0),
                                if self.page == index { ACCENT } else { MUTED },
                            );
                            let response = ui.interact(
                                screen,
                                egui::Id::new(("thumb", index)),
                                egui::Sense::click(),
                            );
                            if response.clicked() {
                                self.go_to(index);
                            }
                        }
                    });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(RichText::new("大纲").size(12.0).color(MUTED));
                egui::ScrollArea::vertical()
                    .id_salt("outline")
                    .show(ui, |ui| {
                        let mut shown = 0;
                        for paragraph in self.editor.document().paragraphs() {
                            let text = paragraph.text();
                            let text = text.trim();
                            if text.is_empty() {
                                continue;
                            }
                            let short: String = text.chars().take(28).collect();
                            ui.label(RichText::new(short).size(12.0));
                            shown += 1;
                            if shown >= 40 {
                                break;
                            }
                        }
                    });
            });
    }

    fn inspector(&mut self, context: &egui::Context) {
        egui::SidePanel::right("inspector")
            .exact_width(280.0)
            .resizable(false)
            .frame(Frame::NONE.fill(SURFACE).inner_margin(Margin::symmetric(14, 12)))
            .show(context, |ui| {
            ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal(|ui| {
                    if ui.small_button("×").clicked() {
                        self.show_inspector = false;
                    }
                    for (tab, label) in [
                        (Inspector::Styles, "样式"),
                        (Inspector::Format, "格式"),
                        (Inspector::Outline, "大纲"),
                        (Inspector::Comments, "批注"),
                    ] {
                        if ui.selectable_label(self.inspector == tab, label).clicked() {
                            self.inspector = tab;
                        }
                    }
                });
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);
                match self.inspector {
                    Inspector::Styles => {
                        ui.label(RichText::new("样式画廊").size(13.0).strong());
                        ui.add_space(6.0);
                        let current_style = self
                            .editor
                            .document()
                            .paragraph_at_global(self.caret.paragraph)
                            .and_then(|paragraph| paragraph.style.clone());
                        let styles: Vec<(String, String)> = self
                            .editor
                            .document()
                            .styles()
                            .iter()
                            .take(24)
                            .map(|style| (style.id.clone(), style.name.clone()))
                            .collect();
                        if styles.is_empty() {
                            ui.label(RichText::new("文档未定义命名样式").size(12.0).color(MUTED));
                        } else {
                            for (id, name) in styles {
                                let selected = current_style.as_deref() == Some(id.as_str());
                                if ui.selectable_label(selected, name).clicked() {
                                    self.apply_style(&id);
                                }
                            }
                        }
                    }
                    Inspector::Format => {
                        let props = self.paragraph_props().unwrap_or_default();
                        ui.label(RichText::new("段落").size(13.0).strong());
                        ui.horizontal(|ui| {
                            if ui.button("左缩进 +18").clicked() {
                                self.adjust_left_indent(18.0);
                            }
                            if ui.button("左缩进 −18").clicked() {
                                self.adjust_left_indent(-18.0);
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button("首行 +18").clicked() {
                                self.adjust_first_indent(18.0);
                            }
                            if ui.button("首行 −18").clicked() {
                                self.adjust_first_indent(-18.0);
                            }
                        });
                        ui.horizontal(|ui| {
                            for spacing in [1.0_f64, 1.15, 1.5, 2.0] {
                                if ui
                                    .selectable_label(
                                        (props.line_spacing - spacing).abs() < 0.01,
                                        format!("{spacing:.2}"),
                                    )
                                    .clicked()
                                {
                                    self.set_line_spacing(spacing);
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui.button("段前 6").clicked() {
                                self.apply_paragraph_format(|props| props.space_before_pt = 6.0);
                            }
                            if ui.button("段后 6").clicked() {
                                self.apply_paragraph_format(|props| props.space_after_pt = 6.0);
                            }
                        });
                        ui.add_space(8.0);
                        let section = self.editor.document().first_section();
                        ui.label(RichText::new("页面").size(13.0).strong());
                        ui.label(format!(
                            "A4 · {:.0} × {:.0} pt",
                            section.props.page.width_pt, section.props.page.height_pt
                        ));
                        ui.label(format!(
                            "边距 上 {:.0} / 下 {:.0} / 左 {:.0} / 右 {:.0}",
                            section.props.margins.top_pt,
                            section.props.margins.bottom_pt,
                            section.props.margins.left_pt,
                            section.props.margins.right_pt
                        ));
                        ui.add_space(10.0);
                        ui.label(RichText::new("文档").size(13.0).strong());
                        ui.label(format!("段落 {}", self.editor.document().total_paragraph_count()));
                        ui.label(format!("字符 {}", self.editor.document().char_count()));
                        ui.label(format!("表 {}", table_count(self.editor.document())));
                        ui.label(format!("编号列表 {}", self.editor.document().numbering().instances.len()));
                        ui.add_space(10.0);
                        ui.label(RichText::new("保真").size(13.0).strong());
                        if self.editor.document().fidelity().is_lossless() {
                            ui.label(RichText::new("lossless").size(12.0).color(MUTED));
                        } else {
                            ui.label(
                                RichText::new(self.editor.document().fidelity().summary())
                                    .size(12.0)
                                    .color(ACCENT),
                            );
                        }
                    }
                    Inspector::Outline => {
                        ui.label(RichText::new("段落").size(13.0).strong());
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let mut shown = 0;
                            for paragraph in self.editor.document().paragraphs() {
                                let text = paragraph.text();
                                let text = text.trim();
                                if text.is_empty() {
                                    continue;
                                }
                                ui.label(RichText::new(text).size(12.0));
                                shown += 1;
                                if shown >= 20 {
                                    break;
                                }
                            }
                        });
                    }
                    Inspector::Comments => {
                        ui.label(RichText::new("批注").size(13.0).strong());
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("当前版本尚未读取批注；打开含批注的文档时会在保真检查中标记。")
                                .size(12.0)
                                .color(MUTED),
                        );
                    }
                }
            });
    }

    fn status(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(30.0)
            .frame(Frame::NONE.fill(RAISED).inner_margin(Margin::symmetric(16, 6)))
            .show(context, |ui| {
            ui.style_mut().visuals.override_text_color = Some(INK);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "第 {} / {} 页 · {} 字 · 页面视图{}",
                            self.page + 1,
                            self.page_count(),
                            self.editor.document().char_count(),
                            self.status_message
                                .as_deref()
                                .map(|message| format!(" · {message}"))
                                .unwrap_or_default()
                        ))
                        .size(12.0)
                        .color(MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("+").clicked() {
                            let current = self.current_zoom();
                            self.fit = false;
                            self.zoom = (current * 1.15).min(4.0);
                        }
                        ui.label(
                            RichText::new(format!("{}%", (self.current_zoom() * 100.0).round()))
                                .size(12.0),
                        );
                        if ui.button("−").clicked() {
                            let current = self.current_zoom();
                            self.fit = false;
                            self.zoom = (current / 1.15).max(0.2);
                        }
                        if ui.button("适宽").clicked() {
                            self.fit = true;
                        }
                    });
                });
            });
    }

    fn current_zoom(&self) -> f32 {
        if self.fit {
            self.last_zoom
        } else {
            self.zoom
        }
    }

    fn canvas(&mut self, context: &egui::Context) {
        let canvas = if self.dark {
            Color32::from_rgb(0x14, 0x13, 0x11)
        } else {
            PAPER
        };
        let focused = context.memory(|memory| memory.has_focus(self.canvas_id));
        let mut ime_screen: Option<egui::Rect> = None;

        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(canvas))
            .show(context, |ui| {
                let (control, smooth_scroll, raw_scroll, zoom_delta) =
                    context.input(|input| {
                        (
                            input.modifiers.command,
                            input.smooth_scroll_delta.y,
                            input.raw_scroll_delta.y,
                            input.zoom_delta(),
                        )
                    });
                let scroll = if smooth_scroll.abs() > 0.01 {
                    smooth_scroll
                } else {
                    raw_scroll
                };
                let pointer_over_canvas = ui.rect_contains_pointer(ui.max_rect());
                let zoom_gesture = (zoom_delta - 1.0).abs() > 0.001;
                if zoom_gesture && pointer_over_canvas {
                    let current = self.current_zoom();
                    self.fit = false;
                    self.zoom = (current * zoom_delta).clamp(0.2, 4.0);
                } else if control
                    && scroll.abs() > 0.1
                    && pointer_over_canvas
                {
                    let current = self.current_zoom();
                    self.fit = false;
                    self.zoom = (current * (1.0015_f32).powf(scroll)).clamp(0.2, 4.0);
                    context.input_mut(|input| {
                        input.smooth_scroll_delta = Vec2::ZERO;
                        input.raw_scroll_delta = Vec2::ZERO;
                    });
                }
                let available = ui.available_width();
                let zoom = if self.fit {
                    self.fit_zoom(available)
                } else {
                    self.zoom
                };
                self.last_zoom = zoom;

                let sizes: Vec<Vec2> = self
                    .layout
                    .pages
                    .iter()
                    .map(|page| {
                        Vec2::new(
                            page.width_pt as f32 * zoom,
                            page.height_pt as f32 * zoom,
                        )
                    })
                    .collect();
                let gap = 24.0_f32;
                let margin = 24.0_f32;
                let mut offsets = Vec::with_capacity(sizes.len());
                let mut y = margin;
                for size in &sizes {
                    offsets.push(y);
                    y += size.y + gap;
                }
                let total_height = (y - gap + margin).max(1.0);
                let content_width = sizes
                    .iter()
                    .map(|size| size.x)
                    .fold(available, f32::max)
                    + 48.0;
                let pixels_per_point = context.pixels_per_point();
                let (selection_start, selection_end) = self.selection();

                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .drag_to_scroll(false)
                    .show_viewport(ui, |ui, viewport| {
                        ui.set_min_size(Vec2::new(content_width, total_height));
                        if ui.rect_contains_pointer(ui.max_rect())
                            && ui.input(|input| {
                                input.pointer.button_down(egui::PointerButton::Middle)
                            })
                        {
                            let delta = ui.input(|input| input.pointer.delta());
                            ui.scroll_with_delta(-delta);
                        }
                        let origin = ui.min_rect().min;
                        let painter = ui.painter().clone();
                        let mut current = None;

                        for (index, size) in sizes.iter().enumerate() {
                            let x = (content_width - size.x) / 2.0;
                            let content_rect = egui::Rect::from_min_size(
                                egui::pos2(x, offsets[index]),
                                *size,
                            );
                            let center = viewport.center().y;
                            if center >= offsets[index] && center <= offsets[index] + size.y {
                                current = Some(index);
                            }
                            if !content_rect.intersects(viewport) {
                                continue;
                            }
                            let screen = content_rect.translate(origin.to_vec2());
                            painter.rect_filled(
                                screen.translate(Vec2::new(0.0, 6.0)).expand(3.0),
                                CornerRadius::same(4),
                                Color32::from_black_alpha(26),
                            );
                            match self.page_texture(context, index, zoom * pixels_per_point) {
                                Some(texture) => {
                                    painter.image(
                                        texture.id(),
                                        screen,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        Color32::WHITE,
                                    );
                                }
                                None => {
                                    painter.rect_filled(screen, CornerRadius::ZERO, SURFACE);
                                }
                            }

                            if focused && self.table_context().is_some() {
                                if let Some((caret_page, caret)) =
                                    self.hit.caret_rect(self.caret, &self.metrics)
                                {
                                    if caret_page == index {
                                        let center = caret.center();
                                        for cell in &self.layout.pages[index].cells {
                                            if cell.rect.contains(
                                                center.x as f64,
                                                center.y as f64,
                                            ) {
                                                let rect = egui::Rect::from_min_size(
                                                    screen.min
                                                        + Vec2::new(
                                                            cell.rect.x as f32 * zoom,
                                                            cell.rect.y as f32 * zoom,
                                                        ),
                                                    Vec2::new(
                                                        cell.rect.width as f32 * zoom,
                                                        cell.rect.height as f32 * zoom,
                                                    ),
                                                );
                                                painter.rect_filled(
                                                    rect,
                                                    CornerRadius::ZERO,
                                                    Color32::from_rgba_unmultiplied(
                                                        217, 72, 15, 18,
                                                    ),
                                                );
                                                painter.rect_stroke(
                                                    rect.shrink(0.5),
                                                    CornerRadius::ZERO,
                                                    Stroke::new(
                                                        1.0_f32,
                                                        Color32::from_rgba_unmultiplied(
                                                            217, 72, 15, 120,
                                                        ),
                                                    ),
                                                    egui::StrokeKind::Inside,
                                                );
                                            }
                                        }
                                    }
                                }
                            }

                            if selection_start != selection_end {
                                if let Some(indices) = self.hit.by_page.get(&index) {
                                    for &line_index in indices {
                                        let line = &self.hit.lines[line_index];
                                        if line.paragraph < selection_start.paragraph
                                            || line.paragraph > selection_end.paragraph
                                        {
                                            continue;
                                        }
                                        let from = if line.paragraph == selection_start.paragraph {
                                            line.start.max(selection_start.character)
                                        } else {
                                            line.start
                                        };
                                        let to = if line.paragraph == selection_end.paragraph {
                                            line.end.min(selection_end.character)
                                        } else {
                                            line.end
                                        };
                                        if from >= to {
                                            continue;
                                        }
                                        let x1 =
                                            self.hit.x_for_offset(line, from, &self.metrics);
                                        let x2 =
                                            self.hit.x_for_offset(line, to, &self.metrics);
                                        let rect = egui::Rect::from_min_size(
                                            screen.min
                                                + Vec2::new(
                                                    x1 * zoom,
                                                    line.rect.top() * zoom,
                                                ),
                                            Vec2::new(
                                                (x2 - x1).max(1.0) * zoom,
                                                line.rect.height() * zoom,
                                            ),
                                        );
                                        painter.rect_filled(
                                            rect,
                                            CornerRadius::ZERO,
                                            Color32::from_rgba_unmultiplied(217, 72, 15, 52),
                                        );
                                    }
                                }
                            }

                            if self.find_open {
                                for (match_index, target) in
                                    self.find_matches.iter().enumerate().take(500)
                                {
                                    let start = DocPos {
                                        paragraph: target.paragraph,
                                        character: target.start,
                                    };
                                    let end = DocPos {
                                        paragraph: target.paragraph,
                                        character: target.end,
                                    };
                                    for rect in self.hit.range_rects(
                                        index,
                                        start,
                                        end,
                                        &self.metrics,
                                    ) {
                                        let screen_rect = egui::Rect::from_min_size(
                                            screen.min
                                                + Vec2::new(
                                                    rect.left() * zoom,
                                                    rect.top() * zoom,
                                                ),
                                            Vec2::new(
                                                rect.width() * zoom,
                                                rect.height() * zoom,
                                            ),
                                        );
                                        let color = if match_index == self.find_index
                                            && self.find_selected
                                        {
                                            Color32::from_rgba_unmultiplied(217, 72, 15, 80)
                                        } else {
                                            Color32::from_rgba_unmultiplied(255, 196, 90, 55)
                                        };
                                        painter.rect_filled(
                                            screen_rect,
                                            CornerRadius::ZERO,
                                            color,
                                        );
                                    }
                                }
                            }

                            if focused {
                                if let Some((caret_page, caret)) =
                                    self.hit.caret_rect(self.caret, &self.metrics)
                                {
                                    if caret_page == index {
                                        let rect = egui::Rect::from_min_size(
                                            screen.min
                                                + Vec2::new(
                                                    caret.left() * zoom,
                                                    caret.top() * zoom,
                                                ),
                                            Vec2::new(
                                                (caret.width() * zoom).max(1.5),
                                                caret.height() * zoom,
                                            ),
                                        );
                                        painter.rect_filled(rect, CornerRadius::ZERO, ACCENT);
                                        ime_screen = Some(rect);
                                        if !self.composition.is_empty() {
                                            painter.text(
                                                rect.left_bottom(),
                                                Align2::LEFT_TOP,
                                                &self.composition,
                                                egui::FontId::proportional(
                                                    (caret.height() * zoom * 0.9).max(10.0),
                                                ),
                                                ACCENT,
                                            );
                                            painter.line_segment(
                                                [
                                                    rect.left_bottom(),
                                                    rect.left_bottom() + Vec2::new(80.0, 0.0),
                                                ],
                                                Stroke::new(1.0_f32, ACCENT),
                                            );
                                        }
                                    }
                                }
                            }

                            if self.page == index {
                                painter.rect_stroke(
                                    screen,
                                    CornerRadius::ZERO,
                                    Stroke::new(1.5_f32, ACCENT),
                                    egui::StrokeKind::Outside,
                                );
                            }
                            painter.text(
                                screen.right_top() + Vec2::new(-6.0, 6.0),
                                Align2::RIGHT_TOP,
                                format!("{}", index + 1),
                                egui::FontId::proportional(10.0),
                                MUTED,
                            );
                        }

                        if let Some(index) = current {
                            if self.pending_scroll.is_none() {
                                self.page = index;
                            }
                        }

                        if let Some(index) = self.pending_scroll.take() {
                            if let Some(size) = sizes.get(index) {
                                let x = (content_width - size.x) / 2.0;
                                let rect = egui::Rect::from_min_size(
                                    egui::pos2(x, offsets[index]),
                                    *size,
                                );
                                // Top-align so the first line of the target page
                                // is visible; a vertically centred tall page
                                // opens on its blank middle.
                                ui.scroll_to_rect(
                                    rect.translate(origin.to_vec2()),
                                    Some(egui::Align::Min),
                                );
                                self.page = index;
                            }
                        }

                        let response = ui.interact(
                            ui.min_rect(),
                            self.canvas_id,
                            egui::Sense::click_and_drag(),
                        );
                        if response.clicked()
                            || response.drag_started()
                            || response.secondary_clicked()
                        {
                            response.request_focus();
                        }
                        if response.clicked()
                            || response.drag_started()
                            || response.dragged()
                            || response.secondary_clicked()
                        {
                            if let Some(pointer) =
                                ui.input(|input| input.pointer.interact_pos())
                            {
                                let local = pointer - origin;
                                let page_index = sizes.iter().enumerate().find_map(
                                    |(index, size)| {
                                        let x = (content_width - size.x) / 2.0;
                                        if local.x >= x
                                            && local.x <= x + size.x
                                            && local.y >= offsets[index]
                                            && local.y <= offsets[index] + size.y
                                        {
                                            Some(index)
                                        } else {
                                            None
                                        }
                                    },
                                );
                                if let Some(index) = page_index {
                                    let size = sizes[index];
                                    let x = (content_width - size.x) / 2.0;
                                    let page_point = egui::pos2(
                                        (local.x - x) / zoom,
                                        (local.y - offsets[index]) / zoom,
                                    );
                                    if let Some(position) =
                                        self.hit.position_at(index, page_point, &self.metrics)
                                    {
                                        if response.clicked()
                                            || response.drag_started()
                                            || response.secondary_clicked()
                                        {
                                            self.anchor = position;
                                        }
                                        self.caret = position;
                                        self.page = index;
                                        self.status_message = None;
                                    } else if response.clicked() {
                                        let in_table = self
                                            .layout
                                            .pages
                                            .get(index)
                                            .map(|page| {
                                                page.cells.iter().any(|cell| {
                                                    cell.rect.contains(
                                                        page_point.x as f64,
                                                        page_point.y as f64,
                                                    )
                                                })
                                            })
                                            .unwrap_or(false);
                                        self.status_message = Some(if in_table {
                                            "单击单元格内的文字即可编辑".to_string()
                                        } else {
                                            "该位置暂不支持编辑（编号段落、页眉页脚等）"
                                                .to_string()
                                        });
                                    }
                                }
                            }
                        }

                        response.context_menu(|ui| {
                            if ui
                                .add_enabled(
                                    self.editor.can_undo(),
                                    egui::Button::new("撤销"),
                                )
                                .clicked()
                            {
                                self.undo();
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(
                                    self.editor.can_redo(),
                                    egui::Button::new("重做"),
                                )
                                .clicked()
                            {
                                self.redo();
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("复制").clicked() {
                                let text = self.selected_text();
                                if !text.is_empty() {
                                    ui.ctx().copy_text(text);
                                }
                                ui.close_menu();
                            }
                            if ui.button("剪切").clicked() {
                                let text = self.selected_text();
                                if !text.is_empty() {
                                    ui.ctx().copy_text(text);
                                    self.delete_selection();
                                }
                                ui.close_menu();
                            }
                            if ui.button("删除").clicked() {
                                self.delete_selection();
                                ui.close_menu();
                            }
                            if ui.button("全选").clicked() {
                                let count = self.editor.document().editable_paragraph_count();
                                self.anchor = DocPos::default();
                                if count > 0 {
                                    let paragraph = count - 1;
                                    self.caret = DocPos {
                                        paragraph,
                                        character: self
                                            .editor
                                            .paragraph_len_global(paragraph)
                                            .unwrap_or(0),
                                    };
                                }
                                ui.close_menu();
                            }
                            ui.separator();
                            if self.table_context().is_some() {
                                if ui.button("上方插入行").clicked() {
                                    self.add_table_row_above();
                                    ui.close_menu();
                                }
                                if ui.button("下方插入行").clicked() {
                                    self.add_table_row();
                                    ui.close_menu();
                                }
                                if ui.button("左侧插入列").clicked() {
                                    self.add_table_column_left();
                                    ui.close_menu();
                                }
                                if ui.button("右侧插入列").clicked() {
                                    self.add_table_column();
                                    ui.close_menu();
                                }
                                if ui.button("删除当前行").clicked() {
                                    self.remove_table_row();
                                    ui.close_menu();
                                }
                                if ui.button("删除当前列").clicked() {
                                    self.remove_table_column();
                                    ui.close_menu();
                                }
                                if ui.button("切换表头底色").clicked() {
                                    self.toggle_table_header();
                                    ui.close_menu();
                                }
                                if ui.button("删除表格").clicked() {
                                    self.delete_current_table();
                                    ui.close_menu();
                                }
                                ui.separator();
                            }
                            if ui.button("插入表格...").clicked() {
                                self.table_dialog_open = true;
                                ui.close_menu();
                            }
                            if ui.button("插入图片...").clicked() {
                                self.insert_image_dialog();
                                ui.close_menu();
                            }
                            if ui.button("查找...").clicked() {
                                self.find_open = true;
                                self.find_focus = true;
                                self.refresh_matches();
                                ui.close_menu();
                            }
                        });
                    });
            });

        self.ime_caret_screen = ime_screen;
    }

    fn palette(&mut self, context: &egui::Context) {
        if !self.palette_open {
            return;
        }
        let mut open = true;
        egui::Window::new("命令")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 120.0])
            .min_width(420.0)
            .show(context, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.palette_query)
                        .hint_text("输入命令或页码")
                        .desired_width(f32::INFINITY),
                );
                if self.palette_focus {
                    response.request_focus();
                    self.palette_focus = false;
                }
                ui.add_space(6.0);
                if ui.button("适宽").clicked() {
                    self.fit = true;
                    self.palette_open = false;
                }
                if ui.button("100%").clicked() {
                    self.fit = false;
                    self.zoom = 1.0;
                    self.palette_open = false;
                }
                if ui.button("下一页").clicked() {
                    self.next_page();
                    self.palette_open = false;
                }
                if ui.button("上一页").clicked() {
                    self.previous_page();
                    self.palette_open = false;
                }
                if let Ok(page) = self.palette_query.trim().parse::<usize>() {
                    if ui.button(format!("跳转到第 {page} 页")).clicked() {
                        self.go_to(page.saturating_sub(1));
                        self.palette_open = false;
                    }
                }
            });
        if !open {
            self.palette_open = false;
        }
    }
}

impl eframe::App for QuireApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_drop(context);
        if self.sheet.is_some() {
            self.sheet_ui(context);
            return;
        }
        self.keyboard(context);
        self.title_bar(context);
        if self.welcome {
            self.welcome_screen(context);
            self.palette(context);
            return;
        }
        if self.focus_mode {
            self.canvas(context);
            self.focus_overlay(context);
            self.palette(context);
            self.editing(context);
            self.selection_pill(context);
            return;
        }
        self.deck(context);
        self.find_bar(context);
        if self.show_rail {
            self.rail(context);
        }
        if self.show_inspector {
            self.inspector(context);
        }
        self.status(context);
        self.canvas(context);
        self.table_dialog(context);
        self.palette(context);
        self.editing(context);
        self.selection_pill(context);
    }
}

fn evict(
    cache: &mut HashMap<(usize, u32), (TextureHandle, u64)>,
    capacity: usize,
    _current: u64,
) {
    while cache.len() > capacity {
        let oldest = cache
            .iter()
            .min_by_key(|(_, (_, used))| *used)
            .map(|(key, _)| *key);
        match oldest {
            Some(key) => {
                cache.remove(&key);
            }
            None => break,
        }
    }
}

fn mime_for_extension(extension: &str) -> &'static str {
    match extension {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

fn page_fingerprints(layout: &Layout) -> Vec<u64> {
    layout
        .pages
        .iter()
        .map(|page| {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            format!("{page:?}").hash(&mut hasher);
            hasher.finish()
        })
        .collect()
}

fn sheet_column_name(mut index: usize) -> String {
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

fn sheet_cell_reference(row: usize, column: usize) -> String {
    format!("{}{}", sheet_column_name(column), row + 1)
}

fn is_sheet_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
            .as_str(),
        "xlsx" | "xlsm" | "csv"
    )
}

fn table_count(document: &Document) -> usize {
    document
        .sections()
        .iter()
        .map(|section| section.table_count())
        .sum()
}

fn configure_fonts(context: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let candidates = [
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\segoeui.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let name = path.rsplit('\\').next().unwrap_or("ui").to_string();
            fonts
                .font_data
                .insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, name.clone());
            }
            break;
        }
    }
    context.set_fonts(fonts);
}

fn main() {
    if let Err(error) = run() {
        show_error(&format!("{error:#}"));
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    let metrics = SystemMetrics::new();
    let system_fonts = metrics.has_fonts();

    let input = args.input.clone();
    let sheet_mode = input.as_ref().is_some_and(|path| is_sheet_path(path));
    let sheet = if let Some(path) = input.as_ref().filter(|_| sheet_mode) {
        Some(SheetState::open(path)?)
    } else {
        None
    };
    let (editor, path, title, welcome) = if let Some(input) = input {
        let title = input
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "document".to_string());
        if sheet_mode {
            (Editor::new(Document::from_text("")), None, title, false)
        } else {
            let document = read_document(&input)
                .with_context(|| format!("cannot read {}", input.display()))?;
            (Editor::new(document), Some(input), title, false)
        }
    } else {
        (
            Editor::new(Document::from_text("")),
            None,
            "Quire".to_string(),
            true,
        )
    };

    let mut layout_cache = LayoutCache::default();
    let layout = layout_with_cache(
        editor.document(),
        &LayoutOptions::default(),
        &metrics,
        &mut layout_cache,
    )
    .map_err(|error| anyhow!("layout failed: {error}"))?;
    if layout.pages.is_empty() {
        return Err(anyhow!("document has no pages"));
    }
    if args.selftest {
        let svg_options = SvgOptions {
            fit_text_to_layout: false,
            draw_canvas_background: false,
            viewport_margin_pt: 0.0,
            page_gap_pt: 0.0,
            ..SvgOptions::default()
        };
        let svg = render_svg_page(&layout, 0, &svg_options)
            .ok_or_else(|| anyhow!("cannot render first page"))?;
        Rasterizer::new()
            .rasterize(&svg, 1.0)
            .map_err(|error| anyhow!("rasterize failed: {error}"))?;
        return Ok(());
    }
    let page = args.page.saturating_sub(1).min(layout.pages.len() - 1);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 840.0])
            .with_min_inner_size([980.0, 640.0])
            .with_title("Quire"),
        ..Default::default()
    };
    eframe::run_native(
        "Quire",
        options,
        Box::new(move |context| {
            let mut app = QuireApp::new(
                &context.egui_ctx,
                title,
                path,
                welcome,
                editor,
                layout,
                metrics,
                layout_cache,
                system_fonts,
                page,
            );
            app.sheet = sheet;
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| anyhow!("viewer: {error}"))
}

fn pick_document() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("打开文档")
        .add_filter("支持的文档", &["docx", "docm", "dotx", "rtf", "odt", "txt", "md", "xlsx", "xlsm", "csv"])
        .add_filter("Word 文档", &["docx", "docm", "dotx"])
        .add_filter("RTF", &["rtf"])
        .add_filter("OpenDocument", &["odt"])
        .add_filter("纯文本", &["txt", "md"])
        .add_filter("表格", &["xlsx", "xlsm", "csv"])
        .add_filter("所有文件", &["*"])
        .pick_file()
}

fn show_error(message: &str) {
    rfd::MessageDialog::new()
        .set_title("Quire")
        .set_description(message)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;
    use quire_layout::layout_deterministic;
    use quire_model::{Document, Paragraph};
    use quire_text::DeterministicMetrics;

    #[test]
    fn maps_clicks_and_carets_to_paragraph_offsets() {
        let mut document = Document::new();
        document.push_paragraph(Paragraph::plain("Hello world"));
        let layout = layout_deterministic(&document, &LayoutOptions::default()).unwrap();
        let hit = HitIndex::build(&layout, &document);
        let metrics = DeterministicMetrics;
        let line = &layout.pages[0].lines[0];

        let position = hit
            .position_at(
                0,
                egui::pos2(line.x as f32 + 0.1, (line.y + line.height / 2.0) as f32),
                &metrics,
            )
            .unwrap();
        assert_eq!(
            position,
            DocPos {
                paragraph: 0,
                character: 0
            }
        );

        let (page, caret) = hit
            .caret_rect(
                DocPos {
                    paragraph: 0,
                    character: 5,
                },
                &metrics,
            )
            .unwrap();
        assert_eq!(page, 0);
        assert!(caret.left() > line.x as f32);
        assert!(caret.height() > 0.0);
    }

    #[test]
    fn maps_table_cell_clicks_to_global_indices() {
        let mut document = Document::new();
        document.push_paragraph(Paragraph::plain("Body"));
        let mut editor = Editor::new(document);
        editor.insert_table(0, 0, 1, 1).unwrap();
        editor.insert_text_global(1, 0, "Cell").unwrap();
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let hit = HitIndex::build(&layout, editor.document());
        let metrics = DeterministicMetrics;

        let paragraph_id = editor
            .document()
            .paragraph_at_global(1)
            .expect("cell paragraph")
            .id
            .get();
        let line = layout.pages[0]
            .lines
            .iter()
            .find(|line| line.paragraph == paragraph_id)
            .expect("cell line");
        let position = hit
            .position_at(
                0,
                egui::pos2(
                    line.x as f32 + 0.1,
                    (line.y + line.height / 2.0) as f32,
                ),
                &metrics,
            )
            .unwrap();
        assert_eq!(position.paragraph, 1);
        assert_eq!(
            hit.by_paragraph.get(&1).map(Vec::len),
            Some(1)
        );
        let (page, caret) = hit
            .caret_rect(
                DocPos {
                    paragraph: 1,
                    character: 2,
                },
                &metrics,
            )
            .unwrap();
        assert_eq!(page, 0);
        assert!(caret.height() > 0.0);
    }


    #[test]
    fn sheet_references_and_grid_actions_work() {
        assert_eq!(sheet_cell_reference(0, 0), "A1");
        assert_eq!(sheet_cell_reference(9, 26), "AA10");
        assert!(is_sheet_path(Path::new("book.xlsx")));
        assert!(is_sheet_path(Path::new("data.CSV")));
        assert!(!is_sheet_path(Path::new("note.docx")));

        let mut state = SheetState {
            path: None,
            sheets: vec![SheetGrid {
                name: "Sheet1".to_string(),
                rows: vec![
                    vec!["A".to_string(), "B".to_string()],
                    vec!["1".to_string(), "2".to_string()],
                ],
            }],
            active: 0,
            selected: (0, 0),
            range_anchor: (0, 0),
            edit: None,
            dirty: false,
            status: None,
            scroll_to: None,
            zoom: 1.0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_open: false,
            find_query: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_focus: false,
        };
        state.set_cell(1, 1, "changed".to_string());
        assert_eq!(state.cell(1, 1), "changed");
        assert!(state.dirty);

        state.selected = (0, 0);
        state.insert_row();
        assert_eq!(state.cell(0, 0), "");
        assert_eq!(state.cell(1, 0), "A");

        state.selected = (0, 0);
        state.insert_column();
        assert_eq!(state.cell(1, 1), "A");
        assert_eq!(state.cell(1, 2), "B");

        state.selected = (0, 1);
        state.delete_column();
        assert_eq!(state.cell(1, 1), "B");

        state.selected = (0, 0);
        state.delete_row();
        assert_eq!(state.cell(0, 0), "");
        assert_eq!(state.cell(0, 1), "B");
    }


    #[test]
    fn sheet_grid_draws_headless() {
        let context = egui::Context::default();
        let mut document = Document::new();
        document.push_paragraph(Paragraph::plain("Body"));
        let editor = Editor::new(document);
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let mut app = QuireApp::new(
            &context,
            "sheet".to_string(),
            None,
            false,
            editor,
            layout,
            SystemMetrics::new(),
            LayoutCache::default(),
            false,
            0,
        );
        app.sheet = Some(SheetState {
            path: None,
            sheets: vec![SheetGrid {
                name: "Sheet1".to_string(),
                rows: vec![
                    vec!["Name".to_string(), "Qty".to_string()],
                    vec!["Pen".to_string(), "12".to_string()],
                ],
            }],
            active: 0,
            selected: (1, 1),
            range_anchor: (1, 1),
            edit: None,
            dirty: false,
            status: None,
            scroll_to: None,
            zoom: 1.0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_open: false,
            find_query: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_focus: false,
        });
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1000.0, 700.0),
            )),
            ..Default::default()
        };
        let _ = context.run(input, |context| {
            app.sheet_ui(context);
        });
    }


    #[test]
    fn sheet_click_and_typing_starts_edit() {
        let context = egui::Context::default();
        let mut document = Document::new();
        document.push_paragraph(Paragraph::plain("Body"));
        let editor = Editor::new(document);
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let mut app = QuireApp::new(
            &context,
            "sheet".to_string(),
            None,
            false,
            editor,
            layout,
            SystemMetrics::new(),
            LayoutCache::default(),
            false,
            0,
        );
        app.sheet = Some(SheetState {
            path: None,
            sheets: vec![SheetGrid {
                name: "Sheet1".to_string(),
                rows: vec![vec!["A".to_string()]],
            }],
            active: 0,
            selected: (0, 0),
            range_anchor: (0, 0),
            edit: None,
            dirty: false,
            status: None,
            scroll_to: None,
            zoom: 1.0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_open: false,
            find_query: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_focus: false,
        });

        let screen = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1000.0, 700.0),
        );
        fn frame(
            app: &mut QuireApp,
            context: &egui::Context,
            screen: egui::Rect,
            events: Vec<egui::Event>,
        ) {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                focused: true,
                ..Default::default()
            };
            let _ = context.run(input, |context| {
                app.sheet_ui(context);
            });
        }

        let point = egui::pos2(80.0, 155.0);
        frame(
            &mut app,
            &context,
            screen,
            vec![egui::Event::PointerMoved(point)],
        );
        frame(
            &mut app,
            &context,
            screen,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            }],
        );
        frame(
            &mut app,
            &context,
            screen,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        frame(&mut app, &context, screen, Vec::new());
        frame(
            &mut app,
            &context,
            screen,
            vec![egui::Event::Text("X".to_string())],
        );
        assert_eq!(app.sheet.as_ref().unwrap().selected, (0, 0));
        let edit = app.sheet.as_ref().unwrap().edit.as_ref();
        assert!(edit.is_some(), "typing should begin cell edit");
        assert_eq!(edit.unwrap().text, "X");

        frame(
            &mut app,
            &context,
            screen,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        let sheet = app.sheet.as_ref().unwrap();
        assert_eq!(sheet.cell(0, 0), "X");
        assert_eq!(sheet.selected, (1, 0));
    }


    #[test]
    fn sheet_undo_redo_restores_cells() {
        let mut state = SheetState {
            path: None,
            sheets: vec![SheetGrid {
                name: "Sheet1".to_string(),
                rows: vec![vec!["A".to_string()]],
            }],
            active: 0,
            selected: (0, 0),
            range_anchor: (0, 0),
            edit: None,
            dirty: false,
            status: None,
            scroll_to: None,
            zoom: 1.0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_open: false,
            find_query: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_focus: false,
        };
        state.begin_edit(Some("B".to_string()));
        state.commit_edit();
        assert_eq!(state.cell(0, 0), "B");
        assert!(state.undo());
        assert_eq!(state.cell(0, 0), "A");
        assert!(state.redo());
        assert_eq!(state.cell(0, 0), "B");
    }


    #[test]
    fn sheet_range_copy_and_clear_work() {
        let mut state = SheetState {
            path: None,
            sheets: vec![SheetGrid {
                name: "Sheet1".to_string(),
                rows: vec![
                    vec!["A".to_string(), "B".to_string()],
                    vec!["1".to_string(), "2".to_string()],
                ],
            }],
            active: 0,
            selected: (1, 1),
            range_anchor: (0, 0),
            edit: None,
            dirty: false,
            status: None,
            scroll_to: None,
            zoom: 1.0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_open: false,
            find_query: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_focus: false,
        };
        assert_eq!(state.selection_reference(), "A1:B2");
        assert_eq!(state.selection_text(), "A\tB\n1\t2");
        state.selected = (0, 0);
        state.range_anchor = (0, 0);
        state.move_selection_extended(1, 1, true);
        assert_eq!(state.selection_reference(), "A1:B2");
        state.clear_range();
        assert_eq!(state.cell(0, 0), "");
        assert_eq!(state.cell(1, 1), "");
        assert!(state.undo());
        assert_eq!(state.cell(0, 0), "A");
        assert_eq!(state.cell(1, 1), "2");
    }


    #[test]
    fn sheet_find_locates_cells() {
        let mut state = SheetState {
            path: None,
            sheets: vec![SheetGrid {
                name: "Sheet1".to_string(),
                rows: vec![
                    vec!["Name".to_string(), "Qty".to_string()],
                    vec!["Pen".to_string(), "12".to_string()],
                ],
            }],
            active: 0,
            selected: (0, 0),
            range_anchor: (0, 0),
            edit: None,
            dirty: false,
            status: None,
            scroll_to: None,
            zoom: 1.0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_open: false,
            find_query: "pen".to_string(),
            find_matches: Vec::new(),
            find_index: 0,
            find_focus: false,
        };
        state.refresh_find();
        assert_eq!(state.find_matches, vec![(1, 0)]);
        state.find_next(true);
        assert_eq!(state.selected, (1, 0));
    }


    #[test]
    fn document_table_insert_puts_caret_in_first_cell() {
        let context = egui::Context::default();
        let document = Document::from_text("Body\n");
        let editor = Editor::new(document);
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let mut app = QuireApp::new(
            &context,
            "doc".to_string(),
            None,
            false,
            editor,
            layout,
            SystemMetrics::new(),
            LayoutCache::default(),
            false,
            0,
        );
        app.caret = DocPos::default();
        app.anchor = app.caret;
        app.table_rows = 2;
        app.table_columns = 2;
        app.insert_table_at_caret();

        let location = app
            .editor
            .document()
            .paragraph_location(app.caret.paragraph)
            .expect("caret should be a table cell");
        assert!(matches!(
            location.0,
            ContainerId::TableCell { row: 0, cell: 0, .. }
        ));

        app.insert_text("X");
        let document = app.editor.document();
        assert!(
            document
                .paragraph_at_global(app.caret.paragraph)
                .map(|paragraph| paragraph.text().contains('X'))
                .unwrap_or(false),
            "typed text should stay in the table cell"
        );
        assert_eq!(document.paragraph(0, 0).unwrap().text(), "Body");
    }


    #[test]
    fn document_table_empty_cell_click_maps_to_cell() {
        let context = egui::Context::default();
        let document = Document::from_text("Body\n");
        let editor = Editor::new(document);
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let mut app = QuireApp::new(
            &context,
            "doc".to_string(),
            None,
            false,
            editor,
            layout,
            SystemMetrics::new(),
            LayoutCache::default(),
            false,
            0,
        );
        app.caret = DocPos::default();
        app.anchor = app.caret;
        app.table_rows = 2;
        app.table_columns = 2;
        app.insert_table_at_caret();

        let table = match app.editor.document().sections()[0]
            .blocks
            .iter()
            .find_map(|block| match block {
                quire_model::Block::Table(table) => Some(table),
                _ => None,
            }) {
            Some(table) => table,
            None => panic!("table not inserted"),
        };
        let id = match &table.rows[1].cells[1].blocks[0] {
            quire_model::Block::Paragraph(paragraph) => paragraph.id,
            _ => panic!("cell paragraph missing"),
        };
        let paragraph = app
            .editor
            .document()
            .global_index_of_paragraph(id)
            .expect("global cell index");
        let line = app.layout.pages[0]
            .lines
            .iter()
            .find(|line| line.paragraph == id.get())
            .expect("cell line");
        let position = app
            .hit
            .position_at(
                0,
                egui::pos2(
                    line.x as f32 + 5.0,
                    (line.y + line.height / 2.0) as f32,
                ),
                &app.metrics,
            )
            .expect("click maps to a paragraph");
        assert_eq!(position.paragraph, paragraph);
        app.caret = position;
        app.anchor = position;
        app.insert_text("Y");
        assert_eq!(
            app.editor
                .document()
                .paragraph_at_global(paragraph)
                .unwrap()
                .text(),
            "Y"
        );
        assert_eq!(
            app.editor.document().paragraph(0, 0).unwrap().text(),
            "Body"
        );
    }


    #[test]
    fn sheet_grid_shows_formula_results_and_raw_formula_in_bar() {
        let state = SheetState {
            path: None,
            sheets: vec![SheetGrid {
                name: "Sheet1".to_string(),
                rows: vec![vec![
                    "1".to_string(),
                    "2".to_string(),
                    "=SUM(A1:B1)".to_string(),
                ]],
            }],
            active: 0,
            selected: (0, 2),
            range_anchor: (0, 2),
            edit: None,
            dirty: false,
            status: None,
            scroll_to: None,
            zoom: 1.0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_open: false,
            find_query: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_focus: false,
        };
        assert_eq!(state.cell(0, 2), "=SUM(A1:B1)");
        assert_eq!(state.display_cell(0, 2), "3");
        assert_eq!(state.selection_text(), "3");
    }


    #[test]
    fn document_table_typing_after_insert_stays_in_cell() {
        let context = egui::Context::default();
        let document = Document::from_text("Body\n");
        let editor = Editor::new(document);
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let mut app = QuireApp::new(
            &context,
            "doc".to_string(),
            None,
            false,
            editor,
            layout,
            SystemMetrics::new(),
            LayoutCache::default(),
            false,
            0,
        );
        app.caret = DocPos::default();
        app.anchor = app.caret;
        app.table_rows = 2;
        app.table_columns = 2;
        app.insert_table_at_caret();
        let cell = app.caret.paragraph;

        context.memory_mut(|memory| memory.request_focus(app.canvas_id));
        let screen = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1000.0, 700.0),
        );
        let input = egui::RawInput {
            screen_rect: Some(screen),
            focused: true,
            ..Default::default()
        };
        let _ = context.run(input, |context| {
            app.canvas(context);
        });

        let input = egui::RawInput {
            screen_rect: Some(screen),
            focused: true,
            events: vec![egui::Event::Text("Z".to_string())],
            ..Default::default()
        };
        let _ = context.run(input, |context| {
            app.editing(context);
        });

        assert_eq!(app.caret.paragraph, cell);
        assert!(app
            .editor
            .document()
            .paragraph_at_global(cell)
            .unwrap()
            .text()
            .contains('Z'));
        assert_eq!(app.editor.document().paragraph(0, 0).unwrap().text(), "Body");
    }


    #[test]
    fn document_table_click_second_cell_then_type_stays_there() {
        let context = egui::Context::default();
        let document = Document::from_text("Body\n");
        let editor = Editor::new(document);
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let mut app = QuireApp::new(
            &context,
            "doc".to_string(),
            None,
            false,
            editor,
            layout,
            SystemMetrics::new(),
            LayoutCache::default(),
            false,
            0,
        );
        app.caret = DocPos::default();
        app.anchor = app.caret;
        app.table_rows = 2;
        app.table_columns = 2;
        app.insert_table_at_caret();

        let table = match app.editor.document().sections()[0]
            .blocks
            .iter()
            .find_map(|block| match block {
                quire_model::Block::Table(table) => Some(table),
                _ => None,
            }) {
            Some(table) => table,
            None => panic!("table not inserted"),
        };
        let id = match &table.rows[1].cells[1].blocks[0] {
            quire_model::Block::Paragraph(paragraph) => paragraph.id,
            _ => panic!("cell paragraph missing"),
        };
        let target = app
            .editor
            .document()
            .global_index_of_paragraph(id)
            .unwrap();
        let line = app.layout.pages[0]
            .lines
            .iter()
            .find(|line| line.paragraph == id.get())
            .expect("cell line");
        let zoom = app.fit_zoom(1000.0);
        let page_width = app.layout.pages[0].width_pt as f32 * zoom;
        let content_width = 1000.0_f32.max(page_width) + 48.0;
        let page_x = (content_width - page_width) / 2.0;
        let point = egui::pos2(
            page_x + line.x as f32 * zoom + 4.0,
            24.0 + (line.y + line.height / 2.0) as f32 * zoom,
        );
        let screen = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1000.0, 700.0),
        );

        fn frame(
            app: &mut QuireApp,
            context: &egui::Context,
            screen: egui::Rect,
            events: Vec<egui::Event>,
            edit: bool,
        ) {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                focused: true,
                events,
                ..Default::default()
            };
            let _ = context.run(input, |context| {
                app.canvas(context);
                if edit {
                    app.editing(context);
                }
            });
        }

        frame(&mut app, &context, screen, vec![egui::Event::PointerMoved(point)], false);
        frame(
            &mut app,
            &context,
            screen,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            }],
            false,
        );
        frame(
            &mut app,
            &context,
            screen,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            }],
            true,
        );
        assert_eq!(app.caret.paragraph, target);
        assert_eq!(app.anchor.paragraph, target);

        frame(
            &mut app,
            &context,
            screen,
            vec![egui::Event::Text("Q".to_string())],
            true,
        );
        assert_eq!(app.caret.paragraph, target);
        assert!(app
            .editor
            .document()
            .paragraph_at_global(target)
            .unwrap()
            .text()
            .contains('Q'));
        assert_eq!(app.editor.document().paragraph(0, 0).unwrap().text(), "Body");
    }


    #[test]
    fn typing_with_caret_in_table_and_anchor_in_body_stays_in_table() {
        let context = egui::Context::default();
        let document = Document::from_text("Body\n");
        let editor = Editor::new(document);
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let mut app = QuireApp::new(
            &context,
            "doc".to_string(),
            None,
            false,
            editor,
            layout,
            SystemMetrics::new(),
            LayoutCache::default(),
            false,
            0,
        );
        app.caret = DocPos::default();
        app.anchor = app.caret;
        app.table_rows = 2;
        app.table_columns = 2;
        app.insert_table_at_caret();
        let cell = app.caret.paragraph;
        app.anchor = DocPos {
            paragraph: 0,
            character: 0,
        };
        app.insert_text("X");
        assert_eq!(app.editor.document().paragraph(0, 0).unwrap().text(), "Body");
        assert!(app
            .editor
            .document()
            .paragraph_at_global(cell)
            .unwrap()
            .text()
            .contains('X'));
    }


    #[test]
    fn ctrl_wheel_changes_zoom() {
        let context = egui::Context::default();
        let document = Document::from_text("Zoom\n");
        let editor = Editor::new(document);
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let mut app = QuireApp::new(
            &context,
            "doc".to_string(),
            None,
            false,
            editor,
            layout,
            SystemMetrics::new(),
            LayoutCache::default(),
            false,
            0,
        );
        app.fit = true;
        let screen = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1000.0, 700.0),
        );
        let input = egui::RawInput {
            screen_rect: Some(screen),
            focused: true,
            events: vec![
                egui::Event::PointerMoved(egui::pos2(500.0, 300.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 120.0),
                    modifiers: egui::Modifiers::CTRL,
                },
            ],
            ..Default::default()
        };
        let _ = context.run(input, |context| {
            app.canvas(context);
        });
        assert!(!app.fit);
        assert!((app.zoom - 1.0).abs() > 0.001);
    }


    #[test]
    fn sheet_ctrl_wheel_zooms_grid() {
        let context = egui::Context::default();
        let mut document = Document::new();
        document.push_paragraph(Paragraph::plain("Body"));
        let editor = Editor::new(document);
        let layout =
            layout_deterministic(editor.document(), &LayoutOptions::default()).unwrap();
        let mut app = QuireApp::new(
            &context,
            "sheet".to_string(),
            None,
            false,
            editor,
            layout,
            SystemMetrics::new(),
            LayoutCache::default(),
            false,
            0,
        );
        app.sheet = Some(SheetState {
            path: None,
            sheets: vec![SheetGrid {
                name: "Sheet1".to_string(),
                rows: vec![vec!["A".to_string()]],
            }],
            active: 0,
            selected: (0, 0),
            range_anchor: (0, 0),
            edit: None,
            dirty: false,
            status: None,
            scroll_to: None,
            zoom: 1.0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_open: false,
            find_query: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_focus: false,
        });
        let screen = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1000.0, 700.0),
        );
        let input = egui::RawInput {
            screen_rect: Some(screen),
            focused: true,
            events: vec![
                egui::Event::PointerMoved(egui::pos2(300.0, 200.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 120.0),
                    modifiers: egui::Modifiers::CTRL,
                },
            ],
            ..Default::default()
        };
        let _ = context.run(input, |context| {
            app.sheet_grid(context);
        });
        assert!(app.sheet.as_ref().unwrap().zoom > 1.0);
    }

}

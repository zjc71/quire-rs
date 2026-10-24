//! Editing operations and undo history.
//!
//! Milestone 1 uses whole-document snapshots for undo. That is simple and
//! obviously correct; it is also the explicit placeholder for the final
//! command/inverse-command design described in the architecture document.
//! Documents in this milestone are small, and the API does not change when the
//! implementation does.

#![forbid(unsafe_code)]

use quire_base::{QuireError, Result, TableId};
use quire_model::{
    Align, Block, Border, CharProps, Color, Document, ImageBlock, Paragraph, Table, TableBorders,
    TableCell, TableRow,
};

/// A text match inside one paragraph, measured in characters.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TextMatch {
    pub paragraph: usize,
    pub start: usize,
    pub end: usize,
}


/// An editable document with bounded undo history.
#[derive(Clone, Debug)]
pub struct Editor {
    document: Document,
    undo: Vec<Document>,
    redo: Vec<Document>,
    history_limit: usize,
}

impl Editor {
    pub fn new(document: Document) -> Self {
        Self {
            document,
            undo: Vec::new(),
            redo: Vec::new(),
            history_limit: 128,
        }
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn into_document(self) -> Document {
        self.document
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Insert text at a character offset inside a paragraph.
    ///
    /// Returns the character offset immediately after the inserted text, which
    /// is where a caret should be placed.
    pub fn insert_text(
        &mut self,
        section: usize,
        paragraph: usize,
        char_offset: usize,
        text: &str,
    ) -> Result<usize> {
        let (length, style) = {
            let target = self
                .document
                .paragraph(section, paragraph)
                .ok_or_else(|| missing(section, paragraph))?;
            (
                target.char_count(),
                target.style_at(char_offset.saturating_sub(1)),
            )
        };
        if char_offset > length {
            return Err(QuireError::edit(format!(
                "offset {char_offset} is past the end of paragraph {paragraph} ({length} chars)"
            )));
        }

        self.checkpoint();
        let inserted = text.chars().count();
        if let Some(target) = self.document.paragraph_mut(section, paragraph) {
            let mut chars = target.char_styles();
            for (index, ch) in text.chars().enumerate() {
                chars.insert(char_offset + index, (ch, style.clone()));
            }
            target.set_char_styles(&chars);
        }
        self.document.bump_revision();
        Ok(char_offset + inserted)
    }

    /// Delete the half-open character range `[start, end)` in a paragraph.
    pub fn delete_range(
        &mut self,
        section: usize,
        paragraph: usize,
        start: usize,
        end: usize,
    ) -> Result<()> {
        let length = self
            .document
            .paragraph(section, paragraph)
            .ok_or_else(|| missing(section, paragraph))?
            .char_count();
        if start > end || end > length {
            return Err(QuireError::edit(format!(
                "invalid delete range {start}..{end} for a {length} character paragraph"
            )));
        }
        if start == end {
            return Ok(());
        }

        self.checkpoint();
        if let Some(target) = self.document.paragraph_mut(section, paragraph) {
            let mut chars = target.char_styles();
            chars.drain(start..end);
            target.set_char_styles(&chars);
        }
        self.document.bump_revision();
        Ok(())
    }

    /// Character length of a paragraph.
    pub fn paragraph_len(&self, section: usize, paragraph: usize) -> Option<usize> {
        self.document
            .paragraph(section, paragraph)
            .map(Paragraph::char_count)
    }

    /// Split a paragraph at a character offset, returning the new paragraph index.
    pub fn split_paragraph(
        &mut self,
        section: usize,
        paragraph: usize,
        char_offset: usize,
    ) -> Result<usize> {
        let length = self
            .paragraph_len(section, paragraph)
            .ok_or_else(|| missing(section, paragraph))?;
        if char_offset > length {
            return Err(QuireError::edit("split offset is out of range"));
        }

        self.checkpoint();
        let id = self.document.next_para_id();
        let (props, style, numbering, first, second) = {
            let target = self
                .document
                .paragraph(section, paragraph)
                .ok_or_else(|| missing(section, paragraph))?;
            let chars = target.char_styles();
            (
                target.props.clone(),
                target.style.clone(),
                target.numbering,
                chars[..char_offset].to_vec(),
                chars[char_offset..].to_vec(),
            )
        };

        if let Some(target) = self.document.paragraph_mut(section, paragraph) {
            target.set_char_styles(&first);
        }
        let mut second_paragraph = Paragraph::new(id);
        second_paragraph.props = props;
        second_paragraph.style = style;
        second_paragraph.numbering = numbering;
        second_paragraph.set_char_styles(&second);
        self.document
            .insert_paragraph_at(section, paragraph + 1, second_paragraph)?;
        self.document.bump_revision();
        Ok(paragraph + 1)
    }

    /// Delete a range that may span several paragraphs, merging the remainder.
    pub fn delete_across(
        &mut self,
        section: usize,
        start_paragraph: usize,
        start: usize,
        end_paragraph: usize,
        end: usize,
    ) -> Result<()> {
        let start_len = self
            .paragraph_len(section, start_paragraph)
            .ok_or_else(|| missing(section, start_paragraph))?;
        let end_len = self
            .paragraph_len(section, end_paragraph)
            .ok_or_else(|| missing(section, end_paragraph))?;
        if start > start_len || end > end_len || start_paragraph > end_paragraph {
            return Err(QuireError::edit("selection is out of range"));
        }
        if start_paragraph == end_paragraph {
            return self.delete_range(section, start_paragraph, start, end);
        }

        self.checkpoint();
        let tail = {
            let target = self
                .document
                .paragraph(section, end_paragraph)
                .ok_or_else(|| missing(section, end_paragraph))?;
            target.char_styles()[end..].to_vec()
        };
        if let Some(target) = self.document.paragraph_mut(section, start_paragraph) {
            let mut chars = target.char_styles();
            chars.truncate(start);
            chars.extend(tail);
            target.set_char_styles(&chars);
        }
        self.document
            .remove_paragraph_range(section, start_paragraph + 1, end_paragraph + 1)?;
        self.document.bump_revision();
        Ok(())
    }

    /// Character formatting at a specific position.
    pub fn char_props_at(
        &self,
        section: usize,
        paragraph: usize,
        character: usize,
    ) -> Option<CharProps> {
        self.document
            .paragraph(section, paragraph)
            .map(|paragraph| paragraph.style_at(character))
    }

    /// Paragraph formatting at a specific paragraph.
    pub fn paragraph_props_at(&self, section: usize, paragraph: usize) -> Option<quire_model::ParaProps> {
        self.document
            .paragraph(section, paragraph)
            .map(|paragraph| paragraph.props.clone())
    }

    /// Change paragraph properties for a paragraph range.
    pub fn set_paragraph_format(
        &mut self,
        section: usize,
        start_paragraph: usize,
        end_paragraph: usize,
        apply: impl Fn(&mut quire_model::ParaProps),
    ) -> Result<()> {
        if start_paragraph > end_paragraph {
            return Err(QuireError::edit("selection is out of range"));
        }
        self.checkpoint();
        for paragraph_index in start_paragraph..=end_paragraph {
            if let Some(paragraph) = self.document.paragraph_mut(section, paragraph_index) {
                apply(&mut paragraph.props);
            }
        }
        self.document.bump_revision();
        Ok(())
    }

    /// Apply a named paragraph style to a paragraph range.
    pub fn apply_paragraph_style(
        &mut self,
        section: usize,
        start_paragraph: usize,
        end_paragraph: usize,
        style_id: &str,
    ) -> Result<()> {
        let Some(style) = self.document.styles().get(style_id).cloned() else {
            return Err(QuireError::edit("style not found"));
        };
        if style.kind != quire_model::StyleKind::Paragraph {
            return Err(QuireError::edit("style is not a paragraph style"));
        }
        self.checkpoint();
        for paragraph_index in start_paragraph..=end_paragraph {
            let Some(paragraph) = self.document.paragraph_mut(section, paragraph_index) else {
                continue;
            };
            paragraph.style = Some(style.id.clone());
            if let Some(props) = &style.para {
                paragraph.props = props.clone();
            }
            if let Some(char_props) = &style.char {
                for run in &mut paragraph.runs {
                    run.props = char_props.clone();
                    run.style = None;
                }
            }
        }
        self.document.bump_revision();
        Ok(())
    }

    /// Apply a character-formatting change to a (possibly multi-paragraph) range.
    pub fn format_range(
        &mut self,
        section: usize,
        start_paragraph: usize,
        start: usize,
        end_paragraph: usize,
        end: usize,
        apply: impl Fn(&mut CharProps),
    ) -> Result<()> {
        if start_paragraph > end_paragraph {
            return Err(QuireError::edit("selection is out of range"));
        }
        self.checkpoint();
        let mut touched = false;
        for paragraph_index in start_paragraph..=end_paragraph {
            let Some(target) = self.document.paragraph_mut(section, paragraph_index) else {
                continue;
            };
            let length = target.char_count();
            let from = if paragraph_index == start_paragraph {
                start
            } else {
                0
            }
            .min(length);
            let to = if paragraph_index == end_paragraph {
                end
            } else {
                length
            }
            .min(length);
            if from >= to {
                continue;
            }
            let mut chars = target.char_styles();
            for (_, props) in &mut chars[from..to] {
                apply(props);
            }
            target.set_char_styles(&chars);
            touched = true;
        }
        if touched {
            self.document.bump_revision();
        }
        Ok(())
    }

    /// Change paragraph alignment for a paragraph range.
    pub fn set_alignment(
        &mut self,
        section: usize,
        start_paragraph: usize,
        end_paragraph: usize,
        align: Align,
    ) -> Result<()> {
        if start_paragraph > end_paragraph {
            return Err(QuireError::edit("selection is out of range"));
        }
        self.checkpoint();
        for paragraph_index in start_paragraph..=end_paragraph {
            if let Some(paragraph) = self.document.paragraph_mut(section, paragraph_index) {
                paragraph.props.align = align;
            }
        }
        self.document.bump_revision();
        Ok(())
    }

    /// Find every occurrence of `query` in the section's top-level paragraphs.
    pub fn find_all(
        &self,
        section: usize,
        query: &str,
        case_sensitive: bool,
    ) -> Vec<TextMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let needle: Vec<char> = query.chars().collect();
        let mut matches = Vec::new();
        let Some(section) = self.document.sections().get(section) else {
            return Vec::new();
        };
        for (paragraph_index, paragraph) in section.paragraphs().enumerate() {
            let haystack: Vec<char> = paragraph.text().chars().collect();
            if needle.len() > haystack.len() {
                continue;
            }
            for start in 0..=haystack.len() - needle.len() {
                if chars_equal(&haystack[start..start + needle.len()], &needle, case_sensitive) {
                    matches.push(TextMatch {
                        paragraph: paragraph_index,
                        start,
                        end: start + needle.len(),
                    });
                }
            }
        }
        matches
    }

    /// Replace one match, preserving the first matched character's formatting.
    pub fn replace_match(
        &mut self,
        section: usize,
        target: TextMatch,
        replacement: &str,
    ) -> Result<()> {
        self.checkpoint();
        self.replace_at(section, target, replacement)?;
        self.document.bump_revision();
        Ok(())
    }

    /// Replace every match in one undo transaction.
    pub fn replace_all(
        &mut self,
        section: usize,
        query: &str,
        replacement: &str,
        case_sensitive: bool,
    ) -> Result<usize> {
        let matches = self.find_all(section, query, case_sensitive);
        if matches.is_empty() {
            return Ok(0);
        }
        self.checkpoint();
        for target in matches.iter().rev().copied() {
            self.replace_at(section, target, replacement)?;
        }
        self.document.bump_revision();
        Ok(matches.len())
    }

    fn replace_at(
        &mut self,
        section: usize,
        target: TextMatch,
        replacement: &str,
    ) -> Result<()> {
        let Some(paragraph) = self.document.paragraph_mut(section, target.paragraph) else {
            return Err(QuireError::edit("match paragraph not found"));
        };
        let mut chars = paragraph.char_styles();
        if target.start > target.end || target.end > chars.len() {
            return Err(QuireError::edit("match range is out of bounds"));
        }
        let style = chars[target.start].1.clone();
        chars.drain(target.start..target.end);
        for (offset, ch) in replacement.chars().enumerate() {
            chars.insert(target.start + offset, (ch, style.clone()));
        }
        paragraph.set_char_styles(&chars);
        Ok(())
    }

    // ----- global paragraph editing (body paragraphs and table cells) -----

    pub fn paragraph_len_global(&self, index: usize) -> Option<usize> {
        self.document
            .paragraph_at_global(index)
            .map(Paragraph::char_count)
    }

    pub fn char_props_at_global(
        &self,
        index: usize,
        character: usize,
    ) -> Option<CharProps> {
        self.document
            .paragraph_at_global(index)
            .map(|paragraph| paragraph.style_at(character))
    }

    pub fn paragraph_props_at_global(
        &self,
        index: usize,
    ) -> Option<quire_model::ParaProps> {
        self.document
            .paragraph_at_global(index)
            .map(|paragraph| paragraph.props.clone())
    }

    pub fn insert_text_global(
        &mut self,
        index: usize,
        char_offset: usize,
        text: &str,
    ) -> Result<usize> {
        let (length, style) = {
            let target = self
                .document
                .paragraph_at_global(index)
                .ok_or_else(|| global_missing(index))?;
            (
                target.char_count(),
                target.style_at(char_offset.saturating_sub(1)),
            )
        };
        if char_offset > length {
            return Err(QuireError::edit(format!(
                "offset {char_offset} is past the end of paragraph {index} ({length} chars)"
            )));
        }
        self.checkpoint();
        let inserted = text.chars().count();
        if let Some(target) = self.document.paragraph_at_global_mut(index) {
            let mut chars = target.char_styles();
            for (offset, ch) in text.chars().enumerate() {
                chars.insert(char_offset + offset, (ch, style.clone()));
            }
            target.set_char_styles(&chars);
        }
        self.document.bump_revision();
        Ok(char_offset + inserted)
    }

    pub fn delete_range_global(
        &mut self,
        index: usize,
        start: usize,
        end: usize,
    ) -> Result<()> {
        let length = self
            .document
            .paragraph_at_global(index)
            .ok_or_else(|| global_missing(index))?
            .char_count();
        if start > end || end > length {
            return Err(QuireError::edit(format!(
                "invalid delete range {start}..{end} for a {length} character paragraph"
            )));
        }
        if start == end {
            return Ok(());
        }
        self.checkpoint();
        if let Some(target) = self.document.paragraph_at_global_mut(index) {
            let mut chars = target.char_styles();
            chars.drain(start..end);
            target.set_char_styles(&chars);
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn split_global(&mut self, index: usize, char_offset: usize) -> Result<usize> {
        let length = self
            .paragraph_len_global(index)
            .ok_or_else(|| global_missing(index))?;
        if char_offset > length {
            return Err(QuireError::edit("split offset is out of range"));
        }
        self.checkpoint();
        let id = self.document.next_para_id();
        let (props, style, numbering, first, second) = {
            let target = self
                .document
                .paragraph_at_global(index)
                .ok_or_else(|| global_missing(index))?;
            let chars = target.char_styles();
            (
                target.props.clone(),
                target.style.clone(),
                target.numbering,
                chars[..char_offset].to_vec(),
                chars[char_offset..].to_vec(),
            )
        };
        if let Some(target) = self.document.paragraph_at_global_mut(index) {
            target.set_char_styles(&first);
        }
        let mut second_paragraph = Paragraph::new(id);
        second_paragraph.props = props;
        second_paragraph.style = style;
        second_paragraph.numbering = numbering;
        second_paragraph.set_char_styles(&second);
        let (container, local) = self
            .document
            .paragraph_location(index)
            .ok_or_else(|| global_missing(index))?;
        self.document
            .insert_paragraph_after_location(container, local, second_paragraph)?;
        self.document.bump_revision();
        Ok(index + 1)
    }

    pub fn delete_across_global(
        &mut self,
        start_index: usize,
        start: usize,
        end_index: usize,
        end: usize,
    ) -> Result<()> {
        if start_index == end_index {
            return self.delete_range_global(start_index, start, end);
        }
        if start_index > end_index {
            return Err(QuireError::edit("selection is out of range"));
        }
        let start_len = self
            .paragraph_len_global(start_index)
            .ok_or_else(|| global_missing(start_index))?;
        let end_len = self
            .paragraph_len_global(end_index)
            .ok_or_else(|| global_missing(end_index))?;
        if start > start_len || end > end_len {
            return Err(QuireError::edit("selection is out of range"));
        }
        let (start_container, start_local) = self
            .document
            .paragraph_location(start_index)
            .ok_or_else(|| global_missing(start_index))?;
        let (end_container, end_local) = self
            .document
            .paragraph_location(end_index)
            .ok_or_else(|| global_missing(end_index))?;
        if start_container != end_container {
            return Err(QuireError::edit(
                "selection cannot span table cells",
            ));
        }
        self.checkpoint();
        let tail = {
            let target = self
                .document
                .paragraph_at_global(end_index)
                .ok_or_else(|| global_missing(end_index))?;
            target.char_styles()[end..].to_vec()
        };
        if let Some(target) = self.document.paragraph_at_global_mut(start_index) {
            let mut chars = target.char_styles();
            chars.truncate(start);
            chars.extend(tail);
            target.set_char_styles(&chars);
        }
        for local in (start_local + 1..=end_local).rev() {
            self.document
                .remove_paragraph_at_location(start_container, local)?;
        }
        self.document.bump_revision();
        Ok(())
    }

    /// Clear a selection even when it spans table cells.
    ///
    /// Text is removed from every touched paragraph, but table structure and
    /// paragraph boundaries are kept. This is what the editor uses when the
    /// user replaces an all-document selection that contains a table.
    pub fn clear_across_global(
        &mut self,
        start_index: usize,
        start: usize,
        end_index: usize,
        end: usize,
    ) -> Result<()> {
        if start_index == end_index {
            return self.delete_range_global(start_index, start, end);
        }
        if start_index > end_index {
            return Err(QuireError::edit("selection is out of range"));
        }
        let start_len = self
            .paragraph_len_global(start_index)
            .ok_or_else(|| global_missing(start_index))?;
        let end_len = self
            .paragraph_len_global(end_index)
            .ok_or_else(|| global_missing(end_index))?;
        if start > start_len || end > end_len {
            return Err(QuireError::edit("selection is out of range"));
        }
        let (start_container, _) = self
            .document
            .paragraph_location(start_index)
            .ok_or_else(|| global_missing(start_index))?;
        let (end_container, _) = self
            .document
            .paragraph_location(end_index)
            .ok_or_else(|| global_missing(end_index))?;
        if start_container == end_container {
            return self.delete_across_global(start_index, start, end_index, end);
        }

        self.checkpoint();
        if let Some(target) = self.document.paragraph_at_global_mut(start_index) {
            let mut chars = target.char_styles();
            chars.truncate(start);
            target.set_char_styles(&chars);
        }
        for index in start_index + 1..end_index {
            if let Some(target) = self.document.paragraph_at_global_mut(index) {
                target.set_char_styles(&[]);
            }
        }
        if let Some(target) = self.document.paragraph_at_global_mut(end_index) {
            let mut chars = target.char_styles();
            chars.drain(..end);
            target.set_char_styles(&chars);
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn set_paragraph_format_global(
        &mut self,
        start_index: usize,
        end_index: usize,
        apply: impl Fn(&mut quire_model::ParaProps),
    ) -> Result<()> {
        if start_index > end_index {
            return Err(QuireError::edit("selection is out of range"));
        }
        self.checkpoint();
        for index in start_index..=end_index {
            if let Some(paragraph) = self.document.paragraph_at_global_mut(index) {
                apply(&mut paragraph.props);
            }
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn apply_paragraph_style_global(
        &mut self,
        start_index: usize,
        end_index: usize,
        style_id: &str,
    ) -> Result<()> {
        let Some(style) = self.document.styles().get(style_id).cloned() else {
            return Err(QuireError::edit("style not found"));
        };
        if style.kind != quire_model::StyleKind::Paragraph {
            return Err(QuireError::edit("style is not a paragraph style"));
        }
        self.checkpoint();
        for index in start_index..=end_index {
            let Some(paragraph) = self.document.paragraph_at_global_mut(index) else {
                continue;
            };
            paragraph.style = Some(style.id.clone());
            if let Some(props) = &style.para {
                paragraph.props = props.clone();
            }
            if let Some(char_props) = &style.char {
                for run in &mut paragraph.runs {
                    run.props = char_props.clone();
                    run.style = None;
                }
            }
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn format_range_global(
        &mut self,
        start_index: usize,
        start: usize,
        end_index: usize,
        end: usize,
        apply: impl Fn(&mut CharProps),
    ) -> Result<()> {
        if start_index > end_index {
            return Err(QuireError::edit("selection is out of range"));
        }
        self.checkpoint();
        let mut touched = false;
        for index in start_index..=end_index {
            let Some(target) = self.document.paragraph_at_global_mut(index) else {
                continue;
            };
            let length = target.char_count();
            let from = if index == start_index { start } else { 0 }.min(length);
            let to = if index == end_index { end } else { length }.min(length);
            if from >= to {
                continue;
            }
            let mut chars = target.char_styles();
            for (_, props) in &mut chars[from..to] {
                apply(props);
            }
            target.set_char_styles(&chars);
            touched = true;
        }
        if touched {
            self.document.bump_revision();
        }
        Ok(())
    }

    pub fn set_alignment_global(
        &mut self,
        start_index: usize,
        end_index: usize,
        align: Align,
    ) -> Result<()> {
        if start_index > end_index {
            return Err(QuireError::edit("selection is out of range"));
        }
        self.checkpoint();
        for index in start_index..=end_index {
            if let Some(paragraph) = self.document.paragraph_at_global_mut(index) {
                paragraph.props.align = align;
            }
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn find_all_global(&self, query: &str, case_sensitive: bool) -> Vec<TextMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let needle: Vec<char> = query.chars().collect();
        let mut matches = Vec::new();
        for paragraph_index in 0..self.document.editable_paragraph_count() {
            let Some(paragraph) = self.document.paragraph_at_global(paragraph_index) else {
                continue;
            };
            let haystack: Vec<char> = paragraph.text().chars().collect();
            if needle.len() > haystack.len() {
                continue;
            }
            for start in 0..=haystack.len() - needle.len() {
                if chars_equal(
                    &haystack[start..start + needle.len()],
                    &needle,
                    case_sensitive,
                ) {
                    matches.push(TextMatch {
                        paragraph: paragraph_index,
                        start,
                        end: start + needle.len(),
                    });
                }
            }
        }
        matches
    }

    pub fn replace_match_global(
        &mut self,
        target: TextMatch,
        replacement: &str,
    ) -> Result<()> {
        self.checkpoint();
        self.replace_at_global(target, replacement)?;
        self.document.bump_revision();
        Ok(())
    }

    pub fn replace_all_global(
        &mut self,
        query: &str,
        replacement: &str,
        case_sensitive: bool,
    ) -> Result<usize> {
        let matches = self.find_all_global(query, case_sensitive);
        if matches.is_empty() {
            return Ok(0);
        }
        self.checkpoint();
        for target in matches.iter().rev().copied() {
            self.replace_at_global(target, replacement)?;
        }
        self.document.bump_revision();
        Ok(matches.len())
    }

    fn replace_at_global(&mut self, target: TextMatch, replacement: &str) -> Result<()> {
        let Some(paragraph) = self.document.paragraph_at_global_mut(target.paragraph) else {
            return Err(QuireError::edit("match paragraph not found"));
        };
        let mut chars = paragraph.char_styles();
        if target.start > target.end || target.end > chars.len() {
            return Err(QuireError::edit("match range is out of bounds"));
        }
        let style = chars[target.start].1.clone();
        chars.drain(target.start..target.end);
        for (offset, ch) in replacement.chars().enumerate() {
            chars.insert(target.start + offset, (ch, style.clone()));
        }
        paragraph.set_char_styles(&chars);
        Ok(())
    }

    // ----- table structure editing -----

    pub fn table_delete(&mut self, table: TableId) -> Result<()> {
        self.checkpoint();
        if !self.document.remove_table(table) {
            return Err(QuireError::edit("table not found"));
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn table_toggle_header_shading(&mut self, table: TableId) -> Result<()> {
        self.checkpoint();
        let table_ref = self
            .document
            .table_by_id_mut(table)
            .ok_or_else(|| QuireError::edit("table not found"))?;
        let shaded = table_ref
            .rows
            .first()
            .map(|row| row.cells.iter().any(|cell| cell.shading.is_some()))
            .unwrap_or(false);
        let shading = if shaded {
            None
        } else {
            Some(Color::rgb(0xF3, 0xEF, 0xE8))
        };
        if let Some(row) = table_ref.rows.first_mut() {
            for cell in &mut row.cells {
                cell.shading = shading;
            }
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn table_insert_row(&mut self, table: TableId, after_row: usize) -> Result<()> {
        let columns = self
            .document
            .table_by_id(table)
            .ok_or_else(|| QuireError::edit("table not found"))?
            .grid
            .len()
            .max(1);
        self.checkpoint();
        let mut row = TableRow::default();
        for _ in 0..columns {
            let id = self.document.next_para_id();
            let mut cell = TableCell::default();
            cell.blocks.push(Block::Paragraph(Paragraph::new(id)));
            row.cells.push(cell);
        }
        let table_mut = self
            .document
            .table_by_id_mut(table)
            .ok_or_else(|| QuireError::edit("table not found"))?;
        if table_mut.rows.is_empty() {
            table_mut.rows.push(row);
        } else {
            table_mut.rows.insert((after_row + 1).min(table_mut.rows.len()), row);
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn table_insert_row_before(&mut self, table: TableId, row: usize) -> Result<()> {
        let columns = self
            .document
            .table_by_id(table)
            .ok_or_else(|| QuireError::edit("table not found"))?
            .grid
            .len()
            .max(1);
        self.checkpoint();
        let mut new_row = TableRow::default();
        for _ in 0..columns {
            let id = self.document.next_para_id();
            let mut cell = TableCell::default();
            cell.blocks.push(Block::Paragraph(Paragraph::new(id)));
            new_row.cells.push(cell);
        }
        let table_mut = self
            .document
            .table_by_id_mut(table)
            .ok_or_else(|| QuireError::edit("table not found"))?;
        let index = row.min(table_mut.rows.len());
        table_mut.rows.insert(index, new_row);
        self.document.bump_revision();
        Ok(())
    }

    pub fn table_delete_row(&mut self, table: TableId, row: usize) -> Result<()> {
        let rows = self
            .document
            .table_by_id(table)
            .ok_or_else(|| QuireError::edit("table not found"))?
            .rows
            .len();
        if rows <= 1 {
            return Err(QuireError::edit("a table needs at least one row"));
        }
        if row >= rows {
            return Err(QuireError::edit("row is out of range"));
        }
        self.checkpoint();
        if let Some(table_mut) = self.document.table_by_id_mut(table) {
            table_mut.rows.remove(row);
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn table_insert_column(
        &mut self,
        table: TableId,
        after_column: usize,
    ) -> Result<()> {
        let (rows, columns, width) = {
            let table_ref = self
                .document
                .table_by_id(table)
                .ok_or_else(|| QuireError::edit("table not found"))?;
            if table_ref.grid.is_empty() {
                return Err(QuireError::edit("table has no columns"));
            }
            (
                table_ref.rows.len(),
                table_ref.grid.len(),
                table_ref
                    .grid
                    .get(after_column.min(table_ref.grid.len() - 1))
                    .copied()
                    .unwrap_or(120.0),
            )
        };
        if after_column >= columns {
            return Err(QuireError::edit("column is out of range"));
        }
        self.checkpoint();
        let ids: Vec<_> = (0..rows)
            .map(|_| self.document.next_para_id())
            .collect();
        let table_mut = self
            .document
            .table_by_id_mut(table)
            .ok_or_else(|| QuireError::edit("table not found"))?;
        table_mut.grid.insert(after_column + 1, width);
        for (row_index, row) in table_mut.rows.iter_mut().enumerate() {
            let id = ids[row_index];
            let mut cell = TableCell::default();
            cell.blocks.push(Block::Paragraph(Paragraph::new(id)));
            let index = (after_column + 1).min(row.cells.len());
            row.cells.insert(index, cell);
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn table_insert_column_before(
        &mut self,
        table: TableId,
        column: usize,
    ) -> Result<()> {
        let (rows, columns, width) = {
            let table_ref = self
                .document
                .table_by_id(table)
                .ok_or_else(|| QuireError::edit("table not found"))?;
            if table_ref.grid.is_empty() {
                return Err(QuireError::edit("table has no columns"));
            }
            (
                table_ref.rows.len(),
                table_ref.grid.len(),
                table_ref
                    .grid
                    .get(column.min(table_ref.grid.len() - 1))
                    .copied()
                    .unwrap_or(120.0),
            )
        };
        if column > columns {
            return Err(QuireError::edit("column is out of range"));
        }
        self.checkpoint();
        let ids: Vec<_> = (0..rows)
            .map(|_| self.document.next_para_id())
            .collect();
        let table_mut = self
            .document
            .table_by_id_mut(table)
            .ok_or_else(|| QuireError::edit("table not found"))?;
        table_mut.grid.insert(column, width);
        for (row_index, row) in table_mut.rows.iter_mut().enumerate() {
            let id = ids[row_index];
            let mut cell = TableCell::default();
            cell.blocks.push(Block::Paragraph(Paragraph::new(id)));
            let index = column.min(row.cells.len());
            row.cells.insert(index, cell);
        }
        self.document.bump_revision();
        Ok(())
    }

    pub fn table_delete_column(
        &mut self,
        table: TableId,
        column: usize,
    ) -> Result<()> {
        let columns = self
            .document
            .table_by_id(table)
            .ok_or_else(|| QuireError::edit("table not found"))?
            .grid
            .len();
        if columns <= 1 {
            return Err(QuireError::edit("a table needs at least one column"));
        }
        if column >= columns {
            return Err(QuireError::edit("column is out of range"));
        }
        self.checkpoint();
        if let Some(table_mut) = self.document.table_by_id_mut(table) {
            table_mut.grid.remove(column);
            for row in &mut table_mut.rows {
                if column < row.cells.len() {
                    row.cells.remove(column);
                }
            }
        }
        self.document.bump_revision();
        Ok(())
    }

    /// Insert an empty table after a top-level paragraph.
    pub fn insert_table(
        &mut self,
        section: usize,
        paragraph: usize,
        rows: usize,
        columns: usize,
    ) -> Result<TableId> {
        if rows == 0 || columns == 0 {
            return Err(QuireError::edit("table dimensions must be positive"));
        }
        self.checkpoint();
        let mut table = Table::new(self.document.next_table_id());
        let content_width = self
            .document
            .sections()
            .get(section)
            .map(|section| {
                section.props.page.width_pt
                    - section.props.margins.left_pt
                    - section.props.margins.right_pt
            })
            .unwrap_or(450.0);
        let column_width = (content_width / columns as f64).max(24.0);
        table.grid = vec![column_width; columns];
        let border = Border::new(0.5, Color::rgb(0x80, 0x80, 0x80));
        table.borders = TableBorders {
            top: Some(border),
            right: Some(border),
            bottom: Some(border),
            left: Some(border),
            inside_horizontal: Some(border),
            inside_vertical: Some(border),
        };
        for _ in 0..rows {
            let mut row = TableRow::default();
            for _ in 0..columns {
                let id = self.document.next_para_id();
                let mut cell = TableCell::default();
                cell.blocks.push(Block::Paragraph(Paragraph::new(id)));
                row.cells.push(cell);
            }
            table.rows.push(row);
        }
        let table_id = table.id;
        self.document
            .insert_block_after_paragraph(section, paragraph, Block::Table(table))?;
        self.document.bump_revision();
        Ok(table_id)
    }

    /// Insert an image block after a top-level paragraph.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_image(
        &mut self,
        section: usize,
        paragraph: usize,
        name: &str,
        mime: &str,
        data: Vec<u8>,
        width_pt: f64,
        height_pt: f64,
    ) -> Result<()> {
        self.checkpoint();
        let image = ImageBlock::new(
            self.document.next_image_id(),
            name,
            mime,
            data,
            width_pt.max(1.0),
            height_pt.max(1.0),
        );
        self.document
            .insert_block_after_paragraph(section, paragraph, Block::Image(image))?;
        self.document.bump_revision();
        Ok(())
    }

    /// Undo the last transaction. Returns `false` when history is empty.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.document, previous);
        self.redo.push(current);
        true
    }

    /// Redo the last undone transaction. Returns `false` when there is nothing
    /// to redo.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.document, next);
        self.undo.push(current);
        true
    }

    fn checkpoint(&mut self) {
        self.redo.clear();
        self.undo.push(self.document.clone());
        if self.undo.len() > self.history_limit {
            self.undo.remove(0);
        }
    }
}

fn chars_equal(left: &[char], right: &[char], case_sensitive: bool) -> bool {
    if case_sensitive {
        return left == right;
    }
    left.iter().zip(right).all(|(left, right)| {
        left.to_lowercase().collect::<String>() == right.to_lowercase().collect::<String>()
    })
}

fn global_missing(index: usize) -> QuireError {
    QuireError::edit(format!("no editable paragraph at index {index}"))
}

fn missing(section: usize, paragraph: usize) -> QuireError {
    QuireError::edit(format!("no paragraph {paragraph} in section {section}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use quire_model::Paragraph;

    fn sample() -> Editor {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Hello world"));
        Editor::new(doc)
    }

    #[test]
    fn inserts_and_positions_caret() {
        let mut editor = sample();
        let caret = editor.insert_text(0, 0, 5, ",").unwrap();
        assert_eq!(caret, 6);
        assert_eq!(editor.document().paragraph(0, 0).unwrap().text(), "Hello, world");
    }

    #[test]
    fn deletes_a_range() {
        let mut editor = sample();
        editor.delete_range(0, 0, 5, 11).unwrap();
        assert_eq!(editor.document().paragraph(0, 0).unwrap().text(), "Hello");
    }

    #[test]
    fn undo_and_redo_restore_content() {
        let mut editor = sample();
        editor.insert_text(0, 0, 11, "!").unwrap();
        assert_eq!(
            editor.document().paragraph(0, 0).unwrap().text(),
            "Hello world!"
        );

        assert!(editor.undo());
        assert_eq!(editor.document().paragraph(0, 0).unwrap().text(), "Hello world");

        assert!(editor.redo());
        assert_eq!(
            editor.document().paragraph(0, 0).unwrap().text(),
            "Hello world!"
        );
    }

    #[test]
    fn rejects_out_of_range_offsets() {
        let mut editor = sample();
        assert!(editor.insert_text(0, 0, 99, "x").is_err());
        assert!(editor.delete_range(0, 0, 5, 99).is_err());
        assert!(editor.insert_text(0, 9, 0, "x").is_err());
    }

    #[test]
    fn undo_is_bounded() {
        let mut editor = sample();
        for _ in 0..200 {
            editor.insert_text(0, 0, 0, "x").unwrap();
        }
        let mut count = 0;
        while editor.undo() {
            count += 1;
        }
        assert_eq!(count, 128);
    }

    #[test]
    fn splits_and_merges_paragraphs() {
        let mut editor = sample();
        let new_index = editor.split_paragraph(0, 0, 5).unwrap();
        assert_eq!(new_index, 1);
        assert_eq!(editor.document().paragraph(0, 0).unwrap().text(), "Hello");
        assert_eq!(
            editor.document().paragraph(0, 1).unwrap().text(),
            " world"
        );

        editor.delete_across(0, 0, 5, 1, 0).unwrap();
        assert_eq!(editor.document().paragraph_count(), 1);
        assert_eq!(
            editor.document().paragraph(0, 0).unwrap().text(),
            "Hello world"
        );

        assert!(editor.undo());
        assert_eq!(editor.document().paragraph_count(), 2);
        assert_eq!(editor.document().paragraph(0, 1).unwrap().text(), " world");
    }

    #[test]
    fn deletes_a_multi_paragraph_selection() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Alpha"));
        doc.push_paragraph(Paragraph::plain("Beta"));
        doc.push_paragraph(Paragraph::plain("Gamma"));
        let mut editor = Editor::new(doc);

        editor.delete_across(0, 0, 3, 2, 2).unwrap();
        assert_eq!(editor.document().paragraph_count(), 1);
        assert_eq!(
            editor.document().paragraph(0, 0).unwrap().text(),
            "Alpmma"
        );
    }


    #[test]
    fn formats_a_multi_paragraph_selection() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Hello"));
        doc.push_paragraph(Paragraph::plain("World"));
        let mut editor = Editor::new(doc);

        editor
            .format_range(0, 0, 1, 1, 3, |props| props.bold = true)
            .unwrap();
        assert!(!editor.document().paragraph(0, 0).unwrap().style_at(0).bold);
        assert!(editor.document().paragraph(0, 0).unwrap().style_at(1).bold);
        assert!(editor.document().paragraph(0, 1).unwrap().style_at(2).bold);
        assert!(!editor.document().paragraph(0, 1).unwrap().style_at(3).bold);

        assert!(editor.undo());
        assert!(!editor.document().paragraph(0, 0).unwrap().style_at(1).bold);
    }

    #[test]
    fn sets_alignment_for_a_paragraph_range() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("One"));
        doc.push_paragraph(Paragraph::plain("Two"));
        let mut editor = Editor::new(doc);

        editor.set_alignment(0, 0, 1, Align::Center).unwrap();
        assert_eq!(editor.document().paragraph(0, 0).unwrap().props.align, Align::Center);
        assert_eq!(editor.document().paragraph(0, 1).unwrap().props.align, Align::Center);
    }


    #[test]
    fn applies_paragraph_formatting_and_styles() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("One"));
        doc.push_paragraph(Paragraph::plain("Two"));
        let mut style = quire_model::Style::new(
            "Body",
            "Body",
            quire_model::StyleKind::Paragraph,
        );
        style.para = Some(quire_model::ParaProps {
            align: Align::Center,
            indent_left_pt: 24.0,
            ..quire_model::ParaProps::default()
        });
        style.char = Some(CharProps {
            size_pt: 14.0,
            ..CharProps::default()
        });
        doc.styles_mut().insert(style);
        let mut editor = Editor::new(doc);

        editor
            .set_paragraph_format(0, 0, 1, |props| props.space_after_pt = 6.0)
            .unwrap();
        assert_eq!(
            editor.document().paragraph(0, 1).unwrap().props.space_after_pt,
            6.0
        );

        editor.apply_paragraph_style(0, 0, 1, "Body").unwrap();
        let paragraph = editor.document().paragraph(0, 0).unwrap();
        assert_eq!(paragraph.style.as_deref(), Some("Body"));
        assert_eq!(paragraph.props.align, Align::Center);
        assert_eq!(paragraph.runs[0].props.size_pt, 14.0);
    }


    #[test]
    fn finds_and_replaces_text() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Alpha alpha"));
        doc.push_paragraph(Paragraph::plain("Beta"));
        let mut editor = Editor::new(doc);

        let matches = editor.find_all(0, "alpha", false);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].start, 0);
        assert_eq!(matches[1].start, 6);

        editor
            .replace_match(0, matches[0], "Omega")
            .unwrap();
        assert_eq!(
            editor.document().paragraph(0, 0).unwrap().text(),
            "Omega alpha"
        );
        assert!(editor.undo());
        assert_eq!(
            editor.document().paragraph(0, 0).unwrap().text(),
            "Alpha alpha"
        );

        let count = editor.replace_all(0, "alpha", "X", false).unwrap();
        assert_eq!(count, 2);
        assert_eq!(editor.document().paragraph(0, 0).unwrap().text(), "X X");
        assert!(editor.undo());
        assert_eq!(
            editor.document().paragraph(0, 0).unwrap().text(),
            "Alpha alpha"
        );
    }


    #[test]
    fn inserts_tables_and_images() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Body"));
        let mut editor = Editor::new(doc);

        editor.insert_table(0, 0, 2, 3).unwrap();
        editor
            .insert_image(0, 0, "pixel.png", "image/png", vec![1, 2, 3], 72.0, 72.0)
            .unwrap();
        assert_eq!(editor.document().block_count(), 3);
        assert_eq!(editor.document().first_section().table_count(), 1);
        assert!(editor.undo());
        assert_eq!(editor.document().block_count(), 2);
    }


    #[test]
    fn edits_paragraphs_inside_table_cells() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Body"));
        let mut editor = Editor::new(doc);
        let table = editor.insert_table(0, 0, 2, 2).unwrap();

        assert_eq!(editor.document().editable_paragraph_count(), 5);
        assert_eq!(
            editor.document().paragraph_at_global(1).unwrap().text(),
            ""
        );
        editor.insert_text_global(1, 0, "Cell A").unwrap();
        assert_eq!(
            editor.document().paragraph_at_global(1).unwrap().text(),
            "Cell A"
        );

        let split = editor.split_global(1, 6).unwrap();
        assert_eq!(split, 2);
        assert_eq!(editor.document().editable_paragraph_count(), 6);
        editor.delete_across_global(1, 6, 2, 0).unwrap();
        assert_eq!(editor.document().editable_paragraph_count(), 5);

        assert_eq!(editor.find_all_global("cell", false).len(), 1);
        editor
            .replace_all_global("cell", "Grid", false)
            .unwrap();
        assert_eq!(
            editor.document().paragraph_at_global(1).unwrap().text(),
            "Grid A"
        );

        let (container, _) = editor
            .document()
            .paragraph_location(1)
            .expect("cell paragraph");
        assert!(matches!(
            container,
            quire_model::ContainerId::TableCell {
                table: found,
                row: 0,
                cell: 0,
            } if found == table
        ));

        editor.table_insert_row(table, 1).unwrap();
        assert_eq!(editor.document().editable_paragraph_count(), 7);
        editor.table_insert_column(table, 1).unwrap();
        assert_eq!(editor.document().editable_paragraph_count(), 10);
        editor.table_delete_column(table, 2).unwrap();
        assert_eq!(editor.document().editable_paragraph_count(), 7);
        editor.table_delete_row(table, 2).unwrap();
        assert_eq!(editor.document().editable_paragraph_count(), 5);

        assert!(editor.undo());
        assert_eq!(editor.document().editable_paragraph_count(), 7);
    }

    #[test]
    fn rejects_deletion_across_cells() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Body"));
        let mut editor = Editor::new(doc);
        editor.insert_table(0, 0, 1, 2).unwrap();
        editor.insert_text_global(1, 0, "A").unwrap();
        editor.insert_text_global(2, 0, "B").unwrap();
        assert!(editor.delete_across_global(1, 1, 2, 0).is_err());
        assert_eq!(
            editor.document().paragraph_at_global(1).unwrap().text(),
            "A"
        );
        assert_eq!(
            editor.document().paragraph_at_global(2).unwrap().text(),
            "B"
        );
    }



    #[test]
    fn clears_selection_across_cells_without_removing_table() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Body"));
        let mut editor = Editor::new(doc);
        editor.insert_table(0, 0, 1, 2).unwrap();
        editor.insert_text_global(1, 0, "A").unwrap();
        editor.insert_text_global(2, 0, "B").unwrap();

        editor.clear_across_global(1, 0, 2, 0).unwrap();
        assert_eq!(editor.document().editable_paragraph_count(), 3);
        assert_eq!(
            editor.document().paragraph_at_global(1).unwrap().text(),
            ""
        );
        assert_eq!(
            editor.document().paragraph_at_global(2).unwrap().text(),
            "B"
        );
        assert_eq!(editor.document().first_section().table_count(), 1);
    }


    #[test]
    fn inserts_rows_and_columns_before() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Body"));
        let mut editor = Editor::new(doc);
        let table = editor.insert_table(0, 0, 2, 2).unwrap();
        let original = editor
            .document()
            .paragraph_at_global(1)
            .unwrap()
            .id;

        editor.table_insert_row_before(table, 0).unwrap();
        assert_eq!(
            editor.document().table_by_id(table).unwrap().rows.len(),
            3
        );
        let index = editor
            .document()
            .global_index_of_paragraph(original)
            .unwrap();
        assert!(matches!(
            editor.document().paragraph_location(index).unwrap().0,
            quire_model::ContainerId::TableCell {
                row: 1,
                cell: 0,
                ..
            }
        ));

        editor.table_insert_column_before(table, 0).unwrap();
        assert_eq!(
            editor.document().table_by_id(table).unwrap().grid.len(),
            3
        );
        let index = editor
            .document()
            .global_index_of_paragraph(original)
            .unwrap();
        assert!(matches!(
            editor.document().paragraph_location(index).unwrap().0,
            quire_model::ContainerId::TableCell {
                row: 1,
                cell: 1,
                ..
            }
        ));
    }


    #[test]
    fn toggles_table_header_and_deletes_table() {
        let mut doc = Document::new();
        doc.push_paragraph(Paragraph::plain("Body"));
        let mut editor = Editor::new(doc);
        let table = editor.insert_table(0, 0, 2, 2).unwrap();
        editor.table_toggle_header_shading(table).unwrap();
        assert!(editor
            .document()
            .table_by_id(table)
            .unwrap()
            .rows[0]
            .cells[0]
            .shading
            .is_some());
        editor.table_toggle_header_shading(table).unwrap();
        assert!(editor
            .document()
            .table_by_id(table)
            .unwrap()
            .rows[0]
            .cells[0]
            .shading
            .is_none());
        editor.table_delete(table).unwrap();
        assert!(editor.document().table_by_id(table).is_none());
        assert!(editor.undo());
        assert!(editor.document().table_by_id(table).is_some());
    }

}

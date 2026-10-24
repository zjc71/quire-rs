//! CSV/TSV import as a document table.

use quire_base::{QuireError, Result};
use quire_model::{
    Block, Border, CharProps, Color, Document, Paragraph, Section, Table, TableBorders, TableCell,
    TableRow,
};

use crate::sheet::SheetGrid;
use crate::text;

const MAX_ROWS: usize = 2000;
const MAX_COLUMNS: usize = 64;

pub(crate) fn parse_rows(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
        } else if ch == '"' && field.is_empty() {
            quoted = true;
        } else if ch == delimiter {
            row.push(std::mem::take(&mut field));
        } else if ch == '\n' {
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
        } else if ch != '\r' {
            field.push(ch);
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

pub(crate) fn read_grid(bytes: &[u8], name: &str) -> Result<SheetGrid> {
    let text = text::decode(bytes)?;
    let rows: Vec<Vec<String>> = parse_rows(&text, ',')
        .into_iter()
        .take(MAX_ROWS)
        .map(|row| row.into_iter().take(MAX_COLUMNS).collect())
        .collect();
    Ok(SheetGrid {
        name: name.to_string(),
        rows,
    })
}

pub(crate) fn write_grid(sheet: &SheetGrid) -> Result<Vec<u8>> {
    fn quote(value: &str) -> String {
        if value.contains(',')
            || value.contains('"')
            || value.contains('\n')
            || value.contains('\r')
            || value.starts_with(' ')
            || value.ends_with(' ')
        {
            let mut out = String::with_capacity(value.len() + 2);
            out.push('"');
            for ch in value.chars() {
                if ch == '"' {
                    out.push('"');
                }
                out.push(ch);
            }
            out.push('"');
            out
        } else {
            value.to_string()
        }
    }

    let mut out = String::from("\u{feff}");
    for row_index in 0..sheet.row_count() {
        let line = (0..sheet.column_count())
            .map(|column| quote(&sheet.display_cell(row_index, column)))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out.into_bytes())
}

/// Read CSV or TSV into a one-table document.
pub fn read(bytes: &[u8], delimiter: char) -> Result<Document> {
    let text = text::decode(bytes)?;
    let rows = parse_rows(&text, delimiter);
    if rows.is_empty() {
        return Err(QuireError::format("csv has no rows"));
    }
    let row_count = rows.len().min(MAX_ROWS);
    let column_count = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .min(MAX_COLUMNS);
    if column_count == 0 {
        return Err(QuireError::format("csv has no columns"));
    }

    let mut document = Document::new();
    document.clear_sections();
    let mut section = Section::default();

    let mut heading = Paragraph::plain("CSV");
    if let Some(run) = heading.runs.first_mut() {
        run.props = CharProps {
            size_pt: 14.0,
            bold: true,
            ..CharProps::default()
        };
    }
    section.push_paragraph_block(heading);

    let border = Border::new(0.5, Color::rgb(0x80, 0x80, 0x80));
    let mut table = Table::new(quire_base::TableId::new(0));
    table.grid = vec![90.0; column_count];
    table.borders = TableBorders {
        top: Some(border),
        right: Some(border),
        bottom: Some(border),
        left: Some(border),
        inside_horizontal: Some(border),
        inside_vertical: Some(border),
    };
    for row in rows.iter().take(row_count) {
        let mut table_row = TableRow::default();
        for column in 0..column_count {
            let value = row.get(column).cloned().unwrap_or_default();
            let mut cell = TableCell::default();
            cell.blocks.push(Block::Paragraph(Paragraph::plain(value)));
            table_row.cells.push(cell);
        }
        table.rows.push(table_row);
    }
    section.push_table(table);
    document.push_section(section);
    document.renumber();
    document.validate()?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_csv() {
        let rows = parse_rows("a,\"b,c\"\n1,2\n", ',');
        assert_eq!(rows[0], vec!["a", "b,c"]);
        assert_eq!(rows[1], vec!["1", "2"]);
    }

    #[test]
    fn imports_as_table() {
        let doc = read(b"a,b\n1,2\n", ',').unwrap();
        assert_eq!(doc.first_section().table_count(), 1);
    }

    #[test]
    fn grid_round_trips_quoted_fields() {
        let sheet = SheetGrid {
            name: "Data".to_string(),
            rows: vec![
                vec!["Name".to_string(), "Note".to_string()],
                vec!["Pen".to_string(), "a,b".to_string()],
            ],
        };
        let bytes = write_grid(&sheet).unwrap();
        let read = read_grid(&bytes, "Data").unwrap();
        assert_eq!(read.rows[1][1], "a,b");
    }


    #[test]
    fn writes_formula_results_to_csv() {
        let sheet = SheetGrid {
            name: "Data".to_string(),
            rows: vec![vec![
                "1".to_string(),
                "2".to_string(),
                "=A1+B1".to_string(),
            ]],
        };
        let bytes = write_grid(&sheet).unwrap();
        let read = read_grid(&bytes, "Data").unwrap();
        assert_eq!(read.cell(0, 2), "3");
    }

}

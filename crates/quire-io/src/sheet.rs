//! Editable worksheet grids for the spreadsheet view.
//!
//! The document model is intentionally not used here: spreadsheets are a
//! different editing surface. These helpers expose cached cell values only.

use std::fs;
use std::path::Path;

use quire_base::{QuireError, Result};

use crate::formats::Format;
use crate::{csv, xlsx};

/// One worksheet or CSV table as a rectangular value grid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SheetGrid {
    pub name: String,
    pub rows: Vec<Vec<String>>,
}

impl SheetGrid {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn column_count(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Value shown in the grid, with formulas evaluated.
    pub fn display_cell(&self, row: usize, column: usize) -> String {
        crate::formula::display_cell(self, row, column)
    }

    pub fn cell(&self, row: usize, column: usize) -> &str {
        self.rows
            .get(row)
            .and_then(|row| row.get(column))
            .map(String::as_str)
            .unwrap_or("")
    }
}

/// Read a workbook or CSV file into editable sheets.
pub fn read_sheets(path: impl AsRef<Path>) -> Result<Vec<SheetGrid>> {
    let path = path.as_ref();
    let bytes = fs::read(path)?;
    match Format::from_path(path) {
        Format::Xlsx => xlsx::read_grids(&bytes),
        Format::Csv => {
            let name = path
                .file_stem()
                .map(|value| value.to_string_lossy().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "Sheet1".to_string());
            Ok(vec![csv::read_grid(&bytes, &name)?])
        }
        _ => Err(QuireError::unsupported(
            "spreadsheet view only supports XLSX and CSV",
        )),
    }
}

/// Write editable sheets back to XLSX or CSV.
pub fn write_sheets(path: impl AsRef<Path>, sheets: &[SheetGrid]) -> Result<()> {
    let path = path.as_ref();
    let bytes = match Format::from_path(path) {
        Format::Xlsx => xlsx::write_grids(sheets)?,
        Format::Csv => {
            let sheet = sheets
                .first()
                .ok_or_else(|| QuireError::format("there is no sheet to write"))?;
            csv::write_grid(sheet)?
        }
        _ => {
            return Err(QuireError::unsupported(
                "spreadsheet view only supports XLSX and CSV",
            ));
        }
    };
    fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_csv_grids() {
        let dir = std::env::temp_dir().join(format!(
            "quire-sheet-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.csv");
        fs::write(&path, "Name,Qty\nPen,12\n").unwrap();
        let sheets = read_sheets(&path).unwrap();
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].cell(0, 0), "Name");
        assert_eq!(sheets[0].cell(1, 1), "12");

        let out = dir.join("out.csv");
        write_sheets(&out, &sheets).unwrap();
        let read = read_sheets(&out).unwrap();
        assert_eq!(read[0].rows, sheets[0].rows);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_formula_results_as_values() {
        let dir = std::env::temp_dir().join(format!(
            "quire-formula-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("formula.xlsx");
        let sheets = vec![SheetGrid {
            name: "Data".to_string(),
            rows: vec![vec![
                "1".to_string(),
                "2".to_string(),
                "=SUM(A1:B1)".to_string(),
            ]],
        }];
        write_sheets(&path, &sheets).unwrap();
        let read = read_sheets(&path).unwrap();
        assert_eq!(read[0].cell(0, 2), "3");
        let _ = fs::remove_dir_all(&dir);
    }

}

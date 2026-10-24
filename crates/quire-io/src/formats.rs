//! Format detection and dispatch.

use std::io::Cursor;
use std::path::Path;

use quire_base::{QuireError, Result};
use quire_model::Document;

use crate::{docx_bytes_force, odt, rtf, text, write_docx};

/// A document container or import format.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Format {
    Docx,
    Txt,
    Rtf,
    Odt,
    Xlsx,
    Csv,
    Doc,
    Xls,
    Pdf,
    Unknown,
}

impl Format {
    pub fn from_path(path: &Path) -> Self {
        let extension = path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match extension.as_str() {
            "docx" | "docm" | "dotx" | "dotm" => Self::Docx,
            "txt" | "text" | "md" | "log" => Self::Txt,
            "rtf" => Self::Rtf,
            "odt" => Self::Odt,
            "xlsx" | "xlsm" | "xltx" | "xltm" => Self::Xlsx,
            "csv" => Self::Csv,
            "doc" | "dot" => Self::Doc,
            "xls" | "xlt" => Self::Xls,
            "pdf" => Self::Pdf,
            _ => Self::Unknown,
        }
    }

    fn from_magic(bytes: &[u8]) -> Self {
        if bytes.starts_with(b"{\\rtf") {
            return Self::Rtf;
        }
        if bytes.starts_with(b"PK\x03\x04") {
            if let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes)) {
                if archive.by_name("word/document.xml").is_ok() {
                    return Self::Docx;
                }
                if archive.by_name("content.xml").is_ok() {
                    return Self::Odt;
                }
                if archive.by_name("xl/workbook.xml").is_ok() {
                    return Self::Xlsx;
                }
            }
        }
        if bytes.starts_with(b"%PDF") {
            return Self::Pdf;
        }
        Self::Txt
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Txt => "txt",
            Self::Rtf => "rtf",
            Self::Odt => "odt",
            Self::Xlsx => "xlsx",
            Self::Csv => "csv",
            Self::Doc => "doc",
            Self::Xls => "xls",
            Self::Pdf => "pdf",
            Self::Unknown => "bin",
        }
    }
}

/// Read any supported document.
pub fn read_document(path: &Path) -> Result<Document> {
    let bytes = std::fs::read(path)?;
    let format = match Format::from_path(path) {
        Format::Unknown => Format::from_magic(&bytes),
        format => format,
    };
    read_bytes(&bytes, format)
}

/// Read a document from bytes with an optional path hint.
pub fn read_document_bytes(bytes: &[u8], hint: Option<&Path>) -> Result<Document> {
    let format = hint
        .map(Format::from_path)
        .filter(|format| *format != Format::Unknown)
        .unwrap_or_else(|| Format::from_magic(bytes));
    read_bytes(bytes, format)
}

fn read_bytes(bytes: &[u8], format: Format) -> Result<Document> {
    match format {
        Format::Docx => crate::read_docx_bytes(bytes),
        Format::Txt => text::read(bytes),
        Format::Rtf => rtf::read(bytes),
        Format::Odt => odt::read(bytes),
        Format::Xlsx => crate::xlsx::read(bytes),
        Format::Csv => crate::csv::read(bytes, ','),
        Format::Doc => Err(QuireError::unsupported(
            "旧版 .doc 暂不支持，请先用其他程序另存为 .docx",
        )),
        Format::Xls => Err(QuireError::unsupported(
            "旧版 .xls 暂不支持，请另存为 .xlsx 或 .csv",
        )),
        Format::Pdf => Err(QuireError::unsupported(
            "PDF 是版式输出格式，暂不支持直接编辑",
        )),
        Format::Unknown => Err(QuireError::format("无法识别文档格式")),
    }
}

/// Write a document using the target path's extension.
pub fn write_document(path: &Path, document: &Document) -> Result<()> {
    write_document_with_policy(path, document, false)
}

/// Write with an explicit lossy policy (diagnostic use).
pub fn write_document_with_policy(
    path: &Path,
    document: &Document,
    allow_lossy: bool,
) -> Result<()> {
    let format = Format::from_path(path);
    if !allow_lossy && !document.fidelity().is_lossless() {
        return Err(QuireError::unsupported(format!(
            "document uses content that cannot be written back yet: {}",
            document.fidelity().summary()
        )));
    }
    match format {
        Format::Docx => {
            if allow_lossy {
                let bytes = docx_bytes_force(document)?;
                std::fs::write(path, bytes)?;
                Ok(())
            } else {
                write_docx(path, document)
            }
        }
        Format::Txt => {
            std::fs::write(path, text::write(document))?;
            Ok(())
        }
        Format::Rtf => {
            std::fs::write(path, rtf::write(document))?;
            Ok(())
        }
        Format::Odt => {
            if allow_lossy {
                let bytes = crate::odt_bytes_force(document)?;
                std::fs::write(path, bytes)?;
                Ok(())
            } else {
                crate::write_odt(path, document)
            }
        }
        Format::Xlsx => Err(QuireError::unsupported("暂不支持导出 XLSX")),
        Format::Csv => Err(QuireError::unsupported("暂不支持导出 CSV")),
        Format::Doc => Err(QuireError::unsupported("暂不支持导出 .doc")),
        Format::Xls => Err(QuireError::unsupported("暂不支持导出 .xls")),
        Format::Pdf => Err(QuireError::unsupported("暂不支持导出 PDF")),
        Format::Unknown => Err(QuireError::unsupported(
            "无法根据文件扩展名选择导出格式",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quire_model::Paragraph;

    #[test]
    fn detects_and_round_trips_text() {
        let mut document = Document::new();
        document.push_paragraph(Paragraph::plain("你好"));
        let dir = std::env::temp_dir();
        let path = dir.join(format!("quire-format-{}.txt", std::process::id()));
        write_document(&path, &document).unwrap();
        let read = read_document(&path).unwrap();
        assert_eq!(read.paragraph(0, 0).unwrap().text(), "你好");
        let _ = std::fs::remove_file(path);
    }
}

//! Quire command line interface.
//!
//! The CLI is intentionally thin: it wires the library crates together and is
//! also the entry point used by golden tests and benchmarks. There is no
//! network access, no account system and no telemetry.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use quire_edit::Editor;
use quire_io::{read_document, write_document, write_document_with_policy};
use quire_layout::{layout, LayoutOptions, ViewMode};
use quire_model::{CharProps, Document, Paragraph};
use quire_paint::{render_svg, SvgOptions};
use quire_text::{DeterministicMetrics, SystemMetrics};

#[derive(Parser)]
#[command(
    name = "quire",
    version,
    about = "Quire - a clean-room document engine written in Rust"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Lay out a DOCX and write an SVG preview.
    Render {
        input: PathBuf,
        #[arg(short, long, default_value = "out.svg")]
        output: PathBuf,
        #[arg(long, value_enum, default_value_t = View::Page)]
        view: View,
        /// Viewport width in points for web view.
        #[arg(long)]
        width: Option<f64>,
        /// Measure with installed system fonts instead of synthetic metrics.
        #[arg(long)]
        system_fonts: bool,
        /// Draw line boxes for debugging.
        #[arg(long)]
        debug_boxes: bool,
    },
    /// Print the plain text of a DOCX.
    Text { input: PathBuf },
    /// Print document statistics.
    Info { input: PathBuf },
    /// Create a DOCX from text; newlines separate paragraphs.
    New {
        output: PathBuf,
        /// Text to write; newlines separate paragraphs. Omit for a sample.
        #[arg(long)]
        text: Option<String>,
    },
    /// Read and rewrite a DOCX through the model (round-trip check).
    Rewrite {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Write even when unsupported content would be lost.
        #[arg(long)]
        force: bool,
    },
    /// Insert text into a paragraph and save the result.
    Insert {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value_t = 0)]
        paragraph: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long)]
        text: String,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum View {
    Page,
    Web,
}

impl From<View> for ViewMode {
    fn from(view: View) -> Self {
        match view {
            View::Page => ViewMode::Page,
            View::Web => ViewMode::Web,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Render {
            input,
            output,
            view,
            width,
            system_fonts,
            debug_boxes,
        } => {
            let document = read_document(&input)
                .with_context(|| format!("cannot read {}", input.display()))?;
            let options = LayoutOptions {
                view: view.into(),
                viewport_width_pt: width,
            };
            let layout = if system_fonts {
                layout(&document, &options, &SystemMetrics::new())
            } else {
                layout(&document, &options, &DeterministicMetrics)
            }
            .context("layout failed")?;
            let svg = render_svg(
                &layout,
                &SvgOptions {
                    debug_boxes,
                    fit_text_to_layout: !system_fonts,
                    ..SvgOptions::default()
                },
            );
            fs::write(&output, svg)
                .with_context(|| format!("cannot write {}", output.display()))?;
            println!(
                "rendered {} page(s), {} line(s) -> {}",
                layout.page_count(),
                layout.line_count(),
                output.display()
            );
        }
        Command::Text { input } => {
            let document = read_document(&input)
                .with_context(|| format!("cannot read {}", input.display()))?;
            print!("{}", document.plain_text());
        }
        Command::Info { input } => {
            let document = read_document(&input)
                .with_context(|| format!("cannot read {}", input.display()))?;
            let section = document.first_section();
            println!("file      {}", input.display());
            println!("format    {:?}", quire_io::Format::from_path(&input));
            println!("revision  {}", document.revision());
            println!(
                "page      {:.1} x {:.1} pt",
                section.props.page.width_pt, section.props.page.height_pt
            );
            println!(
                "margins   top {:.1}, bottom {:.1}, left {:.1}, right {:.1} pt",
                section.props.margins.top_pt,
                section.props.margins.bottom_pt,
                section.props.margins.left_pt,
                section.props.margins.right_pt
            );
            println!("blocks     {}", document.block_count());
            println!("tables     {}", blocks_table_count(&document));
            println!("paragraphs {}", document.total_paragraph_count());
            println!("characters {}", document.char_count());
            println!("styles     {}", document.styles().len());
            println!("lists      {}", document.numbering().instances.len());
            if document.fidelity().is_lossless() {
                println!("fidelity   lossless (within the modelled subset)");
            } else {
                println!("fidelity   lossy: {}", document.fidelity().summary());
            }
        }
        Command::New { output, text } => {
            let document = match text {
                Some(text) if !text.trim().is_empty() => Document::from_text(&text),
                _ => sample_document(),
            };
            write_document(&output, &document)
                .with_context(|| format!("cannot write {}", output.display()))?;
            println!(
                "created {} ({} paragraph(s))",
                output.display(),
                document.paragraph_count()
            );
        }
        Command::Rewrite {
            input,
            output,
            force,
        } => {
            let document = read_document(&input)
                .with_context(|| format!("cannot read {}", input.display()))?;
            write_document_with_policy(&output, &document, force)
                .with_context(|| format!("cannot write {}", output.display()))?;
            println!(
                "rewrote {} block(s) -> {}",
                document.block_count(),
                output.display()
            );
        }
        Command::Insert {
            input,
            output,
            paragraph,
            offset,
            text,
        } => {
            let document = read_document(&input)
                .with_context(|| format!("cannot read {}", input.display()))?;
            let mut editor = Editor::new(document);
            let caret = editor
                .insert_text(0, paragraph, offset, &text)
                .with_context(|| format!("paragraph {paragraph} does not accept the insertion"))?;
            write_document(&output, editor.document())
                .with_context(|| format!("cannot write {}", output.display()))?;
            println!("caret now at character {caret}");
        }
    }
    Ok(())
}

fn sample_document() -> Document {
    let mut document = Document::new();
    document.push_paragraph(Paragraph::plain("Quire"));

    let mut body = Paragraph::plain("A clean-room Rust document engine. ");
    body.push_text(
        "Bold",
        CharProps {
            bold: true,
            ..CharProps::default()
        },
    );
    body.push_text(" and ", CharProps::default());
    body.push_text(
        "italic",
        CharProps {
            italic: true,
            ..CharProps::default()
        },
    );
    body.push_text(" runs, deterministic layout, and no account system.", CharProps::default());
    document.push_paragraph(body);
    document
}
fn blocks_table_count(document: &Document) -> usize {
    document
        .sections()
        .iter()
        .map(|section| section.table_count())
        .sum()
}
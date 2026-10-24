# Quire

**English** | [简体中文](README.zh-CN.md)

Quire is a clean-room word processor and spreadsheet editor written in Rust. It
contains a document model, DOCX/RTF/ODT/TXT I/O, XLSX/CSV value grids,
deterministic line breaking and pagination, an SVG paint backend, and a native
desktop shell (`quire-view`, built with eframe).

The core crates are independent of the UI and the platform. No crate in this
workspace performs network I/O, and every crate is `#![forbid(unsafe_code)]`.

## Project status

A working core plus a native editor shell. It builds, tests, and produces real
output, but it is not yet a drop-in replacement for a full office suite.

| Area | Today | Next |
| --- | --- | --- |
| Model | Sections, paragraphs, runs, styles, fonts, images, tables, numbering, headers/footers, page fields | Additional field types, footnotes |
| Import | DOCX, RTF, ODT and TXT with paragraphs, runs, styles, fonts, images, tables, numbering, headers/footers and PAGE/NUMPAGES; XLSX/CSV cached values into a sheet grid | Legacy .doc/.xls and PDF import; spreadsheet formulas |
| Export | DOCX, RTF, ODT and TXT documents from the CLI and the editor; XLSX/CSV computed cell values from the editor | Full OOXML round-trip, spreadsheet styles and formulas |
| Layout | Line breaking, pagination, images, tables, list labels, header/footer variants, per-page fields | Row splitting, floats, widow/orphan control, incremental relayout |
| Paint | SVG preview with per-run fonts, images, cell fills and borders | GPU (wgpu + vello), PDF, printing |
| Editing | Writer: caret, selection, IME, formatting, styles, split/merge, find/replace, table-cell and row/column editing, contextual table tools, header shading, undo/redo, guarded save. Sheet: cell and rectangular range editing, copy/cut/paste, undo/redo, find, basic formulas, row/column insertion and deletion, multi-sheet tabs, save values to XLSX/CSV | Writer: rich clipboard, merged cells. Sheet: full formula library, styles, sorting/filtering, merged cells |
| UI | Native shell (`quire-view`, eframe): writer with rail/inspector, focus mode, continuous canvas, tables and formatting; spreadsheet grid with column/row headers, formula bar, cell editor, range selection, undo/redo and find toolbar, sheet tabs, XLSX/CSV save and right-click menus | Full command deck, column resizing, merged cells, formatting |

## Editor surfaces

The same executable has two deliberately separate surfaces:

- **Writer** for DOCX, DOCM, DOTX, RTF, ODT and TXT. Markdown files (`.md`) open
  and save as plain text; there is no Markdown parser. Pages, tables, images,
  headers and footers stay in the reflowable document model.
- **Sheet** for XLSX, XLSM and CSV. These files open as a full grid with column
  letters, row numbers, a formula/value bar, editable cells and worksheet tabs.
  Basic arithmetic and SUM/AVERAGE/MIN/MAX/COUNT formulas are evaluated. Saving
  writes computed cell values; formulas, styles, merges and charts are not
  preserved. Opening XLSX prompts for a new output file instead of overwriting
  the source.

## Windows package

`scripts/package.ps1` generates a ready-to-run package at `dist/Quire/` and
`dist/Quire-0.1.0-windows-x64.zip`:

```
dist/Quire/Quire.exe      double-click to start (welcome screen if no document)
dist/Quire/Install.cmd    optional: install to LocalAppData and register .docx
dist/Quire/Uninstall.cmd
dist/Quire/sample.docx
dist/Quire/sample.csv     spreadsheet-grid demo
dist/Quire/sample.xlsx    two-sheet workbook demo
dist/Quire/README.txt
dist/Quire/LICENSE.txt
dist/Quire/NOTICE.txt
```

`dist/` is generated and is not tracked by Git. `Quire.exe` is built with the
static CRT, so it only depends on Windows system DLLs. Double-clicking starts the
app and opens a file picker; dragging a `.docx` onto the exe or passing it on the
command line also works. Open `sample.csv` to try the spreadsheet grid. The
release binary uses the Windows GUI subsystem, so no console window appears.

`Install.cmd` copies the app to `%LOCALAPPDATA%\Programs\Quire`, registers the
`.docx` "Open with" entry, context menu and Start Menu shortcut. Windows 10/11
does not let an app silently replace the default handler; the installer opens
Settings so you can choose Quire as the default for `.docx`.

The executable is unsigned, so SmartScreen may ask for confirmation
("More info" -> "Run anyway").

## Build and test

Requirements: Rust 1.85 or newer (edition 2024). The library crates are portable;
the packaged desktop shell targets Windows 10/11 x64.

```powershell
cargo test --workspace
cargo run -p quire-cli -- --help
cargo run -p quire-ui --bin quire-view
```

Crates are `publish = false` until the API stabilises.

## Try the CLI

```powershell
# Create a sample document
cargo run -p quire-cli -- new sample.docx

# Inspect it
cargo run -p quire-cli -- info sample.docx
cargo run -p quire-cli -- text sample.docx

# Lay it out and write an SVG preview
cargo run -p quire-cli -- render sample.docx -o sample.svg

# Open the native editor
cargo run -p quire-ui --bin quire-view -- sample.docx

# Safe round-trip (refuses unsupported content)
cargo run -p quire-cli -- rewrite sample.docx -o rewritten.docx
# Diagnostic round-trip that bypasses the guard
cargo run -p quire-cli -- rewrite sample.docx -o forced.docx --force

# Insert text at paragraph 1, character 6
cargo run -p quire-cli -- insert sample.docx -o edited.docx --paragraph 1 --offset 6 --text "real "
```

Open `sample.svg` in a browser. By default every text run carries `textLength`,
so the preview keeps the deterministic advances used by layout. Pass
`--system-fonts` to measure with installed fonts and let the viewer render glyphs
at their natural advances.

## Save safety

The writer **refuses** to save a document that contains content it cannot write
back, instead of silently dropping data:

```
$ quire rewrite document.docx -o out.docx
Error: document uses content that cannot be written back yet:
fields, hyperlinks, unsupported run properties
```

`--force` bypasses the guard and is diagnostic only. Parts that are not parsed
(theme, fontTable, docProps, customXml, embeddings, ...) are preserved verbatim,
and their content types and relationships are merged into the regenerated
package.

Do not use the forced path on production documents; the guarded default path is
the safe one. Reading and rendering are non-destructive.

## Known limitations

### Document model and I/O

- Fields other than `PAGE`/`NUMPAGES` are not modelled, and page fields outside
  headers/footers block strict saving. Other field kinds are preserved as visible
  text.
- Footnotes, endnotes, comments and tracked revisions are not modelled.
- Only paragraph and character styles are written; table and numbering styles are
  dropped.
- Hyperlinks, bookmarks, content controls, symbols, ruby and alternate content
  are not modelled.
- Only the first section is written, so multiple sections and per-section
  header/footer sets collapse.
- Floating and anchored shapes, legacy drawings and text boxes are not modelled.
- Numbering defined through a paragraph style (`w:pStyle` plus style-level
  `numPr`) is not applied; only direct `w:numPr` is read.
- Unsupported `pPr`/`rPr`/`sectPr`/table-property children (for example tab stops,
  keep-with-next, columns and table styles) are not written. Font family,
  complex-script size, bold/italic for complex scripts and language are supported.
- ODT export refuses numbered documents instead of flattening the lists (list
  numbering is not written to ODT yet).
- Legacy binary `.doc`/`.xls` and PDF import are not supported.

### Layout and painting

- Table rows do not split across pages and header rows are not repeated.
- Cell rectangles are drawn independently, so vertical merges keep their text
  but are not painted as one merged cell.
- Auto-fit column widths are not implemented; grid widths are scaled to the
  measure.
- Nested tables are read as flat cell content and are not laid out.
- List labels are placed inline with a separator, not on a real tab stop, so the
  number-to-text gap is approximate.
- List restart rules are simplified: switching `numId` restarts the list, and
  deeper levels reset when a shallower level advances.
- Header/footer distance is fixed at 36 pt; `w:pgMar` header/footer values are
  not read. Headers or footers taller than the page margin overlap the body.
- Line breaking is cached per paragraph, but pagination still walks the whole
  document on each edit, and page textures are invalidated by page fingerprint,
  not by a byte-range dirty region.
- Page images are rasterized on demand; the cache keeps 16 full pages and 80
  thumbnails, so long documents re-render pages when scrolling back.
- `SystemMetrics` shapes each token with rustybuzz (kerning and ligatures), but
  only for left-to-right text. Bidi and complex-script reordering are not
  implemented.

### Editing

- Numbered paragraphs and header/footer text are not click-to-edit yet.
- Selection deletion across table cells clears text without merging cells.
- Applying a paragraph style replaces paragraph properties and run formatting;
  direct versus inherited formatting is not tracked separately, so style
  application is destructive.
- Find/replace scans top-level body paragraphs with plain-text matching only;
  there is no regex, whole-word or header/table search yet.
- Clipboard support is plain text only. Rich-text and internal fragment
  clipboard are not implemented. Formatting edits rebuild runs and drop named
  run styles (`rStyle`); formatting applies to a selection only and does not set
  a style for the next typed character.

### Spreadsheet

- Spreadsheet support is a value grid: basic arithmetic and
  SUM/AVERAGE/MIN/MAX/COUNT formulas work, but the full formula library, styles,
  merged cells, charts, frozen panes, sorting and filtering are not implemented.
- Saving XLSX writes computed cell values only.
- Presentations are out of scope.

## Architecture

```
quire-base     units, ids, geometry, shared errors
quire-model    document tree and formatting
quire-io       DOCX/RTF/ODT/TXT and XLSX/CSV I/O
quire-text     tokenisation and font metrics
quire-layout   line breaking and pagination
quire-paint    device-independent preview (SVG now, GPU/PDF later)
quire-edit     editing operations and undo
quire-cli      command line wiring
quire-ui       native shell (page rasterizer + quire-view binary)
```

Dependency direction is enforced by crate boundaries: the core is UI-independent
and platform-independent. No crate in this workspace performs network I/O, and
`#![forbid(unsafe_code)]` is set everywhere.

## Explicit non-goals

- No account, login, cloud sync or membership features.
- No advertising, telemetry, analytics or "recommendations".
- No proprietary document extensions, DRM or store integration.
- No copied code, strings, icons or visual assets from any existing office
  suite. See [`CLEANROOM.md`](CLEANROOM.md).

## Documentation

- [`CONTRIBUTING.md`](CONTRIBUTING.md) — build, test and pull-request rules
  ([简体中文](CONTRIBUTING.zh-CN.md))
- [`CLEANROOM.md`](CLEANROOM.md) — clean-room and provenance policy
  ([简体中文](CLEANROOM.zh-CN.md))

## License

Apache License 2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project is licensed under the Apache License 2.0, without
additional terms or conditions (Apache-2.0 section 5).

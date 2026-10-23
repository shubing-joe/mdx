//! Pipeline behind the MCP tool handlers: parse → assets → math/diagrams →
//! Typst/HTML → output file.
//!
//! `mdx-mcp` never depends on the `mdx-cli` binary crate, so the pipeline the
//! CLI runs is reproduced here on top of the frozen crate APIs (`mdx-parse`,
//! `mdx-assets`, `mdx-math`, `mdx-diagram`, `mdx-typst`, `mdx-html`) with the
//! same defaults the CLI uses when no flag is given. The read-only helpers
//! behind `md_outline` and `md_version` and the batch entry point behind
//! `md_render_many` live here too. Nothing in this module panics: every
//! failure is reported as a [`ToolError`] whose payload carries the stable
//! diagnostic code (`E_IO`, `E_ASSET_MISSING`, …) plus every diagnostic the
//! pipeline collected.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use mdx_assets::{AssetPolicy, AssetStore};
use mdx_core::{Block, DiagCode, Diagnostic, Doc, Inline, Report, TocEntry};
use mdx_typst::{DiagramMap, MathMap, PageSetup, Paper, RenderOptions};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Hard ceiling for a single asset payload (64 MiB, the CLI's limit).
///
/// A payload above it fails with `E_ASSET_UNSUPPORTED` instead of being loaded,
/// which keeps one oversized remote or local file from exhausting the server.
pub const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

/// Default page margin in millimetres, matching the CLI default.
pub const DEFAULT_MARGIN_MM: f32 = 20.0;

/// A tool-level failure.
///
/// The handlers turn this into an `isError` tool result whose structured
/// payload is `{"code", "message", "diagnostics"}`, so callers can branch on
/// the same `E_*`/`W_*` codes the CLI prints.
#[derive(Debug, Serialize)]
pub struct ToolError {
    /// Stable diagnostic code, e.g. `E_IO`.
    pub code: &'static str,
    /// Human-readable summary (the diagnostic's own text).
    pub message: String,
    /// Every diagnostic the failed pipeline stage collected.
    pub diagnostics: Vec<Diagnostic>,
}

impl ToolError {
    /// Failure raised by the tool layer itself, e.g. an unusable `path`.
    pub fn new(code: DiagCode, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            code: code.as_str(),
            diagnostics: vec![Diagnostic::error(code, message.clone())],
            message,
        }
    }

    /// Failure taken from a pipeline diagnostic; the code is derived from it.
    pub fn from_diagnostic(diagnostic: &Diagnostic) -> Self {
        Self {
            code: diagnostic.code.as_str(),
            message: diagnostic.to_string(),
            diagnostics: vec![diagnostic.clone()],
        }
    }

    /// Failure describing a whole report: the first error names the failure,
    /// all diagnostics travel with it. `None` when nothing is fatal.
    pub fn from_report(report: &Report) -> Option<Self> {
        let first = report.errors().next()?;
        Some(Self {
            code: first.code.as_str(),
            message: first.to_string(),
            diagnostics: report.diagnostics.clone(),
        })
    }

    /// Structured payload of the failure.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Output format of [`render_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RenderFormat {
    /// Typst-rendered PDF.
    Pdf,
    /// Self-contained HTML.
    Html,
}

impl RenderFormat {
    /// File extension used for the default output path.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Html => "html",
        }
    }
}

/// Arguments of [`render_file`].
#[derive(Debug)]
pub struct RenderRequest<'a> {
    /// Markdown file to render.
    pub input: &'a str,
    /// Requested output format.
    pub format: RenderFormat,
    /// Output file; defaults to `input` with the format's extension.
    pub out: Option<&'a str>,
    /// Insert a table of contents (PDF output only).
    pub toc: bool,
}

/// Result of a successful render.
#[derive(Debug, Serialize)]
pub struct RenderOutcome {
    /// Path the document was written to, as requested or derived.
    pub output: String,
    /// Number of bytes written.
    pub bytes: usize,
}

/// Result of a successful check, mirroring `mdx check --json`.
#[derive(Debug, Serialize)]
pub struct CheckOutcome {
    /// `true` when no diagnostic is an error.
    pub ok: bool,
    /// Process exit code the CLI would use (`0`, `1` or `3`).
    pub exit: i32,
    /// Every diagnostic, in pipeline order.
    pub diagnostics: Vec<Diagnostic>,
}

/// A Markdown file that was read and parsed.
#[derive(Debug)]
pub struct Loaded {
    /// Path the source was read from.
    pub path: PathBuf,
    /// Parsed document; `source_path` is filled in with `path`.
    pub doc: Doc,
    /// Diagnostics from the parse stage.
    pub report: Report,
}

impl Loaded {
    /// Resolve the document's assets, merging both reports.
    pub fn resolve_assets(mut self) -> (AssetStore, Report, Doc) {
        let (store, assets_report) =
            mdx_assets::resolve_all(&mut self.doc, &doc_dir(&self.path), &asset_policy());
        let mut report = self.report;
        report.merge_from(assets_report);
        (store, report, self.doc)
    }
}

/// Parse `path`, resolve its assets and collect the diagnostics of both stages
/// (`mdx check`).
///
/// Math conversion runs too, so a formula the converter rejects is reported as
/// an `E_MATH_CONVERT` warning exactly like the CLI does; diagrams are not
/// rendered and no output is produced.
pub fn check_file(path: &str) -> Result<CheckOutcome, ToolError> {
    let loaded = parse_file(path)?;
    let (_assets, mut report, doc) = loaded.resolve_assets();
    // Reuse the render-stage conversion so the diagnostics match `mdx render`;
    // the resulting map is only needed for rendering.
    let _math = math_map(&doc, &mut report);
    Ok(CheckOutcome {
        ok: !report.has_errors(),
        exit: mdx_core::exit_code(&report),
        diagnostics: report.diagnostics,
    })
}

/// Build the `mdx-assets/1` manifest of every asset `path` resolves to.
pub fn manifest_file(path: &str) -> Result<Value, ToolError> {
    let loaded = parse_file(path)?;
    let (store, _report, _doc) = loaded.resolve_assets();
    Ok(store.to_json())
}

/// Result of [`outline_file`]: the document's headings, in document order.
#[derive(Debug, Serialize)]
pub struct OutlineOutcome {
    /// One `{level, id, title}` entry per heading, exactly as the parser
    /// recorded them in the `mdx-ast/1` table of contents.
    pub toc: Vec<TocEntry>,
}

/// Read `path` and return its headings as a table of contents.
///
/// Only headings contribute. Ids are the parser's github-style slugs,
/// deduplicated with `-1`, `-2`, so an entry can always be located in the
/// rendered document.
pub fn outline_file(path: &str) -> Result<OutlineOutcome, ToolError> {
    let loaded = parse_file(path)?;
    let toc = loaded.doc.toc;
    Ok(OutlineOutcome { toc })
}

/// Render `request.input` and write it to the requested output file.
///
/// Fails when any stage reports a fatal diagnostic; a document is never
/// written half-rendered.
pub fn render_file(request: &RenderRequest<'_>) -> Result<RenderOutcome, ToolError> {
    let loaded = parse_file(request.input)?;
    let input_path = loaded.path.clone();
    let (assets, mut report, doc) = loaded.resolve_assets();

    // Assets are resolved before anything is rendered, so a missing or refused
    // payload stops the run here (exit 3 in the CLI) instead of producing a
    // document with a broken image.
    if let Some(error) = ToolError::from_report(&report) {
        return Err(error);
    }

    let math = math_map(&doc, &mut report);
    let diagrams = diagram_map(&doc, &mut report);

    // Math conversion problems are warnings (the writer passes the raw LaTeX
    // through), diagram failures are errors.
    if let Some(error) = ToolError::from_report(&report) {
        return Err(error);
    }

    let bytes = match request.format {
        RenderFormat::Pdf => {
            let options = render_options(&doc, request.toc);
            mdx_typst::render_pdf(&doc, &assets, &math, &diagrams, &options)
                .map_err(|diagnostic| ToolError::from_diagnostic(&diagnostic))?
        }
        RenderFormat::Html => {
            let options = html_options(&doc);
            mdx_html::render_html(&doc, &assets, &math, &diagrams, &options)
                .map_err(|diagnostic| ToolError::from_diagnostic(&diagnostic))?
                .into_bytes()
        }
    };

    let output = output_path(&input_path, request.out, request.format);
    std::fs::write(&output, &bytes).map_err(|err| {
        ToolError::new(
            DiagCode::Io,
            format!("cannot write {}: {err}", output.display()),
        )
    })?;

    Ok(RenderOutcome {
        output: output.to_string_lossy().into_owned(),
        bytes: bytes.len(),
    })
}

fn output_path(input: &Path, out: Option<&str>, format: RenderFormat) -> PathBuf {
    match out {
        Some(out) if !out.trim().is_empty() => PathBuf::from(out),
        _ => input.with_extension(format.extension()),
    }
}

/// Arguments of [`render_many`].
#[derive(Debug)]
pub struct RenderManyRequest<'a> {
    /// Markdown files to render, processed in the order given.
    pub inputs: &'a [String],
    /// Output format of every file.
    pub format: RenderFormat,
    /// Directory the outputs are written to; when omitted every file is
    /// written next to its source, as `md_render` would.
    pub out_dir: Option<&'a str>,
}

/// One file of a [`render_many`] run.
#[derive(Debug, Serialize)]
pub struct RenderManyItem {
    /// Input path exactly as requested, so a caller can pair results up.
    pub input: String,
    /// Path the document was written to; `null` when the file failed.
    pub output: Option<String>,
    /// Number of bytes written; `null` when the file failed.
    pub bytes: Option<usize>,
    /// `true` when the file was rendered and written.
    pub ok: bool,
    /// Diagnostic code of the failure (`E_IO`, `E_ASSET_MISSING`, …); absent
    /// when `ok` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
}

/// Result of [`render_many`]: one entry per requested file, in request order.
#[derive(Debug, Serialize)]
pub struct RenderManyOutcome {
    /// Successes and failures alike; the order follows `inputs`.
    pub results: Vec<RenderManyItem>,
}

/// Render every file of `request`, one after another.
///
/// Each file runs through the same pipeline as [`render_file`], sequentially
/// and in the order given. A file that cannot be rendered is reported with
/// `ok: false` and its diagnostic code and never stops the remaining files, so
/// one broken document does not cost the caller the rest of the batch. The
/// entry carries the code only; [`render_file`] on the same path reports the
/// full diagnostic.
///
/// Only the arguments as a whole can fail the call: an empty `paths` list, or
/// an `out_dir` that does not exist and cannot be created. When `out_dir` is
/// set, two inputs with the same file name write to the same output and the
/// later one wins.
pub fn render_many(request: &RenderManyRequest<'_>) -> Result<RenderManyOutcome, ToolError> {
    if request.inputs.is_empty() {
        return Err(ToolError::new(DiagCode::Config, "paths must not be empty"));
    }
    let out_dir = match request.out_dir.map(str::trim) {
        None | Some("") => None,
        Some(dir) => {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).map_err(|err| {
                let message = format!("cannot create {}: {err}", dir.display());
                ToolError::new(DiagCode::Io, message)
            })?;
            Some(dir)
        }
    };
    let mut results = Vec::with_capacity(request.inputs.len());
    for input in request.inputs {
        results.push(render_many_one(input, request.format, out_dir.as_deref()));
    }
    Ok(RenderManyOutcome { results })
}

/// Render one file of a [`render_many`] run; a failure stays with its entry.
fn render_many_one(input: &str, format: RenderFormat, out_dir: Option<&Path>) -> RenderManyItem {
    let out = out_dir
        .and_then(|dir| output_name(input, format).map(|name| dir.join(name)))
        .map(|path| path.to_string_lossy().into_owned());
    let request = RenderRequest {
        input,
        format,
        out: out.as_deref(),
        toc: false,
    };
    match render_file(&request) {
        Ok(outcome) => RenderManyItem {
            input: input.to_string(),
            output: Some(outcome.output),
            bytes: Some(outcome.bytes),
            ok: true,
            code: None,
        },
        Err(error) => RenderManyItem {
            input: input.to_string(),
            output: None,
            bytes: None,
            ok: false,
            code: Some(error.code),
        },
    }
}

/// Output file name of `input` inside an output directory: the input's own
/// file name with the format's extension.
///
/// `None` only when `input` has no file name at all, which an existing file
/// always has; the default output path next to the input then applies.
fn output_name(input: &str, format: RenderFormat) -> Option<PathBuf> {
    let name = Path::new(input).file_name()?;
    Some(PathBuf::from(name).with_extension(format.extension()))
}

/// Version of the `mdx` toolchain, taken from the version cargo stamped into
/// this binary.
///
/// Every crate in the workspace inherits `version.workspace = true`, so this
/// single version is the version of each crate listed in [`CRATES`].
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `mdx` crates this server is built from, sorted by name.
pub const CRATES: [&str; 8] = [
    "mdx-assets",
    "mdx-core",
    "mdx-diagram",
    "mdx-html",
    "mdx-math",
    "mdx-mcp",
    "mdx-parse",
    "mdx-typst",
];

/// Result of [`version`].
#[derive(Debug, Serialize)]
pub struct VersionOutcome {
    /// Toolchain version, e.g. `0.1.0`.
    pub version: &'static str,
    /// Version of every crate, keyed by crate name.
    pub crates: BTreeMap<&'static str, &'static str>,
}

/// Versions of the running toolchain (`mdx version`).
pub fn version() -> VersionOutcome {
    VersionOutcome {
        version: VERSION,
        crates: CRATES.iter().map(|name| (*name, VERSION)).collect(),
    }
}

/// Read and parse `path` into the document model (`mdx ast`), validating the
/// path first.
pub fn parse_file(path: &str) -> Result<Loaded, ToolError> {
    let path = validate_input(path)?;
    let source = std::fs::read_to_string(&path).map_err(|err| {
        ToolError::new(
            DiagCode::Io,
            format!("cannot read {}: {err}", path.display()),
        )
    })?;
    let options = mdx_parse::ParseOptions {
        gfm: true,
        math: true,
        frontmatter: true,
    };
    let (mut doc, report) = mdx_parse::parse(&source, &options);
    doc.source_path = Some(path.to_string_lossy().into_owned());
    Ok(Loaded { path, doc, report })
}

/// Reject arguments that cannot name a readable Markdown file.
fn validate_input(path: &str) -> Result<PathBuf, ToolError> {
    if path.trim().is_empty() {
        return Err(ToolError::new(DiagCode::Config, "path must not be empty"));
    }
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(ToolError::new(
            DiagCode::Io,
            format!("file not found: {}", path.display()),
        ));
    }
    if !path.is_file() {
        return Err(ToolError::new(
            DiagCode::Io,
            format!("not a file: {}", path.display()),
        ));
    }
    Ok(path)
}

/// Directory relative asset paths resolve against.
fn doc_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Asset policy used by every tool: remote assets are allowed, nothing is
/// cached and oversized payloads are refused.
fn asset_policy() -> AssetPolicy {
    AssetPolicy {
        offline: false,
        allow_remote: true,
        cache_dir: None,
        max_bytes: MAX_ASSET_BYTES,
    }
}

/// Typst options for the MCP server: A4, 20 mm margins, numbered pages, the
/// packaged fonts, and the front matter title.
fn render_options(doc: &Doc, toc: bool) -> RenderOptions {
    RenderOptions {
        page: PageSetup {
            paper: Paper::A4,
            margin_mm: DEFAULT_MARGIN_MM,
            numbered: true,
        },
        toc,
        pdf_standard: None,
        fonts: Vec::new(),
        ignore_system_fonts: false,
        // The MCP server has no `--deterministic` flag: dates stay as written.
        deterministic_today: None,
        title: doc.front.title.clone(),
    }
}

/// HTML options: one self-contained document, no extra stylesheet.
fn html_options(doc: &Doc) -> mdx_html::HtmlOptions {
    mdx_html::HtmlOptions {
        embed_assets: true,
        css: None,
        title: doc.front.title.clone(),
    }
}

/// Diagram options: reproducible text metrics, packaged fonts only.
fn diagram_options() -> mdx_diagram::DiagramOptions {
    mdx_diagram::DiagramOptions {
        deterministic_text: true,
        font_files: Vec::new(),
    }
}

/// Convert every formula in `doc` to a Typst expression.
///
/// Formulas the converter rejects are left out of the map (the writer passes
/// the raw LaTeX through) and reported once as an `E_MATH_CONVERT` warning.
fn math_map(doc: &Doc, report: &mut Report) -> MathMap {
    let mut map = MathMap::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    for_each_latex(&doc.blocks, &mut |latex| {
        if map.contains_key(latex) || failed.contains(latex) {
            return;
        }
        match mdx_math::latex_to_typst(latex) {
            Ok(typst) => {
                map.insert(latex.to_string(), typst);
            }
            Err(message) => {
                failed.insert(latex.to_string());
                report.push(Diagnostic::warning(DiagCode::MathConvert, message));
            }
        }
    });
    map
}

/// Render every Mermaid block in `doc` to SVG.
///
/// A diagram that cannot be rendered is reported once as an
/// `E_DIAGRAM_RENDER` error and left out of the map.
fn diagram_map(doc: &Doc, report: &mut Report) -> DiagramMap {
    let mut map = DiagramMap::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    let options = diagram_options();
    for_each_diagram(&doc.blocks, &mut |source| {
        if map.contains_key(source) || failed.contains(source) {
            return;
        }
        match mdx_diagram::render_svg(source, &options) {
            Ok(svg) => {
                map.insert(source.to_string(), svg);
            }
            Err(message) => {
                failed.insert(source.to_string());
                report.push(Diagnostic::error(DiagCode::DiagramRender, message));
            }
        }
    });
    map
}

/// Visit every LaTeX formula in document order.
fn for_each_latex(blocks: &[Block], f: &mut impl FnMut(&str)) {
    for block in blocks {
        match block {
            Block::Math { latex, .. } => f(latex),
            Block::Heading { inlines, .. } | Block::Paragraph { inlines } => {
                for_each_inline_latex(inlines, f);
            }
            Block::List { items, .. } => {
                for item in items {
                    for_each_latex(item, f);
                }
            }
            Block::BlockQuote { blocks } | Block::FootnoteDef { blocks, .. } => {
                for_each_latex(blocks, f);
            }
            Block::Table { header, rows, .. } => {
                for cell in header {
                    for_each_inline_latex(cell, f);
                }
                for row in rows {
                    for cell in row {
                        for_each_inline_latex(cell, f);
                    }
                }
            }
            Block::Code { .. }
            | Block::Diagram { .. }
            | Block::Image { .. }
            | Block::ThematicBreak
            | Block::Html { .. } => {}
        }
    }
}

/// Visit every LaTeX formula inside inline content, in document order.
fn for_each_inline_latex(inlines: &[Inline], f: &mut impl FnMut(&str)) {
    for inline in inlines {
        match inline {
            Inline::Math { latex, .. } => f(latex),
            Inline::Emph { inlines }
            | Inline::Strong { inlines }
            | Inline::Strike { inlines }
            | Inline::Link { inlines, .. } => for_each_inline_latex(inlines, f),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::Image { .. }
            | Inline::FootnoteRef { .. }
            | Inline::LineBreak { .. } => {}
        }
    }
}

/// Visit every Mermaid diagram source in document order.
fn for_each_diagram(blocks: &[Block], f: &mut impl FnMut(&str)) {
    for block in blocks {
        match block {
            Block::Diagram { source } => f(source),
            Block::List { items, .. } => {
                for item in items {
                    for_each_diagram(item, f);
                }
            }
            Block::BlockQuote { blocks } | Block::FootnoteDef { blocks, .. } => {
                for_each_diagram(blocks, f);
            }
            Block::Heading { .. }
            | Block::Paragraph { .. }
            | Block::Code { .. }
            | Block::Math { .. }
            | Block::Image { .. }
            | Block::Table { .. }
            | Block::ThematicBreak
            | Block::Html { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path that cannot exist inside the crate directory.
    const MISSING: &str = "mdx-mcp-missing-fixture.md";

    #[test]
    fn an_empty_path_is_a_config_error() {
        let error = validate_input("   ").expect_err("an empty path must be rejected");
        assert_eq!(error.code, "E_CONFIG");
        assert_eq!(error.code, DiagCode::Config.as_str());
        assert_eq!(error.diagnostics.len(), 1);
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let error = validate_input(MISSING).expect_err("a missing file must be rejected");
        assert_eq!(error.code, "E_IO");
        assert!(
            error.message.contains("file not found"),
            "{}",
            error.message
        );
        let payload = error.to_value();
        assert_eq!(payload["code"], "E_IO");
        assert_eq!(payload["diagnostics"][0]["code"], "E_IO");
        assert_eq!(payload["diagnostics"][0]["severity"], "error");
    }

    #[test]
    fn a_directory_is_not_a_markdown_file() {
        let error = validate_input(".").expect_err("a directory must be rejected");
        assert_eq!(error.code, "E_IO");
        assert!(error.message.contains("not a file"), "{}", error.message);
    }

    #[test]
    fn a_readable_file_resolves_to_its_path() {
        // `Cargo.toml` is the one file every crate directory has.
        let path = validate_input("Cargo.toml").expect("Cargo.toml must be readable");
        assert!(path.is_file());
    }

    #[test]
    fn the_default_output_path_reuses_the_input_stem() {
        let input = Path::new("notes/doc.md");
        assert_eq!(
            output_path(input, None, RenderFormat::Pdf),
            Path::new("notes/doc.pdf")
        );
        assert_eq!(
            output_path(input, None, RenderFormat::Html),
            Path::new("notes/doc.html")
        );
        assert_eq!(
            output_path(input, Some("build/out.html"), RenderFormat::Pdf),
            Path::new("build/out.html")
        );
        // A blank `out` falls back to the derived path.
        assert_eq!(
            output_path(input, Some("  "), RenderFormat::Pdf),
            Path::new("notes/doc.pdf")
        );
    }

    #[test]
    fn a_report_without_errors_is_not_a_failure() {
        let mut report = Report::default();
        report.push(Diagnostic::warning(DiagCode::FrontMatter, "yaml is broken"));
        assert!(ToolError::from_report(&report).is_none());
    }

    #[test]
    fn the_first_error_names_the_failure_and_all_diagnostics_travel_with_it() {
        let mut report = Report::default();
        report.push(Diagnostic::warning(DiagCode::FrontMatter, "yaml is broken"));
        report.push(Diagnostic::error(DiagCode::AssetMissing, "no such image").for_asset("a.png"));
        let error = ToolError::from_report(&report).expect("an error must be reported");
        assert_eq!(error.code, "E_ASSET_MISSING");
        assert_eq!(error.diagnostics.len(), 2);
        assert_eq!(error.diagnostics[1].asset.as_deref(), Some("a.png"));
    }

    #[test]
    fn every_lambda_of_a_nested_document_is_visited_in_order() {
        let doc = Doc::new(vec![
            Block::Math {
                latex: "a".to_string(),
                display: true,
            },
            Block::List {
                ordered: false,
                start: None,
                tight: true,
                items: vec![vec![Block::Paragraph {
                    inlines: vec![
                        Inline::Math {
                            latex: "b".to_string(),
                            display: false,
                        },
                        Inline::Strong {
                            inlines: vec![Inline::Math {
                                latex: "c".to_string(),
                                display: false,
                            }],
                        },
                    ],
                }]],
            },
        ]);
        let mut seen = Vec::new();
        for_each_latex(&doc.blocks, &mut |latex| seen.push(latex.to_string()));
        assert_eq!(
            seen,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn a_nested_diagram_is_visited() {
        let blocks = vec![Block::BlockQuote {
            blocks: vec![Block::Diagram {
                source: "graph TD;".to_string(),
            }],
        }];
        let mut seen = Vec::new();
        for_each_diagram(&blocks, &mut |source| seen.push(source.to_string()));
        assert_eq!(seen, vec!["graph TD;".to_string()]);
    }

    /// A per-process scratch directory.
    ///
    /// Unit tests have no `CARGO_TARGET_TMPDIR`, so the tests that need real
    /// files use the system temp directory and remove it again.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mdx-mcp-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        dir
    }

    #[test]
    fn an_outline_lists_every_heading_in_document_order() {
        let dir = scratch("outline");
        let doc = dir.join("outline.md");
        let source = "# One\n\ntext\n\n## Two\n\n## Two\n\n### Deep\n";
        std::fs::write(&doc, source).expect("write the outline fixture");
        let path = doc.to_string_lossy().into_owned();
        let outcome = outline_file(&path).expect("the fixture must parse");
        let entries: Vec<(u8, &str, &str)> = outcome
            .toc
            .iter()
            .map(|entry| (entry.level, entry.id.as_str(), entry.title.as_str()))
            .collect();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0], (1, "one", "One"));
        assert_eq!(entries[1], (2, "two", "Two"));
        assert_eq!(entries[2], (2, "two-1", "Two"));
        assert_eq!(entries[3], (3, "deep", "Deep"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_outline_of_a_missing_file_is_an_io_error() {
        let error = outline_file(MISSING).expect_err("a missing file must be rejected");
        assert_eq!(error.code, "E_IO");
    }

    #[test]
    fn an_empty_batch_is_a_config_error() {
        let inputs: Vec<String> = Vec::new();
        let request = RenderManyRequest {
            inputs: &inputs,
            format: RenderFormat::Pdf,
            out_dir: None,
        };
        let error = render_many(&request).expect_err("an empty batch must be rejected");
        assert_eq!(error.code, "E_CONFIG");
    }

    #[test]
    fn an_output_name_inside_an_output_directory_keeps_the_input_stem() {
        let pdf = output_name("notes/doc.md", RenderFormat::Pdf);
        assert_eq!(pdf, Some(PathBuf::from("doc.pdf")));
        let html = output_name("/tmp/notes/doc.markdown", RenderFormat::Html);
        assert_eq!(html, Some(PathBuf::from("doc.html")));
    }

    #[test]
    fn a_batch_renders_the_good_file_and_reports_the_broken_one() {
        let root = scratch("render-many");
        let source = root.join("src");
        let out = root.join("out");
        std::fs::create_dir_all(&source).expect("create the source directory");
        let good = source.join("good.md");
        std::fs::write(&good, "# Title\n\nBody text.\n\n## Second\n\n- item\n")
            .expect("write the batch fixture");
        assert!(!out.exists(), "the batch must create the output directory");

        let inputs = vec![MISSING.to_string(), good.to_string_lossy().into_owned()];
        let out_dir = out.to_string_lossy().into_owned();
        let request = RenderManyRequest {
            inputs: &inputs,
            format: RenderFormat::Html,
            out_dir: Some(&out_dir),
        };
        let outcome = render_many(&request).expect("the batch itself must succeed");
        assert_eq!(outcome.results.len(), 2);

        // The broken file comes first: the second one still renders.
        assert!(!outcome.results[0].ok);
        assert_eq!(outcome.results[0].input, MISSING);
        assert_eq!(outcome.results[0].code, Some("E_IO"));
        assert_eq!(outcome.results[0].output, None);
        assert_eq!(outcome.results[0].bytes, None);

        assert!(outcome.results[1].ok, "{:?}", outcome.results[1]);
        assert_eq!(outcome.results[1].input, inputs[1]);
        assert!(outcome.results[1].bytes.is_some_and(|bytes| bytes > 0));
        let expected = out.join("good.html").to_string_lossy().into_owned();
        assert_eq!(
            outcome.results[1].output.as_deref(),
            Some(expected.as_str())
        );
        assert!(out.join("good.html").is_file());

        // A successful entry carries no `code`, a failed one has no output.
        let payload = serde_json::to_value(&outcome).expect("the outcome must serialise");
        assert!(payload["results"][0].get("code").is_some());
        assert!(payload["results"][0]["output"].is_null());
        assert!(payload["results"][1].get("code").is_none());
        assert_eq!(payload["results"][1]["ok"].as_bool(), Some(true));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_version_is_the_packages_version() {
        let outcome = version();
        let expected = env!("CARGO_PKG_VERSION");
        assert_eq!(outcome.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(outcome.version, VERSION);
        assert!(!outcome.version.is_empty());
        assert_eq!(outcome.crates.len(), CRATES.len());
        assert_eq!(outcome.crates.get("mdx-core"), Some(&expected));
        assert_eq!(outcome.crates.get("mdx-mcp"), Some(&VERSION));
    }

    #[test]
    fn every_reported_crate_inherits_the_workspace_version() {
        // One manifest per crate `CRATES` lists, read at compile time: the
        // single version `version()` reports holds only while each of them
        // inherits it from the workspace.
        const MANIFESTS: [(&str, &str); 8] = [
            ("mdx-assets", include_str!("../../mdx-assets/Cargo.toml")),
            ("mdx-core", include_str!("../../mdx-core/Cargo.toml")),
            ("mdx-diagram", include_str!("../../mdx-diagram/Cargo.toml")),
            ("mdx-html", include_str!("../../mdx-html/Cargo.toml")),
            ("mdx-math", include_str!("../../mdx-math/Cargo.toml")),
            ("mdx-mcp", include_str!("../../mdx-mcp/Cargo.toml")),
            ("mdx-parse", include_str!("../../mdx-parse/Cargo.toml")),
            ("mdx-typst", include_str!("../../mdx-typst/Cargo.toml")),
        ];
        assert_eq!(MANIFESTS.map(|(name, _)| name), CRATES);
        for (name, manifest) in MANIFESTS {
            assert!(manifest.contains(&format!("name = \"{name}\"")), "{name}");
            assert!(manifest.contains("version.workspace = true"), "{name}");
        }
    }
}

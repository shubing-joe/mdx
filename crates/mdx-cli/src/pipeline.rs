//! Stage pipeline shared by the `mdx` subcommands.
//!
//! Every subcommand runs a prefix of the same sequence —
//! read → parse → resolve assets → convert math → render diagrams → write —
//! so the stages live here instead of being repeated per command: `render` runs
//! all of them, `check` and `assets` stop after asset resolution (plus math
//! validation), `ast` stops after parsing.
//!
//! Determinism: the work items of the conversion stages are collected into
//! [`BTreeSet`]s and the lookups the writers consume are maps ordered by key, so
//! neither thread scheduling nor the document's own repetition changes a
//! rendered result. No clock, environment or random source is read; the only
//! time-dependent value a build can see is the fixed date [`DETERMINISTIC_TODAY`]
//! that `--deterministic` hands to the Typst world.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use mdx_assets::{AssetPolicy, AssetStore};
use mdx_core::{Block, DiagCode, Diagnostic, Doc, Inline, Report};
use mdx_diagram::DiagramOptions;
use mdx_html::HtmlOptions;
use mdx_parse::ParseOptions;
use mdx_typst::{DiagramMap, MathMap, RenderOptions};

/// Largest asset payload the CLI accepts (64 MiB).
///
/// [`mdx_assets::AssetPolicy`] treats `0` as "unlimited"; a document tool wants
/// a bound so an accidental link to a huge file cannot exhaust memory.
pub const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

/// Fixed date the Typst world is given when `--deterministic` is set.
pub const DETERMINISTIC_TODAY: &str = "2026-01-01";

/// Page margin of the default page setup, in millimetres.
pub const DEFAULT_MARGIN_MM: f32 = 20.0;

/// Characters of a diagram/math source kept in a diagnostic message.
const EXCERPT_CHARS: usize = 60;

/// A parsed document together with everything the subcommand read for it.
pub struct Loaded {
    /// Parsed document; asset references are resolved to [`mdx_core::AssetId`]s.
    pub doc: Doc,
    /// Every asset the document references, keyed by content-addressed id.
    pub assets: AssetStore,
    /// Parse diagnostics first, then asset diagnostics.
    pub report: Report,
}

/// Asset policy the CLI derives from its flags.
///
/// Remote (`http(s)://`) assets are allowed unless `--offline` was given: that
/// flag, not a missing cache, is the CLI's opt-out, and `mdx-assets` refuses
/// remote payloads in offline mode no matter what.
pub fn asset_policy(offline: bool) -> AssetPolicy {
    AssetPolicy {
        offline,
        allow_remote: true,
        cache_dir: None,
        max_bytes: MAX_ASSET_BYTES,
    }
}

/// Directory relative asset paths resolve against.
///
/// That is the input's own directory, or `.` for a bare file name such as
/// `doc.md` (whose parent is the empty path).
pub fn doc_dir(input: &Path) -> PathBuf {
    match input.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Read `input` as UTF-8 text; failures carry `E_IO` and the path.
pub fn read_source(input: &Path) -> Result<String, Diagnostic> {
    std::fs::read_to_string(input).map_err(|error| {
        Diagnostic::error(
            DiagCode::Io,
            format!("cannot read {}: {error}", input.display()),
        )
    })
}

/// Parse `source`, recording `input` as the diagnostic-only source path.
///
/// Every parser feature the toolchain documents is on: GFM, math and
/// frontmatter.
pub fn parse_document(source: &str, input: &Path) -> (Doc, Report) {
    let options = ParseOptions {
        gfm: true,
        math: true,
        frontmatter: true,
    };
    let (mut doc, report) = mdx_parse::parse(source, &options);
    doc.source_path = Some(input.display().to_string());
    (doc, report)
}

/// Read, parse and resolve assets for `input`.
pub fn load(input: &Path, policy: &AssetPolicy) -> Result<Loaded, Diagnostic> {
    let source = read_source(input)?;
    Ok(load_source(&source, input, policy))
}

/// Parse `source` and resolve its assets (the inner half of [`load`]).
pub fn load_source(source: &str, input: &Path, policy: &AssetPolicy) -> Loaded {
    let (mut doc, mut report) = parse_document(source, input);
    let (assets, asset_report) = mdx_assets::resolve_all(&mut doc, &doc_dir(input), policy);
    report.merge_from(asset_report);
    Loaded {
        doc,
        assets,
        report,
    }
}

/// Write rendered `bytes` to `path`; failures carry `E_IO` and the path.
pub fn write_output(path: &Path, bytes: &[u8]) -> Result<(), Diagnostic> {
    std::fs::write(path, bytes).map_err(|error| {
        Diagnostic::error(
            DiagCode::Io,
            format!("cannot write {}: {error}", path.display()),
        )
    })
}

/// Note `--font` paths that do not exist.
///
/// Informational but visible: the font searchers simply never find such a file,
/// so a render must not depend on fonts the document does not need. The paths
/// are still handed to the writers unchanged, which keeps them the single
/// authority on what they could load.
pub fn note_missing_fonts(fonts: &[PathBuf], report: &mut Report) {
    for font in fonts {
        if !font.is_file() {
            report.push(Diagnostic::warning(
                DiagCode::Config,
                format!("font file {} does not exist; ignoring it", font.display()),
            ));
        }
    }
}

/// `latex -> Typst expression` for every formula in `doc`.
///
/// Formulas the Typst converter rejects fall back to RaTeX
/// ([`mdx_math::latex_to_svg`]) to find out whether they are drawable at all. The
/// fallback produces an SVG for a different pipeline than [`MathMap`] feeds, so
/// the formula is reported as an `E_MATH_CONVERT` warning and left out of the
/// map: the Typst writer then passes the original LaTeX through instead of
/// failing.
pub fn math_map(doc: &Doc, jobs: u32, report: &mut Report) -> MathMap {
    let formulas: Vec<String> = collect_math(doc).into_iter().collect();
    let outcomes = parallel_map(&formulas, jobs, |latex: &String| convert_math(latex));
    let mut math = MathMap::new();
    for (latex, outcome) in formulas.iter().zip(outcomes) {
        match outcome {
            MathOutcome::Typst(expression) => {
                math.insert(latex.clone(), expression);
            }
            MathOutcome::FallbackOnly(reason) => report.push(Diagnostic::warning(
                DiagCode::MathConvert,
                format!(
                    "math `{}` has no Typst equivalent ({reason}); the RaTeX fallback renders it, \
                     this document keeps the source LaTeX",
                    excerpt(latex)
                ),
            )),
            MathOutcome::Failed(reason) => report.push(Diagnostic::warning(
                DiagCode::MathConvert,
                format!("math `{}` could not be converted: {reason}", excerpt(latex)),
            )),
        }
    }
    math
}

/// `Mermaid source -> SVG` for every diagram in `doc`.
///
/// A diagram that cannot be rendered is an `E_DIAGRAM_RENDER` error: the
/// document would silently lose a figure otherwise.
pub fn diagram_map(
    doc: &Doc,
    jobs: u32,
    options: &DiagramOptions,
    report: &mut Report,
) -> DiagramMap {
    let sources: Vec<String> = collect_diagrams(doc).into_iter().collect();
    let rendered = parallel_map(&sources, jobs, |source: &String| {
        mdx_diagram::render_svg(source, options)
    });
    let mut diagrams = DiagramMap::new();
    for (source, result) in sources.iter().zip(rendered) {
        match result {
            Ok(svg) => {
                diagrams.insert(source.clone(), svg);
            }
            Err(message) => report.push(Diagnostic::error(
                DiagCode::DiagramRender,
                format!(
                    "diagram `{}` could not be rendered: {message}",
                    excerpt(source)
                ),
            )),
        }
    }
    diagrams
}

/// Render `doc` to PDF bytes, recording an `E_TYPST` failure in `report`.
pub fn render_pdf(
    doc: &Doc,
    assets: &AssetStore,
    math: &MathMap,
    diagrams: &DiagramMap,
    options: &RenderOptions,
    report: &mut Report,
) -> Option<Vec<u8>> {
    match mdx_typst::render_pdf(doc, assets, math, diagrams, options) {
        Ok(bytes) => Some(bytes),
        Err(diagnostic) => {
            report.push(diagnostic);
            None
        }
    }
}

/// Render `doc` to one self-contained HTML document, recording failures.
pub fn render_html(
    doc: &Doc,
    assets: &AssetStore,
    math: &MathMap,
    diagrams: &DiagramMap,
    options: &HtmlOptions,
    report: &mut Report,
) -> Option<String> {
    match mdx_html::render_html(doc, assets, math, diagrams, options) {
        Ok(html) => Some(html),
        Err(diagnostic) => {
            report.push(diagnostic);
            None
        }
    }
}

/// Every math expression in `doc`, deduplicated and sorted.
pub fn collect_math(doc: &Doc) -> BTreeSet<String> {
    let mut formulas = BTreeSet::new();
    collect_block_math(&doc.blocks, &mut formulas);
    formulas
}

/// Every Mermaid source in `doc`, deduplicated and sorted.
pub fn collect_diagrams(doc: &Doc) -> BTreeSet<String> {
    let mut sources = BTreeSet::new();
    collect_block_diagrams(&doc.blocks, &mut sources);
    sources
}

/// Outcome of converting one formula, in the order the fallbacks are tried.
enum MathOutcome {
    /// Typst can typeset the formula; the payload is the math expression body.
    Typst(String),
    /// Typst cannot express the formula but RaTeX can draw it; the payload is
    /// the Typst converter's complaint.
    FallbackOnly(String),
    /// Neither converter handled the formula; the payload explains both.
    Failed(String),
}

/// Convert `latex` to a Typst math expression, with the RaTeX fallback.
fn convert_math(latex: &str) -> MathOutcome {
    match mdx_math::latex_to_typst(latex) {
        Ok(expression) => MathOutcome::Typst(expression),
        Err(typst_error) => match mdx_math::latex_to_svg(latex) {
            Ok(_svg) => MathOutcome::FallbackOnly(typst_error),
            Err(svg_error) => {
                MathOutcome::Failed(format!("{typst_error}; RaTeX fallback: {svg_error}"))
            }
        },
    }
}

/// Run `conversion` over `items`, using at most `jobs` worker threads.
///
/// Results keep the order of `items`, so callers can zip them back. `jobs` is
/// clamped to the number of items, and `jobs <= 1` runs inline. A worker thread
/// that dies must not take the run down with it: its slice is converted again on
/// the calling thread, which is also where the panics of a broken converter would
/// surface.
fn parallel_map<T, R, F>(items: &[T], jobs: u32, conversion: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let workers = (jobs.max(1) as usize).min(items.len());
    if workers <= 1 {
        return items.iter().map(&conversion).collect();
    }

    let chunk = items.len().div_ceil(workers);
    let mut converted: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    std::thread::scope(|scope| {
        let conversion = &conversion;
        let mut handles = Vec::with_capacity(workers);
        for (chunk_index, slice) in items.chunks(chunk).enumerate() {
            let start = chunk_index * chunk;
            handles.push(scope.spawn(move || {
                let mut done = Vec::with_capacity(slice.len());
                for (offset, item) in slice.iter().enumerate() {
                    done.push((start + offset, conversion(item)));
                }
                done
            }));
        }
        for handle in handles {
            let Ok(done) = handle.join() else { continue };
            for (index, value) in done {
                if let Some(slot) = converted.get_mut(index) {
                    *slot = Some(value);
                }
            }
        }
    });

    for (index, slot) in converted.iter_mut().enumerate() {
        if slot.is_none()
            && let Some(item) = items.get(index)
        {
            *slot = Some(conversion(item));
        }
    }
    converted.into_iter().flatten().collect()
}

/// Collect math from `blocks` and everything nested inside them.
fn collect_block_math(blocks: &[Block], formulas: &mut BTreeSet<String>) {
    for block in blocks {
        match block {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines } => {
                collect_inline_math(inlines, formulas);
            }
            Block::Math { latex, .. } => {
                formulas.insert(latex.clone());
            }
            Block::List { items, .. } => {
                for item in items {
                    collect_block_math(item, formulas);
                }
            }
            Block::BlockQuote { blocks } | Block::FootnoteDef { blocks, .. } => {
                collect_block_math(blocks, formulas);
            }
            Block::Table { header, rows, .. } => {
                for cell in header {
                    collect_inline_math(cell, formulas);
                }
                for row in rows {
                    for cell in row {
                        collect_inline_math(cell, formulas);
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

/// Collect math from inline content, descending into styled runs and links.
fn collect_inline_math(inlines: &[Inline], formulas: &mut BTreeSet<String>) {
    for inline in inlines {
        match inline {
            Inline::Math { latex, .. } => {
                formulas.insert(latex.clone());
            }
            Inline::Emph { inlines }
            | Inline::Strong { inlines }
            | Inline::Strike { inlines }
            | Inline::Link { inlines, .. } => collect_inline_math(inlines, formulas),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::Image { .. }
            | Inline::FootnoteRef { .. }
            | Inline::LineBreak { .. } => {}
        }
    }
}

/// Collect Mermaid sources from `blocks` and everything nested inside them.
fn collect_block_diagrams(blocks: &[Block], sources: &mut BTreeSet<String>) {
    for block in blocks {
        match block {
            Block::Diagram { source } => {
                sources.insert(source.clone());
            }
            Block::List { items, .. } => {
                for item in items {
                    collect_block_diagrams(item, sources);
                }
            }
            Block::BlockQuote { blocks } | Block::FootnoteDef { blocks, .. } => {
                collect_block_diagrams(blocks, sources);
            }
            _ => {}
        }
    }
}

/// First non-blank line of `text`, trimmed and clipped for a diagnostic message.
fn excerpt(text: &str) -> String {
    let line = match text.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => line,
        None => return String::new(),
    };
    match line.char_indices().nth(EXCERPT_CHARS) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_string(),
    }
}

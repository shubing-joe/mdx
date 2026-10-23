//! `mdx-bench` — in-process per-stage timings for the `mdx` render pipeline.
//!
//! The binary runs the same sequence `mdx render` runs — parse → resolve assets
//! → convert math → render diagrams → write PDF — but keeps every stage in
//! process and reports how long each one took, which the process-level
//! `scripts/bench.sh` cannot see.
//!
//! Determinism: the work items of the conversion stages are collected into
//! [`BTreeSet`]s, exactly as `mdx-cli`'s pipeline does, so repeated runs convert
//! the same items in the same order. No clock, environment or random source is
//! read: the only date the renderer can see is the fixed default date of
//! [`mdx_typst::world`].
//!
//! Usage:
//!
//! ```text
//! mdx-bench <INPUT.md> [--runs N] [--jobs N] [--toc] [--offline]
//! ```
//!
//! Exit codes: `0` success, `1` a render stage failed, `2` bad usage or an
//! unreadable input.

use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use mdx_assets::AssetPolicy;
use mdx_core::{Block, Doc, Inline};
use mdx_diagram::DiagramOptions;
use mdx_parse::ParseOptions;
use mdx_typst::{DiagramMap, MathMap, PageSetup, Paper, RenderOptions};

/// Largest asset payload accepted; mirrors `mdx-cli`'s `MAX_ASSET_BYTES`.
///
/// [`mdx_assets::AssetPolicy`] reads `0` as "unlimited"; a document tool wants a
/// bound so an accidental link to a huge file cannot exhaust memory.
// API: crates/mdx-cli/src/pipeline.rs — MAX_ASSET_BYTES
const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

/// Page margin of the default page set-up, in millimetres (CLI default).
// API: crates/mdx-cli/src/pipeline.rs — DEFAULT_MARGIN_MM
const DEFAULT_MARGIN_MM: f32 = 20.0;

/// Default number of timed runs per configuration.
const DEFAULT_RUNS: u32 = 3;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("mdx-bench: {failure}");
            ExitCode::from(failure.exit_code())
        }
    }
}

/// Why the benchmark could not produce a report.
enum Failure {
    /// The command line was wrong.
    Usage(String),
    /// The input could not be read.
    Io(String),
    /// A render stage failed on the document.
    Stage(String),
}

impl Failure {
    /// Exit code this failure maps to (`2` for caller mistakes, `1` for a
    /// document that does not render).
    fn exit_code(&self) -> u8 {
        match self {
            Failure::Usage(_) | Failure::Io(_) => 2,
            Failure::Stage(_) => 1,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Usage(message) | Failure::Io(message) | Failure::Stage(message) => {
                formatter.write_str(message)
            }
        }
    }
}

/// The configuration one invocation runs with.
struct Options {
    /// Document to render.
    input: PathBuf,
    /// Timed runs per configuration.
    runs: u32,
    /// Worker threads for the math and diagram stages.
    jobs: u32,
    /// Whether the PDF gets a table of contents.
    toc: bool,
    /// Whether remote assets are refused.
    offline: bool,
}

/// Per-stage wall-clock samples, in milliseconds, one entry per run.
#[derive(Default)]
struct Samples {
    parse: Vec<f64>,
    assets: Vec<f64>,
    math: Vec<f64>,
    diagram: Vec<f64>,
    pdf: Vec<f64>,
    total: Vec<f64>,
}

/// What the last run observed about the document.
struct Facts {
    /// Size of the rendered PDF, in bytes.
    pdf_bytes: usize,
    /// Blocks in the document, nested ones included.
    blocks: usize,
    /// Unique formulas converted by the math stage.
    formulas: usize,
    /// Formulas the Typst converter rejected and that the writer therefore
    /// passes through as raw source.
    math_failed: usize,
    /// Unique Mermaid sources converted by the diagram stage.
    diagrams: usize,
    /// Diagrams whose rendering failed.
    diagram_failed: usize,
}

/// Parses `args` (without the program name) and runs the benchmark.
///
/// `Ok(())` covers the help path as well: `--help` is a successful invocation.
fn run() -> Result<(), Failure> {
    let Some(options) = parse_args(env::args().skip(1))? else {
        print!("{USAGE}");
        return Ok(());
    };

    let source = std::fs::read_to_string(&options.input).map_err(|error| {
        Failure::Io(format!(
            "cannot read {}: {error}",
            options.input.display()
        ))
    })?;
    let dir = doc_dir(&options.input);

    let mut samples = Samples::default();
    let mut facts = None;
    for run_index in 0..options.runs {
        let observed = run_once(&source, &options, &dir, &mut samples)?;
        if run_index + 1 == options.runs {
            facts = Some(observed);
        }
    }
    let facts = match facts {
        Some(facts) => facts,
        None => return Err(Failure::Usage("--runs must be at least 1".to_string())),
    };

    report(&options, source.len(), &samples, &facts);
    Ok(())
}

/// Normalises the argument list: `Some(options)` to benchmark, `None` after
/// `--help`.
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Options>, Failure> {
    let mut input: Option<PathBuf> = None;
    let mut runs = DEFAULT_RUNS;
    let mut jobs = 1u32;
    let mut toc = false;
    let mut offline = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--toc" => toc = true,
            "--offline" => offline = true,
            flag if flag == "--runs" || flag.starts_with("--runs=") => {
                runs = positive(&flag_value(flag, &mut args)?, "--runs")?;
            }
            flag if flag == "--jobs" || flag.starts_with("--jobs=") => {
                jobs = positive(&flag_value(flag, &mut args)?, "--jobs")?;
            }
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(Failure::Usage(format!("unknown flag `{flag}`")));
            }
            _ => {
                if input.is_some() {
                    return Err(Failure::Usage(format!(
                        "expected one input document, got a second one (`{arg}`)"
                    )));
                }
                input = Some(PathBuf::from(arg));
            }
        }
    }

    let input = input.ok_or_else(|| Failure::Usage("an input document is required".to_string()))?;
    Ok(Some(Options {
        input,
        runs,
        jobs,
        toc,
        offline,
    }))
}

/// The value of `--flag VALUE` or `--flag=VALUE`.
fn flag_value(flag: &str, args: &mut impl Iterator<Item = String>) -> Result<String, Failure> {
    if let Some(value) = flag.split_once('=').map(|(_, value)| value) {
        return Ok(value.to_string());
    }
    let name = flag.to_string();
    args.next()
        .ok_or_else(|| Failure::Usage(format!("`{name}` needs a value")))
}

/// Parses a `u32` that must be at least 1.
fn positive(value: &str, flag: &str) -> Result<u32, Failure> {
    match value.parse::<u32>() {
        Ok(number) if number >= 1 => Ok(number),
        _ => Err(Failure::Usage(format!(
            "`{flag}` needs a whole number of at least 1, got `{value}`"
        ))),
    }
}

/// Directory relative asset paths resolve against — the input's own directory,
/// or `.` for a bare file name.
// API: crates/mdx-cli/src/pipeline.rs — doc_dir
fn doc_dir(input: &Path) -> PathBuf {
    match input.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Runs every stage once, recording each stage's wall-clock time.
fn run_once(
    source: &str,
    options: &Options,
    dir: &Path,
    samples: &mut Samples,
) -> Result<Facts, Failure> {
    let run_start = Instant::now();

    let started = Instant::now();
    let (mut doc, _report) = mdx_parse::parse(
        source,
        &ParseOptions {
            gfm: true,
            math: true,
            frontmatter: true,
        },
    );
    doc.source_path = Some(options.input.display().to_string());
    samples.parse.push(millis(started.elapsed()));

    let started = Instant::now();
    let policy = AssetPolicy {
        offline: options.offline,
        allow_remote: true,
        cache_dir: None,
        max_bytes: MAX_ASSET_BYTES,
    };
    let (assets, _asset_report) = mdx_assets::resolve_all(&mut doc, dir, &policy);
    samples.assets.push(millis(started.elapsed()));

    let started = Instant::now();
    let (math, math_failed) = math_map(&doc, options.jobs);
    samples.math.push(millis(started.elapsed()));

    let started = Instant::now();
    let diagram_options = DiagramOptions {
        deterministic_text: true,
        font_files: Vec::new(),
    };
    let (diagrams, diagram_failed) = diagram_map(&doc, options.jobs, &diagram_options);
    samples.diagram.push(millis(started.elapsed()));

    let started = Instant::now();
    let render_options = RenderOptions {
        page: PageSetup {
            paper: Paper::A4,
            margin_mm: DEFAULT_MARGIN_MM,
            numbered: true,
        },
        toc: options.toc,
        pdf_standard: None,
        fonts: Vec::new(),
        // The renderer never reads the host font store — the bundled
        // `typst-assets` fonts plus `fonts` are the whole font book — so this
        // flag only records intent and cannot skew the timings.
        ignore_system_fonts: true,
        deterministic_today: None,
        title: doc.front.title.clone(),
    };
    let pdf = mdx_typst::render_pdf(&doc, &assets, &math, &diagrams, &render_options)
        .map_err(|diagnostic| {
            Failure::Stage(format!("typst rejected the document: {diagnostic}"))
        })?;
    samples.pdf.push(millis(started.elapsed()));
    samples.total.push(millis(run_start.elapsed()));

    Ok(Facts {
        pdf_bytes: pdf.len(),
        blocks: count_blocks(&doc.blocks),
        formulas: math.len(),
        math_failed,
        diagrams: diagrams.len(),
        diagram_failed,
    })
}

/// `latex -> Typst expression` for every unique formula in `doc`.
///
/// Formulas the Typst converter rejects are counted and left out of the map, so
/// the writer passes their raw source through instead of failing — the same
/// policy `mdx-cli` applies, minus the RaTeX fallback probe this benchmark has
/// no use for.
fn math_map(doc: &Doc, jobs: u32) -> (MathMap, usize) {
    let formulas: Vec<String> = collect_math(doc).into_iter().collect();
    let converted = parallel_map(&formulas, jobs, |latex: &String| {
        mdx_math::latex_to_typst(latex).ok()
    });
    let mut math = MathMap::new();
    let mut failed = 0usize;
    for (latex, expression) in formulas.iter().zip(converted) {
        match expression {
            Some(expression) => {
                math.insert(latex.clone(), expression);
            }
            None => failed += 1,
        }
    }
    (math, failed)
}

/// `Mermaid source -> SVG` for every unique diagram in `doc`.
fn diagram_map(doc: &Doc, jobs: u32, options: &DiagramOptions) -> (DiagramMap, usize) {
    let sources: Vec<String> = collect_diagrams(doc).into_iter().collect();
    let rendered = parallel_map(&sources, jobs, |source: &String| {
        mdx_diagram::render_svg(source, options)
    });
    let mut diagrams = DiagramMap::new();
    let mut failed = 0usize;
    for (source, result) in sources.iter().zip(rendered) {
        match result {
            Ok(svg) => {
                diagrams.insert(source.clone(), svg);
            }
            Err(_) => failed += 1,
        }
    }
    (diagrams, failed)
}

/// Runs `conversion` over `items` with at most `jobs` worker threads, keeping
/// the order of `items`.
///
/// `jobs` is clamped to the number of items and `jobs <= 1` runs inline. A
/// worker that dies must not take the run down with it: its slice is converted
/// again on the calling thread.
// API: crates/mdx-cli/src/pipeline.rs — parallel_map
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

/// Every math expression in `doc`, deduplicated and sorted.
fn collect_math(doc: &Doc) -> BTreeSet<String> {
    let mut formulas = BTreeSet::new();
    collect_block_math(&doc.blocks, &mut formulas);
    formulas
}

/// Every Mermaid source in `doc`, deduplicated and sorted.
fn collect_diagrams(doc: &Doc) -> BTreeSet<String> {
    let mut sources = BTreeSet::new();
    collect_block_diagrams(&doc.blocks, &mut sources);
    sources
}

/// Collects math from `blocks` and everything nested inside them.
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

/// Collects math from inline content, descending into styled runs and links.
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

/// Collects Mermaid sources from `blocks` and everything nested inside them.
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

/// Number of blocks in `blocks`, nested ones included.
fn count_blocks(blocks: &[Block]) -> usize {
    let mut total = 0usize;
    for block in blocks {
        total += 1;
        match block {
            Block::List { items, .. } => {
                for item in items {
                    total += count_blocks(item);
                }
            }
            Block::BlockQuote { blocks } | Block::FootnoteDef { blocks, .. } => {
                total += count_blocks(blocks);
            }
            _ => {}
        }
    }
    total
}

/// Milliseconds in `duration`, as a float.
fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// `(median, min, max)` of `samples`, all zero when there are none.
fn stats(samples: &[f64]) -> (f64, f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    let middle = sorted.len() / 2;
    let median = if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    };
    let min = sorted.first().copied().unwrap_or_default();
    let max = sorted.last().copied().unwrap_or_default();
    (median, min, max)
}

/// Prints the report table.
fn report(options: &Options, source_bytes: usize, samples: &Samples, facts: &Facts) {
    println!(
        "mdx-bench: {} ({} bytes, {} blocks, {} formulas, {} diagrams)",
        options.input.display(),
        source_bytes,
        facts.blocks,
        facts.formulas,
        facts.diagrams
    );
    println!(
        "runs: {}   jobs: {}   toc: {}   offline: {}",
        options.runs,
        options.jobs,
        yes_no(options.toc),
        yes_no(options.offline)
    );
    // The diagram stage goes through `mdx_diagram::render_svg`, which reads and
    // writes an advisory on-disk cache when its options carry no font files.
    // With the cache live, run 1 renders the diagrams and runs 2..N read SVGs
    // from disk, so the median describes neither. `std::env::set_var` is unsafe
    // in edition 2024, so the benchmark cannot turn it off itself and says so
    // instead.
    if env::var_os("MDX_CACHE_DIR").is_none() {
        println!(
            "note: MDX_CACHE_DIR is unset - if the diagram cache is enabled, runs after the \
             first reuse cached SVGs; set MDX_CACHE_DIR to a path that does not exist for \
             clean per-render numbers"
        );
    }
    println!();
    println!("{:<10} {:>12} {:>12} {:>12}", "stage", "median ms", "min ms", "max ms");
    for (name, values) in [
        ("parse", &samples.parse),
        ("assets", &samples.assets),
        ("math", &samples.math),
        ("diagram", &samples.diagram),
        ("typst-pdf", &samples.pdf),
    ] {
        let (median, min, max) = stats(values);
        println!("{name:<10} {median:>12.3} {min:>12.3} {max:>12.3}");
    }
    println!("{:<10} {:>12} {:>12} {:>12}", "-", "-", "-", "-");
    let (median, min, max) = stats(&samples.total);
    println!("{:<10} {median:>12.3} {min:>12.3} {max:>12.3}", "total");
    println!();
    println!("pdf: {} bytes (last run)", facts.pdf_bytes);
    if facts.math_failed > 0 || facts.diagram_failed > 0 {
        println!(
            "failed conversions: {} math, {} diagrams",
            facts.math_failed, facts.diagram_failed
        );
    }
}

/// `yes`/`no` for a boolean flag column.
fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Usage text printed by `--help`.
const USAGE: &str = "\
mdx-bench — per-stage timings of the mdx render pipeline, in process

Usage: mdx-bench <INPUT.md> [--runs N] [--jobs N] [--toc] [--offline]

Arguments:
  <INPUT.md>     Markdown document to render (assets resolve against its directory)

Options:
  --runs N       Timed runs per configuration, median reported [default: 3]
  --jobs N       Worker threads for the math and diagram stages [default: 1]
  --toc          Add a table of contents to the PDF
  --offline      Refuse remote assets (they fail with E_ASSET_OFFLINE)
  -h, --help     Print this help

The reported stages are parse, assets, math, diagram and typst-pdf. Build with
`--release`: a debug build measures unoptimised fractions of the real pipeline.
";

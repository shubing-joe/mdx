//! `mdx` — command line front end of the `mdx` Markdown toolchain.
//!
//! Six subcommands sit on one pipeline (see [`pipeline`]):
//!
//! * `render` writes a PDF or a self-contained HTML document,
//! * `check` parses a document and resolves its assets without writing output,
//! * `ast` prints the parsed document model as JSON,
//! * `assets` resolves assets and prints the asset manifest,
//! * `outline` prints the heading tree of a document,
//! * `watch` repeats the `render` build whenever the input file changes.
//!
//! Exit codes come from `mdx-core`: `0` success, `1` parse/render failure,
//! `2` usage or config error, `3` unresolved assets. `--json` replaces stdout
//! with a single object `{"ok","exit","diagnostics","output","manifest"}`
//! (`"ast"` is added by `mdx ast`); without it, diagnostics go to stderr as
//! `CODE: message` and the command's result — written path, document model or
//! manifest — goes to stdout. `mdx outline --json` prints its array instead of
//! that envelope, and `mdx watch` prints one status line per build,
//! `built <path> in <ms> ms` or `error <CODE>: <message>`.

mod pipeline;
mod watch;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand, ValueEnum};
use mdx_core::{DiagCode, Diagnostic, Doc, Report, Severity, TocEntry};
use serde::Serialize;

fn main() {
    let cli = Cli::parse();
    let exit = match cli.command {
        Command::Render(args) => render(args).finish(),
        Command::Check(args) => check(args).finish(),
        Command::Ast(args) => ast(args).finish(),
        Command::Assets(args) => assets(args).finish(),
        Command::Outline(args) => outline(args),
        Command::Watch(args) => watch_command(args),
    };
    std::process::exit(exit);
}

/// `mdx` command line interface.
#[derive(Debug, Parser)]
#[command(
    name = "mdx",
    version,
    about = "Render Markdown documents to PDF or HTML",
    long_about = "Converts a Markdown document to a PDF (Typst) or self-contained HTML, \
                  resolving images, LaTeX math and Mermaid diagrams on the way. \
                  `mdx watch` repeats that build whenever the input file changes, \
                  `mdx outline` prints the heading tree."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The six operations `mdx` offers.
#[derive(Debug, Subcommand)]
enum Command {
    /// Render a document to PDF or HTML.
    Render(RenderArgs),
    /// Parse a document and resolve its assets without writing output.
    Check(DocumentArgs),
    /// Print the parsed document model as JSON.
    Ast(DocumentArgs),
    /// Resolve assets and print the asset manifest as JSON.
    Assets(DocumentArgs),
    /// Print the document's table of contents.
    Outline(OutlineArgs),
    /// Rebuild a document whenever its file changes.
    Watch(WatchArgs),
}

/// Build flags shared by `mdx render` and `mdx watch`.
///
/// `mdx watch` accepts every one of them so a watched build is exactly the
/// build `mdx render` would produce; the defaults are the same, too.
#[derive(Debug, Args)]
struct BuildArgs {
    /// Where to write the result (default: the input path with the new extension).
    #[arg(short = 'o', long = "out", value_name = "OUT")]
    out: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Pdf)]
    format: Format,

    /// Add a table of contents (PDF).
    #[arg(long)]
    toc: bool,

    /// Page size.
    #[arg(long, value_enum, default_value_t = PaperArg::A4)]
    paper: PaperArg,

    /// Enforce a PDF conformance standard.
    #[arg(long, value_enum)]
    pdf_standard: Option<PdfStandardArg>,

    /// Packaged font file to use; repeatable.
    #[arg(long = "font", value_name = "PATH")]
    fonts: Vec<PathBuf>,

    /// Ignore the fonts installed on this system.
    #[arg(long)]
    ignore_system_fonts: bool,

    /// Never read remote assets; they fail with `E_ASSET_OFFLINE`.
    #[arg(long)]
    offline: bool,

    /// Reproducible output: fixed build date and deterministic diagram text.
    #[arg(long)]
    deterministic: bool,

    /// Maximum number of conversion workers.
    #[arg(
        long,
        value_name = "N",
        default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    jobs: u32,

    /// Write the generated Typst markup to this path (debugging aid).
    #[arg(long = "dump-typst", value_name = "PATH")]
    dump_typst: Option<PathBuf>,
}

/// Arguments of `mdx render`.
#[derive(Debug, Args)]
struct RenderArgs {
    /// Markdown document to render.
    #[arg(value_name = "INPUT")]
    input: PathBuf,

    #[command(flatten)]
    build: BuildArgs,

    /// Print one machine-readable JSON object instead of the default output.
    #[arg(long)]
    json: bool,
}

/// Arguments of `mdx outline`.
#[derive(Debug, Args)]
struct OutlineArgs {
    /// Markdown document to outline.
    #[arg(value_name = "INPUT")]
    input: PathBuf,

    /// Print the table of contents as a JSON array instead of an indented tree.
    #[arg(long)]
    json: bool,
}

/// Arguments of `mdx watch`.
#[derive(Debug, Args)]
struct WatchArgs {
    /// Markdown document to watch.
    #[arg(value_name = "INPUT")]
    input: PathBuf,

    #[command(flatten)]
    build: BuildArgs,

    /// Milliseconds the input has to stay unchanged before a rebuild starts.
    #[arg(
        long = "debounce-ms",
        value_name = "N",
        default_value_t = watch::DEFAULT_DEBOUNCE_MS
    )]
    debounce_ms: u64,

    /// Build once and exit instead of watching the file.
    #[arg(long)]
    once: bool,
}

/// Arguments shared by `mdx check`, `mdx ast` and `mdx assets`.
#[derive(Debug, Args)]
struct DocumentArgs {
    /// Markdown document to read.
    #[arg(value_name = "INPUT")]
    input: PathBuf,

    /// Print one machine-readable JSON object instead of the default output.
    #[arg(long)]
    json: bool,
}

/// Output format of `mdx render`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    /// PDF via Typst.
    #[value(name = "pdf")]
    Pdf,
    /// Self-contained HTML.
    #[value(name = "html")]
    Html,
}

impl Format {
    /// File extension of the output this format writes.
    fn extension(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Html => "html",
        }
    }
}

impl std::fmt::Display for Format {
    /// Writes the value name `--format` accepts, so the clap default roundtrips.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.extension())
    }
}

/// Page size of the PDF layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PaperArg {
    /// ISO A4 (210 x 297 mm).
    #[value(name = "a4")]
    A4,
    /// US Letter (8.5 x 11 in).
    #[value(name = "letter")]
    Letter,
}

impl PaperArg {
    /// The `mdx-typst` page size this flag selects.
    fn to_typst(self) -> mdx_typst::Paper {
        match self {
            PaperArg::A4 => mdx_typst::Paper::A4,
            PaperArg::Letter => mdx_typst::Paper::Letter,
        }
    }

    /// The value name `--paper` accepts.
    fn name(self) -> &'static str {
        match self {
            PaperArg::A4 => "a4",
            PaperArg::Letter => "letter",
        }
    }
}

impl std::fmt::Display for PaperArg {
    /// Writes the value name `--paper` accepts, so the clap default roundtrips.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// PDF conformance standard of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PdfStandardArg {
    /// PDF/A-2b archival profile.
    #[value(name = "a-2b")]
    A2b,
    /// PDF/UA-1 accessibility profile.
    #[value(name = "ua-1")]
    Ua1,
}

impl PdfStandardArg {
    /// The `mdx-typst` standard this flag selects.
    ///
    // API: typst-pdf 0.15.1 spells these `PdfStandard::A_2b` and
    // `PdfStandard::Ua_1`; the type is re-exported by `mdx-typst` because
    // `mdx-cli` does not depend on `typst-pdf` itself.
    fn to_typst(self) -> mdx_typst::PdfStandard {
        match self {
            PdfStandardArg::A2b => mdx_typst::PdfStandard::A_2b,
            PdfStandardArg::Ua1 => mdx_typst::PdfStandard::Ua_1,
        }
    }
}

/// Render one document to PDF or HTML.
fn render(args: RenderArgs) -> Outcome {
    let RenderArgs { input, build, json } = args;
    build_document(&build, &input, json)
}

/// Build one document — read, resolve, render, write — reporting nothing.
///
/// `mdx render` prints the returned [`Outcome`]; `mdx watch` consumes it and
/// reports one status line per rebuild. Both go through here, so a watched
/// build cannot drift from the build `mdx render` performs. The output path is
/// [`default_output`] unless `-o` overrode it.
fn build_document(args: &BuildArgs, input: &Path, json: bool) -> Outcome {
    if !input.exists() {
        return Outcome::usage(
            json,
            format!("input file {} does not exist", input.display()),
        );
    }
    let loaded = match pipeline::load(input, &pipeline::asset_policy(args.offline)) {
        Ok(loaded) => loaded,
        Err(diagnostic) => return Outcome::error(json, diagnostic),
    };
    let pipeline::Loaded {
        doc,
        assets,
        mut report,
    } = loaded;

    // A document whose assets did not resolve is never written out: the exit
    // code already says the build failed, and a partial file would look done.
    if report.has_errors() {
        return Outcome::from_report(
            report,
            json,
            Stdout::Silent,
            None,
            Some(assets.to_json()),
            None,
        );
    }

    let format = args.format;
    pipeline::note_missing_fonts(&args.fonts, &mut report);
    let math = pipeline::math_map(&doc, args.jobs, &mut report);
    let diagram_options = mdx_diagram::DiagramOptions {
        deterministic_text: args.deterministic,
        font_files: args.fonts.clone(),
    };
    let diagrams = pipeline::diagram_map(&doc, args.jobs, &diagram_options, &mut report);
    // PDF/A and PDF/UA require a document title; fall back to the file stem so
    // `--pdf-standard ua-1` works on documents without front matter.
    let document_title = doc.front.title.clone().or_else(|| {
        input
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_owned)
    });
    let path = match &args.out {
        Some(path) => path.clone(),
        None => default_output(input, format),
    };

    let written = match format {
        Format::Pdf => {
            let options = mdx_typst::RenderOptions {
                page: mdx_typst::PageSetup {
                    paper: args.paper.to_typst(),
                    margin_mm: pipeline::DEFAULT_MARGIN_MM,
                    numbered: true,
                },
                toc: args.toc,
                pdf_standard: args.pdf_standard.map(PdfStandardArg::to_typst),
                fonts: args.fonts.clone(),
                ignore_system_fonts: args.ignore_system_fonts,
                deterministic_today: args
                    .deterministic
                    .then(|| pipeline::DETERMINISTIC_TODAY.to_string()),
                title: document_title.clone(),
            };
            if let Some(path) = args.dump_typst.as_deref() {
                let source = mdx_typst::writer::build(&doc, &assets, &math, &diagrams, &options);
                if let Err(error) = std::fs::write(path, source) {
                    report.push(mdx_core::Diagnostic::error(
                        mdx_core::DiagCode::Io,
                        format!("cannot write {}: {error}", path.display()),
                    ));
                }
            }
            pipeline::render_pdf(&doc, &assets, &math, &diagrams, &options, &mut report)
                .map(|bytes| pipeline::write_output(&path, &bytes))
        }
        Format::Html => {
            // Assets are embedded, so the HTML file needs nothing beside it.
            let options = mdx_html::HtmlOptions {
                embed_assets: true,
                css: None,
                title: document_title.clone(),
            };
            pipeline::render_html(&doc, &assets, &math, &diagrams, &options, &mut report)
                .map(|html| pipeline::write_output(&path, html.as_bytes()))
        }
    };

    let output = match written {
        Some(Ok(())) => Some(path.display().to_string()),
        Some(Err(diagnostic)) => {
            report.push(diagnostic);
            None
        }
        None => None,
    };
    Outcome::from_report(
        report,
        json,
        Stdout::Path,
        output,
        Some(assets.to_json()),
        None,
    )
}

/// Parse a document and resolve its assets; nothing is written.
fn check(args: DocumentArgs) -> Outcome {
    let DocumentArgs { input, json } = args;

    if !input.exists() {
        return Outcome::usage(
            json,
            format!("input file {} does not exist", input.display()),
        );
    }
    let loaded = match pipeline::load(&input, &pipeline::asset_policy(false)) {
        Ok(loaded) => loaded,
        Err(diagnostic) => return Outcome::error(json, diagnostic),
    };
    let pipeline::Loaded {
        doc,
        assets,
        mut report,
    } = loaded;

    // Math conversion is the only later stage cheap enough to run here; it is
    // what surfaces `E_MATH_CONVERT` before anyone tries to render.
    pipeline::math_map(&doc, 1, &mut report);
    Outcome::from_report(
        report,
        json,
        Stdout::Silent,
        None,
        Some(assets.to_json()),
        None,
    )
}

/// Print the parsed document model.
fn ast(args: DocumentArgs) -> Outcome {
    let DocumentArgs { input, json } = args;

    if !input.exists() {
        return Outcome::usage(
            json,
            format!("input file {} does not exist", input.display()),
        );
    }
    let source = match pipeline::read_source(&input) {
        Ok(source) => source,
        Err(diagnostic) => return Outcome::error(json, diagnostic),
    };

    // `ast` shows what the parser produced, so asset references stay unresolved.
    let (doc, report) = pipeline::parse_document(&source, &input);
    Outcome::from_report(report, json, Stdout::Doc, None, None, Some(doc))
}

/// Resolve assets and print the manifest.
fn assets(args: DocumentArgs) -> Outcome {
    let DocumentArgs { input, json } = args;

    if !input.exists() {
        return Outcome::usage(
            json,
            format!("input file {} does not exist", input.display()),
        );
    }
    let loaded = match pipeline::load(&input, &pipeline::asset_policy(false)) {
        Ok(loaded) => loaded,
        Err(diagnostic) => return Outcome::error(json, diagnostic),
    };
    let pipeline::Loaded { assets, report, .. } = loaded;

    Outcome::from_report(
        report,
        json,
        Stdout::Manifest,
        None,
        Some(assets.to_json()),
        None,
    )
}

/// Print the document's table of contents.
///
/// The text form is an indented tree — two spaces per heading level below the
/// first — and `--json` prints the bare `[{level,id,title}]` array, because an
/// array is what a script wants to read. Diagnostics go to stderr in both
/// forms, and the exit code reports them either way: an outline is still
/// printable for a document that does not build, so it is not withheld. The
/// entries come from the parser alone, as in `mdx ast`; asset resolution cannot
/// add or remove a heading.
fn outline(args: OutlineArgs) -> i32 {
    let OutlineArgs { input, json } = args;

    // A failure here has no array form, so it uses the ordinary non-JSON
    // reporting: `E_IO` on stderr and the usage exit code.
    if !input.exists() {
        let message = format!("input file {} does not exist", input.display());
        return Outcome::usage(false, message).finish();
    }
    let source = match pipeline::read_source(&input) {
        Ok(source) => source,
        Err(diagnostic) => return Outcome::error(false, diagnostic).finish(),
    };
    let (doc, report) = pipeline::parse_document(&source, &input);

    for diagnostic in &report.diagnostics {
        print_diagnostic(diagnostic);
    }
    if json {
        print_json(&doc.toc);
    } else {
        for entry in &doc.toc {
            println!("{}", outline_line(entry));
        }
    }
    mdx_core::exit_code(&report)
}

/// One line of the indented outline: `title (id)`, two spaces per level.
fn outline_line(entry: &TocEntry) -> String {
    let mut line = String::with_capacity(entry.title.len() + entry.id.len() + 6);
    for _ in 1..entry.level {
        line.push_str("  ");
    }
    line.push_str(&entry.title);
    line.push_str(" (");
    line.push_str(&entry.id);
    line.push(')');
    line
}

/// Watch one document and rebuild it whenever the file changes.
///
/// The loop polls the input's modification time every [`watch::TICK`] and
/// builds once the change has been quiet for `--debounce-ms` (400 ms by
/// default), so a save that arrives as several writes still produces one build.
/// The first build starts as soon as the input can be read. stdout carries
/// exactly one line per build — `built <path> in <ms> ms` or
/// `error <CODE>: <message>` — while warnings keep their position information
/// on stderr. An input that cannot be read is reported once and the loop keeps
/// polling for it to come back; only Ctrl-C ends the process (default signal
/// handling), and `--once` returns the exit code of its single build.
fn watch_command(args: WatchArgs) -> i32 {
    let WatchArgs {
        input,
        build,
        debounce_ms,
        once,
    } = args;

    let output = match build.out.as_deref() {
        Some(path) => path.to_path_buf(),
        None => default_output(&input, build.format),
    };
    // Rewriting the watched file would change its modification time and rebuild
    // forever, so this pair is a configuration mistake, not a loop to run.
    if watch::same_file(&input, &output) {
        let message = format!(
            "output {} is the watched input; use -o/--out to name another file",
            output.display()
        );
        return Outcome::usage(false, message).finish();
    }

    if once {
        let started = Instant::now();
        let outcome = build_document(&build, &input, false);
        print_build_line(&outcome, started.elapsed());
        return outcome.exit;
    }

    eprintln!("watching {} (Ctrl-C to stop)", input.display());
    let mut watcher = watch::Watcher::new(Duration::from_millis(debounce_ms));
    loop {
        let now = Instant::now();
        match watcher.step(now, watch::mtime(&input)) {
            watch::Step::Unreachable(message) => {
                println!("error {}: {message}", DiagCode::Io.as_str());
            }
            watch::Step::Build => {
                let started = Instant::now();
                let outcome = build_document(&build, &input, false);
                print_build_line(&outcome, started.elapsed());
            }
            watch::Step::Wait => {}
        }
        std::thread::sleep(watch::TICK);
    }
}

/// Report one watch build: warnings on stderr, one status line on stdout.
///
/// The status line is the build's verdict, so a failure is not repeated on
/// stderr; a warning (`E_MATH_CONVERT`, `W_FRONTMATTER`, …) fails nothing and
/// would otherwise be invisible.
fn print_build_line(outcome: &Outcome, elapsed: Duration) {
    for diagnostic in &outcome.report.diagnostics {
        if diagnostic.severity != Severity::Error {
            print_diagnostic(diagnostic);
        }
    }
    println!("{}", build_line(outcome, elapsed));
}

/// The single status line `mdx watch` prints for one build.
///
/// The path is the one that was written, so `--once` and a rebuild report the
/// same thing `mdx render` prints on stdout.
fn build_line(outcome: &Outcome, elapsed: Duration) -> String {
    if let (mdx_core::exit::OK, Some(path)) = (outcome.exit, outcome.output.as_deref()) {
        return format!("built {path} in {} ms", elapsed.as_millis());
    }
    // `build_document` fails with a diagnostic; if one ever escapes without one,
    // the exit code is still worth reporting.
    let fallback_code = DiagCode::Io.as_str();
    match outcome.report.errors().next() {
        Some(diagnostic) => {
            let code = diagnostic.code.as_str();
            format!("error {code}: {}", diagnostic.message)
        }
        None => format!(
            "error {fallback_code}: build failed (exit {})",
            outcome.exit
        ),
    }
}

/// Default output path: the input path with the format's extension.
fn default_output(input: &Path, format: Format) -> PathBuf {
    input.with_extension(format.extension())
}

/// What a subcommand prints on stdout when `--json` is not set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stdout {
    /// Nothing at all; `check` reports through the exit code and stderr only.
    Silent,
    /// The path that was written.
    Path,
    /// The parsed document model.
    Doc,
    /// The asset manifest.
    Manifest,
}

/// The result of one subcommand: diagnostics, exit code and any artifacts.
struct Outcome {
    /// Every diagnostic the run produced, in pipeline order.
    report: Report,
    /// Process exit code this run reports.
    exit: i32,
    /// What a non-JSON run prints on stdout.
    stdout: Stdout,
    /// Path that was written, if any.
    output: Option<String>,
    /// Asset manifest, if the run resolved assets.
    manifest: Option<serde_json::Value>,
    /// Parsed document, for `mdx ast`.
    ast: Option<Doc>,
    /// Whether `--json` was requested.
    json: bool,
}

impl Outcome {
    /// A usage failure: nothing was read and no artifact exists.
    fn usage(json: bool, message: String) -> Self {
        let mut report = Report::default();
        report.push(Diagnostic::error(DiagCode::Io, message));
        Self {
            report,
            exit: mdx_core::exit::USAGE,
            stdout: Stdout::Silent,
            output: None,
            manifest: None,
            ast: None,
            json,
        }
    }

    /// A run that stopped on a single diagnostic before producing anything.
    fn error(json: bool, diagnostic: Diagnostic) -> Self {
        let mut report = Report::default();
        report.push(diagnostic);
        let exit = mdx_core::exit_code(&report);
        Self {
            report,
            exit,
            stdout: Stdout::Silent,
            output: None,
            manifest: None,
            ast: None,
            json,
        }
    }

    /// A finished run; the exit code follows from `report`.
    fn from_report(
        report: Report,
        json: bool,
        stdout: Stdout,
        output: Option<String>,
        manifest: Option<serde_json::Value>,
        ast: Option<Doc>,
    ) -> Self {
        let exit = mdx_core::exit_code(&report);
        Self {
            report,
            exit,
            stdout,
            output,
            manifest,
            ast,
            json,
        }
    }

    /// Print the result and return the process exit code.
    fn finish(self) -> i32 {
        let Outcome {
            report,
            exit,
            stdout,
            output,
            manifest,
            ast,
            json,
        } = self;
        if json {
            let envelope = Envelope {
                ok: exit == mdx_core::exit::OK,
                exit,
                diagnostics: &report.diagnostics,
                output: output.as_deref(),
                manifest: manifest.as_ref(),
                ast: ast.as_ref(),
            };
            match serde_json::to_string(&envelope) {
                Ok(text) => println!("{text}"),
                // Unreachable for these types; still, never lose the exit code.
                Err(_) => {
                    let ok = exit == mdx_core::exit::OK;
                    println!(
                        "{{\"ok\":{ok},\"exit\":{exit},\"diagnostics\":[],\"output\":null,\"manifest\":null}}"
                    );
                }
            }
        } else {
            for diagnostic in &report.diagnostics {
                print_diagnostic(diagnostic);
            }
            match stdout {
                Stdout::Silent => {}
                Stdout::Path => {
                    if let Some(path) = output.as_deref() {
                        println!("{path}");
                    }
                }
                Stdout::Doc => {
                    if let Some(doc) = ast.as_ref() {
                        print_json(doc);
                    }
                }
                Stdout::Manifest => {
                    if let Some(value) = manifest.as_ref() {
                        print_json(value);
                    }
                }
            }
        }
        exit
    }
}

/// The single object `--json` writes to stdout.
#[derive(Debug, Serialize)]
struct Envelope<'a> {
    /// True when `exit` is `mdx_core::exit::OK`.
    ok: bool,
    /// Process exit code of this run.
    exit: i32,
    /// Every diagnostic, in pipeline order.
    diagnostics: &'a [Diagnostic],
    /// Path `render` wrote; `null` when nothing was written.
    output: Option<&'a str>,
    /// Asset manifest (`mdx-assets/1`); `null` for `mdx ast`.
    manifest: Option<&'a serde_json::Value>,
    /// Parsed document; only `mdx ast` sets it.
    #[serde(skip_serializing_if = "Option::is_none")]
    ast: Option<&'a Doc>,
}

/// Write one diagnostic to stderr as `CODE: message`, with position when known.
fn print_diagnostic(diagnostic: &Diagnostic) {
    match (diagnostic.line, diagnostic.column) {
        (Some(line), Some(column)) => eprintln!(
            "{}:{}:{}: {}",
            diagnostic.code.as_str(),
            line,
            column,
            diagnostic.message
        ),
        (Some(line), None) => {
            eprintln!(
                "{}:{}: {}",
                diagnostic.code.as_str(),
                line,
                diagnostic.message
            )
        }
        _ => eprintln!("{}: {}", diagnostic.code.as_str(), diagnostic.message),
    }
}

/// Write `value` to stdout as pretty JSON.
fn print_json<T: Serialize>(value: &T) {
    if let Ok(text) = serde_json::to_string_pretty(value) {
        println!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The outline text of `source`, one line per heading.
    fn outline_text(source: &str) -> String {
        let (doc, _) = pipeline::parse_document(source, Path::new("test.md"));
        doc.toc
            .iter()
            .map(outline_line)
            .collect::<Vec<String>>()
            .join("\n")
    }

    #[test]
    fn outline_lines_indent_by_heading_level() {
        let text = outline_text("# One\n\n## Two\n\n### Three\n");
        assert_eq!(text, "One (one)\n  Two (two)\n    Three (three)");
    }

    #[test]
    fn outline_lines_keep_the_heading_id() {
        // The id is the anchor a reader copies, so the dedupe suffix shows up
        // in the outline instead of a second, identical-looking title.
        let text = outline_text("## Notes\n\n## Notes\n");
        assert_eq!(text, "  Notes (notes)\n  Notes (notes-1)");
    }

    #[test]
    fn a_built_line_names_the_written_path_and_its_duration() {
        let ok = Outcome::from_report(
            Report::default(),
            false,
            Stdout::Path,
            Some("/tmp/doc.pdf".to_string()),
            None,
            None,
        );
        let line = build_line(&ok, Duration::from_millis(12));
        assert_eq!(line, "built /tmp/doc.pdf in 12 ms");
    }

    #[test]
    fn a_failed_line_is_the_code_and_message_of_the_first_error() {
        let mut report = Report::default();
        report.push(Diagnostic::warning(DiagCode::FrontMatter, "no title"));
        report.push(Diagnostic::error(
            DiagCode::AssetMissing,
            "image not found: pic.png",
        ));
        let failed = Outcome::from_report(report, false, Stdout::Silent, None, None, None);

        let line = build_line(&failed, Duration::from_millis(3));
        assert_eq!(line, "error E_ASSET_MISSING: image not found: pic.png");
    }
}

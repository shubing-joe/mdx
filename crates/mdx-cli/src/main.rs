//! `mdx` — command line front end of the `mdx` Markdown toolchain.
//!
//! Four subcommands sit on one pipeline (see [`pipeline`]):
//!
//! * `render` writes a PDF or a self-contained HTML document,
//! * `check` parses a document and resolves its assets without writing output,
//! * `ast` prints the parsed document model as JSON,
//! * `assets` resolves assets and prints the asset manifest.
//!
//! Exit codes come from `mdx-core`: `0` success, `1` parse/render failure,
//! `2` usage or config error, `3` unresolved assets. `--json` replaces stdout
//! with a single object `{"ok","exit","diagnostics","output","manifest"}`
//! (`"ast"` is added by `mdx ast`); without it, diagnostics go to stderr as
//! `CODE: message` and the command's result — written path, document model or
//! manifest — goes to stdout.

mod pipeline;

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use mdx_core::{DiagCode, Diagnostic, Doc, Report};
use serde::Serialize;

fn main() {
    let cli = Cli::parse();
    let outcome = match cli.command {
        Command::Render(args) => render(args),
        Command::Check(args) => check(args),
        Command::Ast(args) => ast(args),
        Command::Assets(args) => assets(args),
    };
    std::process::exit(outcome.finish());
}

/// `mdx` command line interface.
#[derive(Debug, Parser)]
#[command(
    name = "mdx",
    version,
    about = "Render Markdown documents to PDF or HTML",
    long_about = "Converts a Markdown document to a PDF (Typst) or self-contained HTML, \
                  resolving images, LaTeX math and Mermaid diagrams on the way."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The four operations `mdx` offers.
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
}

/// Arguments of `mdx render`.
#[derive(Debug, Args)]
struct RenderArgs {
    /// Markdown document to render.
    #[arg(value_name = "INPUT")]
    input: PathBuf,

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

    /// Print one machine-readable JSON object instead of the default output.
    #[arg(long)]
    json: bool,
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
    let RenderArgs {
        input,
        out,
        format,
        toc,
        paper,
        pdf_standard,
        fonts,
        ignore_system_fonts,
        offline,
        deterministic,
        jobs,
        dump_typst,
        json,
    } = args;

    if !input.exists() {
        return Outcome::usage(
            json,
            format!("input file {} does not exist", input.display()),
        );
    }
    let loaded = match pipeline::load(&input, &pipeline::asset_policy(offline)) {
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

    pipeline::note_missing_fonts(&fonts, &mut report);
    let math = pipeline::math_map(&doc, jobs, &mut report);
    let diagram_options = mdx_diagram::DiagramOptions {
        deterministic_text: deterministic,
        font_files: fonts.clone(),
    };
    let diagrams = pipeline::diagram_map(&doc, jobs, &diagram_options, &mut report);
    let path = match out {
        Some(path) => path,
        None => default_output(&input, format),
    };

    let written = match format {
        Format::Pdf => {
            let options = mdx_typst::RenderOptions {
                page: mdx_typst::PageSetup {
                    paper: paper.to_typst(),
                    margin_mm: pipeline::DEFAULT_MARGIN_MM,
                    numbered: true,
                },
                toc,
                pdf_standard: pdf_standard.map(PdfStandardArg::to_typst),
                fonts,
                ignore_system_fonts,
                deterministic_today: deterministic
                    .then(|| pipeline::DETERMINISTIC_TODAY.to_string()),
                title: doc.front.title.clone(),
            };
            if let Some(path) = dump_typst.as_deref() {
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
                title: doc.front.title.clone(),
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

//! `mdx-mcp` — Model Context Protocol server for the `mdx` toolchain.
//!
//! The server speaks MCP over stdio ([`rmcp`]) and exposes five tools backed by
//! the same pipeline the CLI runs (see [`tools`]):
//!
//! | tool        | answer                                            |
//! |-------------|---------------------------------------------------|
//! | `md_parse`  | the `mdx-ast/1` document as JSON                  |
//! | `md_check`  | `{ok, exit, diagnostics}`                         |
//! | `md_assets` | the `mdx-assets/1` manifest                       |
//! | `md_render` | `{output, bytes}` after writing PDF or HTML       |
//! | `md_math`   | `{typst}` or `{error, code}` for one formula      |
//!
//! Every handler answers with structured JSON and never panics: an unusable
//! path, an unreadable file or a fatal pipeline diagnostic becomes an
//! `isError` tool result whose payload carries the stable diagnostic code.

mod tools;

use std::path::Path;
use std::process::ExitCode;

use mdx_core::DiagCode;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::tools::{RenderFormat, RenderRequest, ToolError};

/// Arguments of `md_parse`.
#[derive(Debug, Deserialize, JsonSchema)]
struct ParseArgs {
    /// Markdown file to parse; must exist and be readable.
    path: String,
}

/// Arguments of `md_check`.
#[derive(Debug, Deserialize, JsonSchema)]
struct CheckArgs {
    /// Markdown file to check; must exist and be readable.
    path: String,
}

/// Arguments of `md_assets`.
#[derive(Debug, Deserialize, JsonSchema)]
struct AssetsArgs {
    /// Markdown file whose assets should be resolved.
    path: String,
}

/// Arguments of `md_render`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RenderArgs {
    /// Markdown file to render; must exist and be readable.
    path: String,
    /// Output format; defaults to `pdf`, or to `html` when `out` ends in
    /// `.html`.
    #[serde(default)]
    format: Option<RenderFormat>,
    /// Output file; defaults to `path` with the format's extension.
    #[serde(default)]
    out: Option<String>,
    /// Insert a table of contents (PDF output only).
    #[serde(default)]
    toc: Option<bool>,
}

/// Arguments of `md_math`.
#[derive(Debug, Deserialize, JsonSchema)]
struct MathArgs {
    /// LaTeX formula, with or without `$`/`$$` fences.
    latex: String,
}

/// The `mdx` MCP server: a thin layer over the shared pipeline.
#[derive(Debug, Clone, Copy, Default)]
struct MdxServer;

#[tool_router]
impl MdxServer {
    /// Parse a Markdown file into the document model.
    #[tool(
        description = "Parse a Markdown file into the mdx document model (mdx-ast/1) as JSON: \
                       front matter, blocks and table of contents. Image blocks keep their raw \
                       src and a null asset id until assets are resolved."
    )]
    async fn md_parse(&self, Parameters(args): Parameters<ParseArgs>) -> CallToolResult {
        match tools::parse_file(&args.path) {
            // `Doc` is serialised as-is: the tool answers with the model itself.
            Ok(loaded) => structured(&loaded.doc),
            Err(error) => failure(&error),
        }
    }

    /// Parse a file, resolve its assets and report the diagnostics.
    #[tool(
        description = "Check a Markdown file: parse it, resolve its assets and convert its \
                       formulas. Returns {ok, exit, diagnostics} using the same diagnostics and \
                       exit code as `mdx check` (0 ok, 1 failure, 3 asset problems)."
    )]
    async fn md_check(&self, Parameters(args): Parameters<CheckArgs>) -> CallToolResult {
        match tools::check_file(&args.path) {
            Ok(outcome) => structured(&outcome),
            Err(error) => failure(&error),
        }
    }

    /// Resolve the referenced assets and report them as a manifest.
    #[tool(
        description = "Resolve every asset referenced by a Markdown file and return the \
                       mdx-assets/1 manifest: id, kind, origin, mime, sha256, width, height and \
                       payload size for each asset, sorted by id. Assets that cannot be resolved \
                       are omitted from the manifest; md_check reports why."
    )]
    async fn md_assets(&self, Parameters(args): Parameters<AssetsArgs>) -> CallToolResult {
        match tools::manifest_file(&args.path) {
            Ok(manifest) => CallToolResult::structured(manifest),
            Err(error) => failure(&error),
        }
    }

    /// Render a Markdown file to PDF or HTML.
    #[tool(
        description = "Render a Markdown file to PDF (default) or to self-contained HTML, \
                       writing the result to `out` (default: `path` with the format's \
                       extension). Returns {output, bytes}. Fails when a pipeline stage reports \
                       a fatal diagnostic, so no half-rendered document is written."
    )]
    async fn md_render(&self, Parameters(args): Parameters<RenderArgs>) -> CallToolResult {
        let request = RenderRequest {
            input: &args.path,
            format: render_format(&args),
            out: args.out.as_deref(),
            toc: args.toc.unwrap_or(false),
        };
        match tools::render_file(&request) {
            Ok(outcome) => structured(&outcome),
            Err(error) => failure(&error),
        }
    }

    /// Convert one LaTeX formula to Typst math.
    #[tool(
        description = "Convert one LaTeX formula to a Typst math expression. Returns {typst} on \
                       success, or {error, code} when the formula cannot be converted."
    )]
    async fn md_math(&self, Parameters(args): Parameters<MathArgs>) -> CallToolResult {
        match mdx_math::latex_to_typst(&args.latex) {
            Ok(typst) => CallToolResult::structured(json!({ "typst": typst })),
            Err(message) => CallToolResult::structured_error(json!({
                "error": message,
                "code": DiagCode::MathConvert.as_str(),
            })),
        }
    }
}

#[tool_handler(
    name = "mdx",
    instructions = "Markdown toolchain: md_parse returns the document model, md_check reports \
                    diagnostics, md_assets lists resolved assets, md_render writes a PDF or HTML \
                    file, and md_math converts one LaTeX formula."
)]
impl ServerHandler for MdxServer {}

/// Requested format, inferred from the output extension when omitted.
fn render_format(args: &RenderArgs) -> RenderFormat {
    if let Some(format) = args.format {
        return format;
    }
    match args.out.as_deref().and_then(extension) {
        Some(extension) if extension.eq_ignore_ascii_case("html") => RenderFormat::Html,
        _ => RenderFormat::Pdf,
    }
}

/// Lowercased extension of `path`, when it has one.
fn extension(path: &str) -> Option<&str> {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
}

/// Structured (non-error) tool result.
fn structured<T: Serialize>(value: &T) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(value) => CallToolResult::structured(value),
        Err(err) => failure(&ToolError::new(
            DiagCode::Config,
            format!("cannot serialise the tool result: {err}"),
        )),
    }
}

/// Tool-level error result; `error` already carries the diagnostic code.
fn failure(error: &ToolError) -> CallToolResult {
    CallToolResult::structured_error(error.to_value())
}

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("mdx-mcp: cannot start the tokio runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mdx-mcp: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Serve the MCP protocol over stdio until the client disconnects.
async fn serve() -> Result<(), String> {
    let service = MdxServer
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|err| format!("initialisation failed: {err}"))?;
    // The quit reason is informational only; a join failure means the
    // transport died before the client disconnected.
    match service.waiting().await {
        Ok(_reason) => Ok(()),
        Err(err) => Err(format!("transport failed: {err}")),
    }
}

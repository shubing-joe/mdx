//! `mdx-mcp` — Model Context Protocol server for the `mdx` toolchain.
//!
//! The server speaks MCP over stdio ([`rmcp`]) and exposes eight tools backed
//! by the same pipeline the CLI runs (see [`tools`]):
//!
//! | tool             | answer                                              |
//! |------------------|-----------------------------------------------------|
//! | `md_parse`       | the `mdx-ast/1` document as JSON                    |
//! | `md_check`       | `{ok, exit, diagnostics}`                           |
//! | `md_assets`      | the `mdx-assets/1` manifest                         |
//! | `md_render`      | `{output, bytes}` after writing PDF or HTML         |
//! | `md_math`        | `{typst}` or `{error, code}` for one formula        |
//! | `md_outline`     | `{toc}` — one `{level, id, title}` per heading      |
//! | `md_render_many` | `{results}` for a whole batch of documents          |
//! | `md_version`     | `{version, crates}` of the running toolchain        |
//!
//! Every handler answers with structured JSON and never panics: an unusable
//! path, an unreadable file or a fatal pipeline diagnostic becomes an
//! `isError` tool result whose payload carries the stable diagnostic code.
//! `md_render_many` is the one tool whose per-file failures are answers rather
//! than errors, so a broken document cannot cost the caller the rest of its
//! batch.

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

/// Arguments of `md_outline`.
#[derive(Debug, Deserialize, JsonSchema)]
struct OutlineArgs {
    /// Markdown file whose headings should be listed.
    path: String,
}

/// Arguments of `md_render_many`.
#[derive(Debug, Deserialize, JsonSchema)]
struct RenderManyFilesArgs {
    /// Markdown files to render, processed in the order given.
    paths: Vec<String>,
    /// Output format for every file; defaults to `pdf`.
    #[serde(default)]
    format: Option<RenderFormat>,
    /// Directory the outputs are written to; defaults to each input's own
    /// directory. Created when missing.
    #[serde(default)]
    out_dir: Option<String>,
}

/// Arguments of `md_version`: none.
#[derive(Debug, Default, Deserialize, JsonSchema)]
struct VersionArgs {}

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

    /// List a document's headings.
    #[tool(
        description = "List the headings of a Markdown file as {toc:[{level,id,title}]}, in \
                       document order, using the same heading ids the rendered document anchors \
                       (github-style slugs, duplicates suffixed with -1, -2). Only headings \
                       contribute; a file without headings yields an empty toc."
    )]
    async fn md_outline(&self, Parameters(args): Parameters<OutlineArgs>) -> CallToolResult {
        match tools::outline_file(&args.path) {
            Ok(outcome) => structured(&outcome),
            Err(error) => failure(&error),
        }
    }

    /// Render a batch of Markdown files.
    #[tool(
        description = "Render several Markdown files to PDF (default) or self-contained HTML, \
                       one after another and in the order given, writing them to `out_dir` \
                       (created when missing; default: each input's own directory, named after \
                       the input). Returns {results:[{input,output,bytes,ok,code?}]}, one entry \
                       per requested file: a file that fails is reported with ok:false and its \
                       diagnostic code and does not stop the remaining files."
    )]
    async fn md_render_many(
        &self,
        Parameters(args): Parameters<RenderManyFilesArgs>,
    ) -> CallToolResult {
        let request = tools::RenderManyRequest {
            inputs: &args.paths,
            format: args.format.unwrap_or(RenderFormat::Pdf),
            out_dir: args.out_dir.as_deref(),
        };
        match tools::render_many(&request) {
            Ok(outcome) => structured(&outcome),
            Err(error) => failure(&error),
        }
    }

    /// Report the version of the running toolchain.
    #[tool(
        description = "Report the version of the running mdx toolchain as {version, crates}, \
                       where `crates` maps every mdx crate the server is built from to its \
                       version. Takes no arguments."
    )]
    async fn md_version(&self, Parameters(_args): Parameters<VersionArgs>) -> CallToolResult {
        structured(&tools::version())
    }
}

#[tool_handler(
    name = "mdx",
    instructions = "Markdown toolchain: md_parse returns the document model, md_check reports \
                    diagnostics, md_assets lists resolved assets, md_outline lists headings, \
                    md_render writes a PDF or HTML file, md_render_many renders a batch of \
                    files, md_math converts one LaTeX formula, and md_version reports the \
                    toolchain version."
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Everything `tools/list` must advertise, in the order the router
    /// reports it (sorted by name).
    const TOOLS: [&str; 8] = [
        "md_assets",
        "md_check",
        "md_math",
        "md_outline",
        "md_parse",
        "md_render",
        "md_render_many",
        "md_version",
    ];

    /// A path that cannot exist inside the crate directory.
    const MISSING: &str = "mdx-mcp-missing-fixture.md";

    /// Sorted property names of a tool's input schema.
    fn properties(schema: &Value) -> Vec<String> {
        let listed = schema["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut names: Vec<String> = listed.into_iter().map(|(name, _)| name).collect();
        names.sort_unstable();
        names
    }

    /// Sorted `required` names of a tool's input schema.
    fn required(schema: &Value) -> Vec<String> {
        let listed = schema["required"].as_array().cloned().unwrap_or_default();
        let mut names: Vec<String> = listed
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        names.sort_unstable();
        names
    }

    /// The input schema of a routed tool, as JSON.
    fn schema(name: &str) -> (Value, String) {
        let router = MdxServer::tool_router();
        let tool = router.get(name).expect("the tool must be routed");
        let description = tool.description.as_deref().unwrap_or_default().to_string();
        (
            Value::Object(tool.input_schema.as_ref().clone()),
            description,
        )
    }

    #[test]
    fn the_router_serves_exactly_eight_tools() {
        let tools = MdxServer::tool_router().list_all();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(names, TOOLS);
    }

    #[test]
    fn every_tool_documents_itself_and_takes_an_object() {
        for tool in MdxServer::tool_router().list_all() {
            assert!(
                tool.description
                    .as_deref()
                    .is_some_and(|text| !text.is_empty()),
                "{} has no description",
                tool.name
            );
            let schema = Value::Object(tool.input_schema.as_ref().clone());
            assert_eq!(schema["type"], "object", "{}", tool.name);
        }
    }

    #[test]
    fn the_five_existing_tools_keep_their_inputs() {
        let expected: [(&str, &[&str], &[&str]); 5] = [
            ("md_parse", &["path"], &["path"]),
            ("md_check", &["path"], &["path"]),
            ("md_assets", &["path"], &["path"]),
            ("md_render", &["format", "out", "path", "toc"], &["path"]),
            ("md_math", &["latex"], &["latex"]),
        ];
        for (name, want_properties, want_required) in expected {
            let (schema, _description) = schema(name);
            assert_eq!(properties(&schema), want_properties, "{name}");
            assert_eq!(required(&schema), want_required, "{name}");
        }
    }

    #[test]
    fn the_new_tools_require_the_arguments_they_need() {
        let (outline, _) = schema("md_outline");
        assert_eq!(properties(&outline), ["path"]);
        assert_eq!(required(&outline), ["path"]);

        let (many, _) = schema("md_render_many");
        assert_eq!(properties(&many), ["format", "out_dir", "paths"]);
        assert_eq!(required(&many), ["paths"]);

        let (version, _) = schema("md_version");
        assert!(properties(&version).is_empty());
        assert!(required(&version).is_empty());
    }

    #[tokio::test]
    async fn md_outline_lists_the_headings_of_a_document() {
        let path = std::env::temp_dir().join(format!("mdx-mcp-handler-{}.md", std::process::id()));
        std::fs::write(&path, "# One\n\ntext\n\n## Two\n").expect("write the outline fixture");
        let args = OutlineArgs {
            path: path.to_string_lossy().into_owned(),
        };
        let result = MdxServer.md_outline(Parameters(args)).await;
        std::fs::remove_file(&path).ok();

        assert_eq!(result.is_error, Some(false));
        let payload = result.structured_content.expect("a structured payload");
        assert_eq!(payload["toc"][0]["level"], 1);
        assert_eq!(payload["toc"][0]["id"], "one");
        assert_eq!(payload["toc"][0]["title"], "One");
        assert_eq!(payload["toc"][1]["id"], "two");
        assert_eq!(payload["toc"].as_array().map(Vec::len), Some(2));
    }

    #[tokio::test]
    async fn md_outline_reports_a_missing_file_as_a_tool_error() {
        let args = OutlineArgs {
            path: MISSING.to_string(),
        };
        let result = MdxServer.md_outline(Parameters(args)).await;
        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("an error payload");
        assert_eq!(payload["code"], "E_IO");
    }

    #[tokio::test]
    async fn md_render_many_answers_with_one_entry_per_file() {
        let args = RenderManyFilesArgs {
            paths: vec![MISSING.to_string(), MISSING.to_string()],
            format: Some(RenderFormat::Html),
            out_dir: None,
        };
        let result = MdxServer.md_render_many(Parameters(args)).await;

        // Per-file failures are answers, not tool errors.
        assert_eq!(result.is_error, Some(false));
        let payload = result.structured_content.expect("a structured payload");
        let results = payload["results"]
            .as_array()
            .expect("the results must be an array");
        assert_eq!(results.len(), 2);
        for entry in results {
            assert_eq!(entry["input"], MISSING);
            assert_eq!(entry["ok"].as_bool(), Some(false));
            assert_eq!(entry["code"], "E_IO");
            assert!(entry["output"].is_null());
            assert!(entry["bytes"].is_null());
        }
    }

    #[tokio::test]
    async fn md_render_many_rejects_an_empty_batch() {
        let args = RenderManyFilesArgs {
            paths: Vec::new(),
            format: None,
            out_dir: None,
        };
        let result = MdxServer.md_render_many(Parameters(args)).await;
        assert_eq!(result.is_error, Some(true));
        let payload = result.structured_content.expect("an error payload");
        assert_eq!(payload["code"], "E_CONFIG");
    }

    #[tokio::test]
    async fn md_version_answers_with_the_crate_versions() {
        let result = MdxServer.md_version(Parameters(VersionArgs {})).await;
        assert_eq!(result.is_error, Some(false));
        let payload = result.structured_content.expect("a structured payload");
        assert_eq!(payload["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(payload["crates"]["mdx-core"], env!("CARGO_PKG_VERSION"));
        assert_eq!(payload["crates"]["mdx-mcp"], env!("CARGO_PKG_VERSION"));
    }
}

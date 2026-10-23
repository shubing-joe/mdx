# mdx — crate contract (v0.2)

The shape of the workspace, the signatures that cross a crate boundary, and the
front-end surfaces users can rely on.

**Rule: everything in this document is true of the code at HEAD.** Internals may
change freely as long as these boundaries hold. Arguments in parentheses below
are deliberate deviations that would need a milestone of their own.

Work that is planned but **not** in HEAD — watch mode, `mdx outline`, the extra
MCP tools — is listed in [`ROADMAP.md`](./ROADMAP.md), never here. If a flag or
a function is not described below, it does not exist yet.

## Crates and owned paths

| crate | modules | responsibility |
|-------|---------|----------------|
| `mdx-core` | `diag`, `doc`, `exit` | document model (`mdx-ast/1`), diagnostic codes, exit codes |
| `mdx-parse` | `lib`, `convert`, `front`, `slug`, `toc` | comrak → model, YAML front matter, heading slugs, TOC |
| `mdx-assets` | `lib`, `resolve`, `manifest`, `encode`, `policy` | relative/`data:`/gated-remote resolution, SHA-256 ids, MIME, manifest |
| `mdx-math` | `lib`, `typst`, `svg` | LaTeX → Typst math (`tylax`), RaTeX SVG fallback |
| `mdx-diagram` | `lib`, `render`, `cache` | Mermaid → resvg-safe SVG (merman), content-addressed keys |
| `mdx-typst` | `lib`, `world`, `writer`, `pdf`, `svg`, `page` | Typst world (in-memory VFS, fonts, fixed clock), markup writer, PDF and SVG-page export |
| `mdx-html` | `lib`, `render`, `escape`, `sanitize` | one self-contained HTML file |
| `mdx-cli` | `main`, `pipeline` | the `mdx` binary |
| `mdx-mcp` | `main`, `tools` | MCP server over stdio |
| `mdx-gui` | `main`, `app`, `editor`, `preview`, `diagnostics` | desktop shell |

Dependency direction is one-way: `mdx-core` depends on nothing in the workspace,
the stage crates depend on `mdx-core` (and the two writers on `mdx-assets`), and
the three front ends are leaves. See
[`ARCHITECTURE.md`](./ARCHITECTURE.md#crate-dependency-graph) for the graph.

## Frozen APIs

### mdx-core

```rust
// doc.rs — model, schema `mdx-ast/1`
pub struct AssetId(pub String);                    // "img-<sha256[..12]>"
impl AssetId { pub fn as_str(&self) -> &str; }
pub enum AssetKind { Image, Font, Other }
pub enum Align { None, Left, Center, Right }

pub struct FrontMatter { pub title: Option<String>, pub author: Option<String>,
                         pub date: Option<String>,
                         pub extra: serde_json::Map<String, serde_json::Value> }
pub struct TocEntry { pub level: u8, pub id: String, pub title: String }

pub struct Doc { pub source_path: Option<String>, pub front: FrontMatter,
                 pub blocks: Vec<Block>, pub toc: Vec<TocEntry> }
impl Doc { pub fn new(blocks: Vec<Block>) -> Self; }      // collects `toc`
pub fn collect_toc(blocks: &[Block]) -> Vec<TocEntry>;
pub fn plain_text(inlines: &[Inline]) -> String;          // TOC / alt text / metadata

pub enum Block { Heading { level, id, inlines }, Paragraph { inlines },
                 Code { lang, text, meta }, Math { latex, display },
                 Diagram { source }, Image { asset, src, alt, title, width, height },
                 List { ordered, start, tight, items }, BlockQuote { blocks },
                 Table { aligns, header, rows }, ThematicBreak,
                 Html { raw }, FootnoteDef { label, blocks } }
pub enum Inline { Text { text }, Code { text }, Emph { inlines }, Strong { inlines },
                  Strike { inlines }, Link { url, title, inlines },
                  Image { asset, src, alt, title }, Math { latex, display },
                  FootnoteRef { label }, LineBreak { hard } }
```

Both enums are `#[serde(tag = "type", rename_all = "snake_case")]`, so their JSON
spelling is part of the `mdx-ast/1` schema.

```rust
// diag.rs — diagnostics, codes are a stable contract
pub enum Severity { Error, Warning, Info }
pub enum DiagCode { Parse, AssetMissing, AssetUnsupported, AssetOffline,
                    MathConvert, DiagramParse, DiagramRender, Typst, Io, Config,
                    FrontMatter, UnsupportedHtml, AssetRemoteCached }
impl DiagCode { pub fn as_str(self) -> &'static str; }

pub struct Diagnostic { pub code: DiagCode, pub severity: Severity,
                        pub message: String, pub line: Option<u32>,
                        pub column: Option<u32>, pub asset: Option<String> }
impl Diagnostic {
    pub fn error(code: DiagCode, message: impl Into<String>) -> Self;
    pub fn warning(code: DiagCode, message: impl Into<String>) -> Self;
    pub fn at(self, line: u32, column: u32) -> Self;
    pub fn for_asset(self, asset: impl Into<String>) -> Self;
    pub fn is_fatal(&self) -> bool;   // E_* AND Severity::Error
}
// `Display` renders "CODE:line:column message" / "CODE:line message" / "CODE message".
// Implements std::error::Error.

pub struct Report { pub diagnostics: Vec<Diagnostic> }
impl Report {
    pub fn push(&mut self, d: Diagnostic);
    pub fn extend(&mut self, other: Report);
    pub fn merge_from(&mut self, other: Report);
    pub fn summary(&self) -> (usize, usize, usize);   // (errors, warnings, infos)
    pub fn has_errors(&self) -> bool;
    pub fn has_code(&self, code: DiagCode) -> bool;
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic>;
}
```

```rust
// exit.rs — process exit codes (stable contract)
pub const OK: i32 = 0;      // success
pub const FAILED: i32 = 1;  // parse or render failure
pub const USAGE: i32 = 2;   // usage or configuration error
pub const ASSETS: i32 = 3;  // unresolved assets (missing/offline/unsupported)
pub fn exit_code(report: &Report) -> i32;
```

`exit_code` returns `ASSETS` when any diagnostic carries `AssetMissing`,
`AssetUnsupported` or `AssetOffline`; otherwise `FAILED` when the report has any
**error**-severity diagnostic, and `OK` otherwise. Warnings never change it.

### mdx-parse

```rust
pub struct ParseOptions { pub gfm: bool, pub math: bool, pub frontmatter: bool }
impl Default for ParseOptions { .. }                  // all three on

pub fn parse(source: &str, opts: &ParseOptions) -> (mdx_core::Doc, mdx_core::Report);
```

Rules:

- `mermaid` fenced blocks become `Block::Diagram`, not `Block::Code`; the body is
  kept literally.
- `$$…$$` / `$…$` / `math`-tagged fences become `Block::Math` / `Inline::Math`,
  keeping **raw LaTeX** — no conversion happens in the parser.
- Heading ids are GitHub-style slugs, deduplicated in document order with `-1`,
  `-2`, … .
- `frontmatter` is YAML between leading `---` lines; a failure is
  `W_FRONTMATTER` and the body is still parsed.
- Images keep `src` verbatim with `asset: None`; `mdx-assets` fills the id later.
- Each switch is independent: turning one off leaves that syntax as ordinary text
  instead of reporting an error.

### mdx-assets

```rust
pub const DEFAULT_MAX_BYTES: u64 = 32 * 1024 * 1024;

pub struct AssetPolicy { pub offline: bool, pub allow_remote: bool,
                         pub cache_dir: Option<PathBuf>, pub max_bytes: u64 }
impl Default for AssetPolicy { .. }   // offline:false, allow_remote:false,
                                      // cache_dir:None, max_bytes:DEFAULT_MAX_BYTES

pub struct Asset {
    pub id: AssetId,        // "img-<sha256[..12]>"
    pub kind: mdx_core::AssetKind,
    pub origin: String,     // original src as written
    pub mime: String,       // lowercase, no parameters
    pub bytes: Vec<u8>,
    pub sha256: String,     // hex, full
    pub width: Option<u32>, pub height: Option<u32>,
}

pub struct AssetStore { /* sorted by id, deduplicated */ }
impl AssetStore {
    pub fn new() -> Self;
    pub fn get(&self, id: &AssetId) -> Option<&Asset>;      // binary search
    pub fn iter(&self) -> impl Iterator<Item = &Asset>;     // sorted by id
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn insert(&mut self, asset: Asset) -> AssetId;      // first wins
    pub fn to_json(&self) -> serde_json::Value;             // manifest
}
// FromIterator<Asset> + Extend<Asset> also collapse duplicates.

pub fn resolve_all(doc: &mut mdx_core::Doc, doc_dir: &Path, policy: &AssetPolicy)
    -> (AssetStore, mdx_core::Report);
pub fn resolve_one(src: &str, doc_dir: &Path, policy: &AssetPolicy)
    -> Result<Asset, mdx_core::Diagnostic>;
```

Rules:

- `data:` URIs are decoded in place; `http(s)://` is allowed only when
  `allow_remote && !offline`, otherwise `E_ASSET_OFFLINE`.
- Relative paths resolve against `doc_dir`; a miss is `E_ASSET_MISSING` with the
  raw `src` in `.asset`.
- A remote payload cached in `cache_dir` (keyed by `sha256_hex(url)`) is used
  without touching the network and reported as `W_ASSET_REMOTE_CACHED`.
- Same content always yields the same id, so duplicate files dedupe.
- PNG/JPEG/GIF/WebP/BMP/TIFF decode for `width`/`height`; decode failure is
  `E_ASSET_UNSUPPORTED`.
- The manifest is `{"schema": "mdx-assets/1", "assets": [ … ]}`
  (`manifest::SCHEMA`); it describes each payload but never embeds bytes.

### mdx-math

```rust
pub fn latex_to_typst(src: &str) -> Result<String, String>;   // Typst math body
pub fn latex_to_svg(src: &str) -> Result<String, String>;     // RaTeX fallback
```

`latex_to_typst` returns exactly the text that belongs **between** the `$…$`
delimiters. It strips `$$…$$` / `$…$` / `\[…\]` fences and a trivial
`\begin{equation}` wrapper, rejects structurally broken LaTeX before conversion
(unbalanced `{}`, mismatched `\begin`/`\end`, unbalanced `\left`/`\right`), calls
the third-party converter behind a panic guard, and extracts the math body from
anything document-shaped the converter produced. The result therefore never
contains a `$` fence, a `#set`/`#let`/`#show`/`#import` directive, a
`#math.equation(…)` wrapper, or a raw LaTeX command. The converter's `zws`
substitution for an empty mandatory argument is never passed on.

Errors are one line: a short reason plus at most 80 characters of the offending
snippet. Nothing in this crate panics on input.

### mdx-diagram

```rust
pub struct DiagramOptions { pub deterministic_text: bool,
                            pub font_files: Vec<PathBuf> }
pub fn render_svg(src: &str, opts: &DiagramOptions) -> Result<String, String>;

pub fn cache::key(src: &str) -> String;      // "mmd-" + 16 hex digits, 20 chars
pub struct cache::Memo { .. }                // in-memory key → SVG, sorted iteration
```

Rules:

- The pipeline is **resvg-safe**: output never contains `<foreignObject>` HTML
  labels and always starts with `<svg`.
- Everything that could leak in from the host is pinned: no operation deadline
  (no clock in the output), an id derived from the diagram source (two diagrams
  cannot share generated marker/edge ids), and text metrics that never come from
  installed fonts.
- Text metrics come from merman's deterministic width model by default; a
  non-empty `font_files` switches to those faces' `hmtx` advances scaled by
  `unitsPerEm`. Merman's `native-fonts`/`system-fonts` features are deliberately
  **off**, so no rasterizer enters the dependency closure.
- Errors are a `String` prefixed with a `DiagCode` string: `E_DIAGRAM_PARSE: …`
  (empty, malformed or unsupported source) and `E_DIAGRAM_RENDER: …` (unusable
  font file, or output that violates the SVG contract).

### mdx-typst

```rust
// Re-exports that keep callers off `typst-layout` / `typst-pdf` directly.
// API: Typst 0.15 keeps the document type in `typst-layout`; the `typst` facade
// re-exports `typst-library` only, so `typst::layout::PagedDocument` does not
// resolve.
pub use typst_layout::PagedDocument;
pub use typst_pdf::PdfStandard;                // A_2b, Ua_1, …

pub enum Paper { A4, Letter }
pub struct PageSetup { pub paper: Paper, pub margin_mm: f32, pub numbered: bool }
impl Default for PageSetup { .. }              // A4, 20 mm, numbered

pub struct RenderOptions {
    pub page: PageSetup,
    pub toc: bool,
    pub pdf_standard: Option<PdfStandard>,
    pub fonts: Vec<PathBuf>,
    pub ignore_system_fonts: bool,
    pub deterministic_today: Option<String>,   // "YYYY-MM-DD"
    pub title: Option<String>,
}

pub type MathMap = std::collections::BTreeMap<String, String>;     // latex → typst
pub type DiagramMap = std::collections::BTreeMap<String, String>;  // mermaid → svg

pub fn compile(doc: &Doc, assets: &AssetStore, math: &MathMap, diagrams: &DiagramMap,
               opts: &RenderOptions) -> Result<PagedDocument, Diagnostic>;

pub fn render_pdf(doc: &Doc, assets: &AssetStore, math: &MathMap, diagrams: &DiagramMap,
                  opts: &RenderOptions) -> Result<Vec<u8>, Diagnostic>;

pub fn render_svg_pages(doc: &Doc, assets: &AssetStore, math: &MathMap,
                        diagrams: &DiagramMap, opts: &RenderOptions)
    -> Result<Vec<String>, Diagnostic>;
```

`render_pdf` and `render_svg_pages` are the two entry points the front ends use;
both compile through [`compile`] first.

```rust
// pdf.rs
pub fn export(doc: &PagedDocument, opts: &RenderOptions) -> Result<Vec<u8>, Diagnostic>;

// svg.rs
pub fn page_count(doc: &PagedDocument) -> usize;
pub fn render_pages(doc: &PagedDocument) -> Result<Vec<String>, Diagnostic>;

// page.rs — pure geometry helpers
pub fn page_size(paper: &Paper) -> (f32, f32);          // points
pub fn margins(mm: f32) -> (f32, f32, f32, f32);        // (top, right, bottom, left)

// writer.rs — Markdown → Typst markup (pure: no I/O, no clock, no randomness)
pub fn build(doc: &Doc, assets: &AssetStore, math: &MathMap, diagrams: &DiagramMap,
             opts: &RenderOptions) -> String;
pub fn asset_path(id: &str) -> String;                  // "/assets/<asset id>"
pub fn diagram_key(src: &str) -> String;                // "diagram-<fnv1a64 hex>"
pub fn diagram_path(src: &str) -> String;               // "/diagrams/<key>.svg"
pub fn diagram_files(diagrams: &DiagramMap) -> Vec<(String, Vec<u8>)>;

// world.rs — the hermetic compiler environment
pub const MAIN_PATH: &str = "main.typ";
pub const DEFAULT_TODAY: (i32, u8, u8) = (2026, 1, 1);
impl World {
    pub fn new(doc: &Doc, assets: &AssetStore, math: &MathMap, diagrams: &DiagramMap,
               opts: &RenderOptions) -> Result<Self, Diagnostic>;
    pub fn line_column(&self, span: typst::syntax::DiagSpan) -> (Option<u32>, Option<u32>);
}
impl typst::World for World { .. }
```

Rules:

- The world serves exactly three kinds of virtual path — `main.typ`,
  `/assets/<asset id>`, `/diagrams/<key>.svg` — and answers
  `FileError::NotFound` for anything else. `main.typ` is generated by
  `writer::build` and nothing is `#include`d.
- **No ambient input.** The only file-system reads are the font files named in
  `RenderOptions::fonts` and the bounded CJK system-font scan (below). Sources,
  assets and diagram SVGs are passed in memory.
- **A fixed clock.** `World::today` always returns
  `RenderOptions::deterministic_today`, or `DEFAULT_TODAY` when the caller pins
  none. A malformed date is `E_CONFIG`. The wall clock is never read, and the
  PDF export omits the timestamp (`PdfOptions::timestamp = None` at HEAD), so two
  exports of one document are byte-identical.
- **Fonts.** The bundled `typst-assets` fonts load first (so `Libertinus Serif`
  is always present and heads the writer's stack). Unless
  `ignore_system_fonts` is set, a scan adds at most six CJK-capable faces found
  under `/usr/share/fonts`, `/usr/local/share/fonts`, `$HOME/.local/share/fonts`
  and `$HOME/.fonts`, matching known CJK family markers. `ignore_system_fonts`
  turns that scan off for hermetic builds. A `fonts` entry that cannot be read,
  or that contains no usable font, is `E_IO`.
- **Math probe.** A converted formula can still be invalid Typst, so `compile`
  probes: on a Typst error it rebuilds the world with the offending expression
  removed (in key order) and retries, letting the writer fall back to the raw
  LaTeX. A document whose math is all dropped still compiles; only an error that
  is not attributable to a formula becomes `E_TYPST`.
- **Failure shape.** `E_TYPST` carries the first Typst error with its hints and —
  when the span points into a served file — a 1-based line/column; further errors
  are summarized as a count so a message stays bounded.
- **Bookmarks.** typst-pdf builds the PDF outline from the document's headings
  unconditionally, so every heading becomes a bookmark node and no extra option
  enables them.
- SVG pages: `svg::render_pages` returns exactly `svg::page_count(doc)` entries in
  document order; each starts with `<svg`, carries `width`/`height` (and
  `viewBox`) and embeds its resources, so it can go straight to an image widget.

### mdx-html

```rust
pub type MathMap = BTreeMap<String, String>;      // identical to mdx-typst's
pub type DiagramMap = BTreeMap<String, String>;

pub struct HtmlOptions { pub embed_assets: bool, pub css: Option<String>,
                         pub title: Option<String> }
impl Default for HtmlOptions { .. }               // embed_assets: true

pub fn render_html(doc: &Doc, assets: &AssetStore, math: &MathMap,
                   diagrams: &DiagramMap, opts: &HtmlOptions)
    -> Result<String, Diagnostic>;
```

The output carries no external references: the stylesheet is inlined, math keeps
its original LaTeX in `<span class="math" data-tex="…">`, diagrams are inlined as
sanitized SVG and, when `embed_assets` is set, images become `data:` URIs.

`MathMap` and `DiagramMap` are aliases with exactly the shape of `mdx-typst`'s,
so a front end can build one pair of maps and pass it to either writer.

## Front ends

### mdx-cli — the `mdx` binary

```
mdx render <INPUT.md> [-o OUT] [--format pdf|html] [--toc] [--paper a4|letter]
           [--pdf-standard a-2b|ua-1] [--font PATH]... [--ignore-system-fonts]
           [--offline] [--deterministic] [--jobs N] [--dump-typst PATH] [--json]
mdx check  <INPUT.md> [--json]
mdx ast    <INPUT.md> [--json]
mdx assets <INPUT.md> [--json]
```

- `-o`/`--out` is the **long spelling `--out`**; the default output path is the
  input path with the format's extension (`doc.md` → `doc.pdf` / `doc.html`).
- `--format` defaults to `pdf`; `--paper` defaults to `a4`; `--jobs` defaults to
  `1` and must be ≥ 1.
- `--deterministic` pins the document date to `2026-01-01`
  (`pipeline::DETERMINISTIC_TODAY`) and turns on deterministic diagram text.
- `--dump-typst PATH` writes the generated Typst markup before rendering; a write
  failure is `E_IO` and is reported alongside the render result.
- `--offline` is the CLI's opt-out for remote assets; the CLI sets
  `AssetPolicy { offline, allow_remote: true, cache_dir: None,
  max_bytes: 64 MiB }`, so remote references are attempted only when `--offline`
  is absent.
- `render` refuses to write anything when the report already carries an error.
- `--font PATH` is checked once: a path that does not name a file yields an
  `E_CONFIG` **warning** (non-fatal — see below) and is still passed through to
  the writers.

stdout/stderr without `--json`: diagnostics go to stderr as `CODE: message`
(with `CODE:line:column` when a position is known), and the command's result goes
to stdout — the written path for `render`, the `mdx-ast/1` model for `ast`, the
`mdx-assets/1` manifest for `assets`, and nothing for `check`.

With `--json`, stdout is exactly one object:

```json
{"ok": true, "exit": 0, "diagnostics": [], "output": "doc.pdf", "manifest": null}
```

`ast` adds an `"ast"` key (omitted otherwise). `output` and `manifest` are `null`
when the run produced neither. A failing run still prints the envelope with
`ok: false` and the exit code it returns.

Exit codes: `0` ok, `1` render/parse failure, `2` usage or configuration error
(a missing input file is usage), `3` asset problems.

### mdx-mcp — stdio server

Five tools over stdio, each a thin layer over the shared pipeline:

| tool | input | output |
|------|-------|--------|
| `md_parse` | `{path}` | the `mdx-ast/1` model (assets unresolved) |
| `md_check` | `{path}` | `{ok, exit, diagnostics}` |
| `md_assets` | `{path}` | the `mdx-assets/1` manifest |
| `md_render` | `{path, format?, out?, toc?}` | `{output, bytes}` |
| `md_math` | `{latex}` | `{typst}` or `{error, code}` |

`md_render`'s format defaults to `pdf`, or to `html` when `out` ends in
`.html`. Tool-level failures carry the diagnostic code in the error payload.

### mdx-gui — desktop shell

An `eframe`/`egui` window with a source editor pane, a preview pane and a
diagnostics pane, plus `Ast` and `Assets` views of the same document. Two rules:

- **The UI thread never renders.** Parsing, asset resolution, formula and diagram
  conversion and the Typst run happen on a worker thread; the result returns over
  an `mpsc` channel and is applied on a later frame, so typing stays responsive.
  Exactly one worker runs at a time. A render is re-run 0.35 s
  (`app::RENDER_DEBOUNCE`) after the buffer stops changing.
- **Every failure is visible.** File-system errors and failed renders land in the
  status bar; parser and asset diagnostics land in the diagnostics pane. Nothing
  is unwrapped and a missing file never panics.

The GUI runs the same `mdx-parse` → `mdx-assets` → `mdx-typst` path with the same
options, so the preview matches what `mdx render` writes, with one deliberate
difference: the GUI's asset policy allows remote images, because a viewer is
expected to fetch what a document refers to.

## Diagnostics

| code | kind | raised by |
|------|------|-----------|
| `E_PARSE` | error | comrak/parser failure |
| `E_ASSET_MISSING` | error | reference resolves to nothing |
| `E_ASSET_UNSUPPORTED` | error | payload cannot be decoded/typed |
| `E_ASSET_OFFLINE` | error | remote reference under `offline` |
| `E_MATH_CONVERT` | error/warning | formula conversion (warning: document keeps source LaTeX) |
| `E_DIAGRAM_PARSE` | error | Mermaid source rejected |
| `E_DIAGRAM_RENDER` | error | diagram understood, rendering failed |
| `E_TYPST` | error | world construction, compile or PDF export failed |
| `E_IO` | error | read/write of a document, asset or font file |
| `E_CONFIG` | error/warning | invalid configuration (bad date, bad path, missing `--font` file) |
| `W_FRONTMATTER` | warning | front matter failed to parse |
| `W_UNSUPPORTED_HTML` | warning | raw HTML dropped in a non-HTML target |
| `W_ASSET_REMOTE_CACHED` | warning | remote payload served from the cache |

Codes are **add-only**: a code is never renamed and never removed. Fatality is
decided by `Diagnostic::is_fatal`, which requires an `E_*` code **and**
`Severity::Error` — so a `W_*` code is never fatal even when its severity was
constructed inconsistently, and an `E_*` code built at warning severity is
non-fatal too (`E_CONFIG` for a missing `--font` file is the existing case).

## Non-negotiables

- No `unsafe`.
- No `unwrap()`/`expect()`/`panic!` on user input, user paths or parse results.
  The one third-party black box that may panic (`tylax`) is called behind a panic
  guard.
- No network calls except the explicitly gated remote-asset path.
- Every public function that can fail returns a `mdx_core::Diagnostic` or a typed
  error carrying one.
- Determinism: fixed clock, fixed fonts, sorted iteration, content-addressed ids.
  Two runs over the same input produce byte-identical output.
- No ambient state at HEAD: the only environment variable any code path reads is
  `HOME`, and only to locate user font directories in the CJK font scan. (M2 adds
  `MDX_CACHE_DIR` for the diagram cache — see [`ROADMAP.md`](./ROADMAP.md).)

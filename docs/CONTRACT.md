# mdx — crate contract (v0.1)

**Rule: every crate boundary below is frozen for the first milestone.** Agents may
change internals freely, but signatures and file ownership are fixed so parallel
work cannot conflict.

## Ownership map (20 slices)

| # | Slice | Owned paths |
|---|-------|-------------|
| 1 | core tests + schema fixture | `crates/mdx-core/tests/**` |
| 2 | diagnostics UX helpers | `crates/mdx-core/src/diag.rs` (additive only) + `crates/mdx-core/tests/diag.rs` |
| 3 | comrak bridge | `crates/mdx-parse/src/{lib.rs,convert.rs}` |
| 4 | frontmatter + toc + heading ids | `crates/mdx-parse/src/{front.rs,toc.rs,slug.rs}` |
| 5 | asset resolver | `crates/mdx-assets/src/{lib.rs,resolve.rs}` |
| 6 | manifest / hash / mime / offline | `crates/mdx-assets/src/{manifest.rs,encode.rs,policy.rs}` |
| 7 | LaTeX → Typst math | `crates/mdx-math/src/{lib.rs,typst.rs}` |
| 8 | RaTeX fallback + math corpus | `crates/mdx-math/src/svg.rs`, `fixtures/math/**` |
| 9 | Mermaid renderer | `crates/mdx-diagram/src/{lib.rs,render.rs}` |
| 10 | diagram cache + corpus | `crates/mdx-diagram/src/cache.rs`, `fixtures/diagrams/**` |
| 11 | Typst World (VFS, fonts, clock) | `crates/mdx-typst/src/{lib.rs,world.rs}` |
| 12 | Markdown → Typst markup | `crates/mdx-typst/src/writer.rs` |
| 13 | PDF options / bookmarks / pages | `crates/mdx-typst/src/{pdf.rs,page.rs}` |
| 14 | SVG page preview | `crates/mdx-typst/src/svg.rs` |
| 15 | self-contained HTML | `crates/mdx-html/src/**` |
| 16 | CLI | `crates/mdx-cli/src/**` |
| 17 | MCP server | `crates/mdx-mcp/src/**` |
| 18 | GUI shell + editor pane | `crates/mdx-gui/src/{main.rs,app.rs,editor.rs}` |
| 19 | GUI preview + diagnostics | `crates/mdx-gui/src/{preview.rs,diagnostics.rs}` |
| 20 | fixtures, integration tests, CI, docs | `fixtures/notes/**`, `tests/**`, `.github/**`, `README.md`, `docs/**`, `scripts/**` |

## Frozen APIs

### mdx-parse
```rust
pub struct ParseOptions { pub gfm: bool, pub math: bool, pub frontmatter: bool }
impl Default for ParseOptions { .. }

pub fn parse(source: &str, opts: &ParseOptions) -> (mdx_core::Doc, mdx_core::Report);
```
Rules:
- `mermaid` fenced blocks become `Block::Diagram`, not `Block::Code`.
- `$$..$$` / `$..$` become `Block::Math` / `Inline::Math` (keep raw LaTeX, no conversion here).
- Heading ids are github-style slugs, deduplicated with `-1`, `-2`.
- `frontmatter` is YAML between leading `---` lines; failure → `DiagCode::FrontMatter` warning, body still parsed.
- Images keep `src` verbatim and `asset: None`; `mdx-assets` fills it later.

### mdx-assets
```rust
pub struct AssetPolicy { pub offline: bool, pub allow_remote: bool, pub cache_dir: Option<PathBuf>, pub max_bytes: u64 }
pub struct Asset {
    pub id: mdx_core::AssetId,   // "img-<sha256[..12]>"
    pub kind: mdx_core::AssetKind,
    pub origin: String,          // original src as written
    pub mime: String,
    pub bytes: Vec<u8>,
    pub sha256: String,          // hex, full
    pub width: Option<u32>, pub height: Option<u32>,
}
pub struct AssetStore { /* id -> Asset */ }
impl AssetStore {
    pub fn get(&self, id: &mdx_core::AssetId) -> Option<&Asset>;
    pub fn iter(&self) -> impl Iterator<Item = &Asset>;
    pub fn to_json(&self) -> serde_json::Value;   // manifest schema mdx-assets/1
}
pub fn resolve_all(doc: &mut mdx_core::Doc, doc_dir: &Path, policy: &AssetPolicy) -> (AssetStore, mdx_core::Report);
pub fn resolve_one(src: &str, doc_dir: &Path, policy: &AssetPolicy) -> Result<Asset, mdx_core::Diagnostic>;
```
Rules:
- `data:` URIs decoded in place; `http(s)://` allowed only when `allow_remote && !offline`, otherwise `E_ASSET_OFFLINE`.
- Relative paths resolve against `doc_dir`; missing → `E_ASSET_MISSING` with the raw `src` in `.asset`.
- Same content always yields the same id; duplicate files dedupe.
- PNG/JPEG/GIF/WebP decoded for `width`/`height`; decode failure → `E_ASSET_UNSUPPORTED`.

### mdx-math
```rust
pub fn latex_to_typst(src: &str) -> Result<String, String>;   // Typst math expression body
pub fn latex_to_svg(src: &str) -> Result<String, String>;     // RaTeX fallback, standalone SVG
```
Formula conversion must strip `$`/`$$` fences if present and never emit Typst markup outside math mode.

### mdx-diagram
```rust
pub struct DiagramOptions { pub deterministic_text: bool, pub font_files: Vec<PathBuf> }
pub fn render_svg(src: &str, opts: &DiagramOptions) -> Result<String, String>;
```
Must use the **resvg-safe** pipeline (no `foreignObject`); output must start with `<svg`.

### mdx-typst
```rust
pub struct PageSetup { pub paper: Paper, pub margin_mm: f32, pub numbered: bool }
pub enum Paper { A4, Letter }
pub struct RenderOptions {
    pub page: PageSetup,
    pub toc: bool,
    pub pdf_standard: Option<typst_pdf::PdfStandard>,
    pub fonts: Vec<PathBuf>,          // packaged font files
    pub ignore_system_fonts: bool,
    pub deterministic_today: Option<String>, // "2026-01-01"
    pub title: Option<String>,
}
pub type MathMap = std::collections::BTreeMap<String, String>;      // latex -> typst expr
pub type DiagramMap = std::collections::BTreeMap<String, String>;   // mermaid src -> svg

pub fn render_pdf(doc: &mdx_core::Doc, assets: &mdx_assets::AssetStore,
                  math: &MathMap, diagrams: &DiagramMap,
                  opts: &RenderOptions) -> Result<Vec<u8>, mdx_core::Diagnostic>;

pub fn render_svg_pages(doc: &mdx_core::Doc, assets: &mdx_assets::AssetStore,
                        math: &MathMap, diagrams: &DiagramMap,
                        opts: &RenderOptions) -> Result<Vec<String>, mdx_core::Diagnostic>;
```

### mdx-html
```rust
pub struct HtmlOptions { pub embed_assets: bool, pub css: Option<String>, pub title: Option<String> }
pub fn render_html(doc: &mdx_core::Doc, assets: &mdx_assets::AssetStore,
                   math: &MathMap, diagrams: &DiagramMap,
                   opts: &HtmlOptions) -> Result<String, mdx_core::Diagnostic>;
```
Math in HTML is emitted as `<span class="math">` with the original LaTeX (`data-tex`), diagrams as inline SVG.

### mdx-cli
```
mdx render <INPUT.md> [-o OUT] [--format pdf|html] [--toc] [--paper a4|letter]
           [--pdf-standard a-2b|ua-1] [--font PATH]... [--ignore-system-fonts]
           [--offline] [--deterministic] [--jobs N] [--json]
mdx check <INPUT.md> [--json]
mdx ast <INPUT.md> [--json]
mdx assets <INPUT.md> [--json]
```
Exit codes: 0 ok, 1 render/parse failure, 2 usage, 3 asset problems.
`--json` writes one object to stdout: `{"ok":bool,"exit":int,"diagnostics":[...],"output":optional path,"manifest":optional}`.

### mdx-mcp
Tools: `md_parse`, `md_check`, `md_assets`, `md_render`, `md_math`. Transport: stdio.

## Non-negotiables
- No `unsafe`; no network calls except the explicitly gated remote-asset path.
- No `unwrap()`/`expect()` on user input paths or parse results.
- Every public function that can fail returns a `mdx_core::Diagnostic` or a typed error carrying one.
- Determinism: fixed clock, fixed fonts, sorted iteration.

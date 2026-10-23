# Changelog

All notable, user-visible changes to `mdx`. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project aims at
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Everything under a released version below exists in the code at HEAD. Work that
is planned but not shipped lives in [`docs/ROADMAP.md`](./docs/ROADMAP.md), not
here.

## [Unreleased] — M2

M2 is in flight: watch mode, `mdx outline`, three additional MCP tools,
print-quality PDF structure, GUI usability work, performance baselines and
packaging. None of it is released yet; the planned surface and its acceptance
criteria are in [`docs/ROADMAP.md`](./docs/ROADMAP.md). Entries move into a
version here as they ship.

## [0.1.0] — 2026-10-23

The first end-to-end release: one Markdown file in, a PDF or a self-contained
HTML page out, with no LaTeX distribution, browser or Pandoc in between.

### Added

**Command line** — the `mdx` binary.

- `mdx render <INPUT.md>` writes a PDF (Typst) or a self-contained HTML document.
  Flags: `-o/--out OUT`, `--format pdf|html`, `--toc`, `--paper a4|letter`,
  `--pdf-standard a-2b|ua-1`, `--font PATH` (repeatable),
  `--ignore-system-fonts`, `--offline`, `--deterministic`, `--jobs N`,
  `--dump-typst PATH`, `--json`. The default output path is the input path with
  the format's extension.
- `mdx check <INPUT.md>` parses, resolves assets and validates formulas without
  writing anything; the exit code is the result.
- `mdx ast <INPUT.md>` prints the parsed document model (`mdx-ast/1`).
- `mdx assets <INPUT.md>` prints the resolved asset manifest (`mdx-assets/1`).
- `--json` makes every subcommand print exactly one object —
  `{"ok", "exit", "diagnostics", "output", "manifest"}`, plus `"ast"` for
  `mdx ast` — so a script can read one line instead of parsing prose.
- Exit codes distinguish the failure classes: `0` success, `1` parse/render
  failure, `2` usage or configuration error, `3` unresolved assets (missing,
  offline or unsupported).

**Parsing**

- CommonMark plus the GitHub extensions (tables, strikethrough, task lists,
  autolinks).
- YAML front matter for `title`, `author`, `date` and arbitrary extra keys; a
  malformed block is a `W_FRONTMATTER` warning and the body still parses.
- GitHub-style heading slugs, deduplicated as `-1`, `-2`, …, mirrored in the
  document's table of contents.
- LaTeX math from `$…$`, `$$…$$` and `math`-tagged fences, kept as raw LaTeX in
  the model.
- `mermaid` fenced blocks become diagrams rather than code blocks.
- Footnotes, block quotes, nested lists, images and raw HTML.

**Assets**

- Images resolve from relative paths, absolute paths, `file:` and `data:` URIs,
  and — only when the policy allows it — `http(s)` references.
- Assets are content addressed: the id is `img-<sha256[..12]>`, so identical
  bytes share one entry and duplicate files collapse.
- PNG, JPEG, GIF, WebP, BMP and TIFF payloads are decoded for intrinsic width
  and height; a decode failure is `E_ASSET_UNSUPPORTED`.
- `--offline` refuses every remote reference with `E_ASSET_OFFLINE`; a render
  never opens a socket of its own.
- `mdx-assets/1` manifest with id, kind, origin, media type, digest, byte length
  and pixel size per asset.

**Math**

- LaTeX is converted to Typst math with `tylax`, with the RaTeX SVG renderer as
  a fallback probe.
- A formula Typst cannot express no longer fails the document: it is reported as
  `E_MATH_CONVERT` and the document keeps the original LaTeX. The compile step
  probes the converted expressions too, so an expression Typst rejects is
  dropped rather than aborting the render.

**Diagrams**

- Mermaid renders through merman with the `all-diagrams` and `complete-svg`
  feature set, over a resvg-safe pipeline (no `<foreignObject>`), so the SVG is
  accepted by both the HTML output and the Typst/PDF path.
- Text metrics are deterministic by default; passing `--font` switches label
  measurement to those faces' metrics. Diagram files are content-addressed
  (`diagram-<fnv1a64(source)>`).

**Typst rendering**

- A hermetic world: the generated `main.typ`, resolved assets and rendered
  diagrams are served from memory, and the only file-system reads are the font
  files named with `--font` (plus the bounded CJK scan). Nothing is `#include`d.
- All Typst calls that can fail return a stable diagnostic, with the first Typst
  error's line and column in the generated source.
- PDF export via `typst-pdf` with optional `PDF/A-2b` or `PDF/UA-1` conformance,
  an outline built from the document's headings, and a document id derived from
  the title.
- One standalone SVG per page, in document order — the preview path.

**HTML**

- A single self-contained HTML file: inlined stylesheet, assets embedded as
  `data:` URIs, diagrams inlined as sanitized SVG, and math kept as its original
  LaTeX in `<span class="math" data-tex="…">`.

**Fonts**

- Typst's bundled fonts (`Libertinus Serif` and friends) are always available,
  so no system font is required.
- Typst ships no CJK face, so a document with Chinese, Japanese or Korean text
  picks up a CJK font from the host by default; `--ignore-system-fonts` disables
  the scan for hermetic builds, and `--font PATH` adds or overrides a family.

**Determinism**

- `--deterministic` pins the document date to `2026-01-01` and selects
  deterministic diagram text metrics; combined with content-addressed ids and
  sorted iteration this makes repeated renders byte-identical.

**Diagnostics**

- Stable codes across every stage: `E_PARSE`, `E_ASSET_MISSING`,
  `E_ASSET_UNSUPPORTED`, `E_ASSET_OFFLINE`, `E_MATH_CONVERT`,
  `E_DIAGRAM_PARSE`, `E_DIAGRAM_RENDER`, `E_TYPST`, `E_IO`, `E_CONFIG`,
  `W_FRONTMATTER`, `W_UNSUPPORTED_HTML`, `W_ASSET_REMOTE_CACHED`.
- Codes are add-only and carry a severity, a message and, where known, a 1-based
  line and column; on a terminal they render as `CODE:line:column message`.

**MCP server** — `mdx-mcp`, stdio transport.

- `md_parse`, `md_check`, `md_assets`, `md_render` and `md_math`, each a thin
  layer over the same pipeline, so an agent sees the same document model,
  diagnostics and exit codes as the CLI.

**Desktop shell** — `mdx-gui`.

- Editor pane, SVG page preview and diagnostics pane in one `egui`/`eframe`
  window, with `AST` and `Assets` views of the same document.
- Rendering runs on a worker thread (debounced 0.35 s after the last edit), so
  typing stays responsive; results arrive over a channel.
- `Ctrl+S` saves the buffer, `Ctrl+R` re-reads the file, `Ctrl+E` renders the
  PDF. File-system errors and failed renders land in the status bar; parser and
  asset diagnostics land in the diagnostics pane.
- The preview runs the same parse → assets → Typst path as the CLI, so it
  matches what `mdx render` would write. Its one deliberate difference is that a
  viewer may fetch the remote images a document refers to.

### Notes

- No `unsafe`, and no `unwrap`/`expect`/`panic!` on user input. Failures travel
  as `mdx_core::Diagnostic` values.
- Dual-licensed MIT OR Apache-2.0; `scripts/license-audit.sh` fails the build on
  a GPL, AGPL, SSPL or EPL dependency.

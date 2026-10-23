# mdx — roadmap

Milestones are cumulative: each one is expected to leave
`cargo test --workspace` green, `cargo clippy --workspace --all-targets -D
warnings` clean, and the contract in [`CONTRACT.md`](./CONTRACT.md) true as
written.

Status legend: **done** — shipped and covered by tests; **in flight** — being
implemented now, criteria may still move; **candidate** — not started, listed so
the next slice is unambiguous.

---

## M1 — end-to-end pipeline · **done**

One document renders from a shell, an editor and an agent, deterministically.

User-visible surface: `mdx render`, `mdx check`, `mdx ast`, `mdx assets`; the MCP
tools `md_parse`, `md_check`, `md_assets`, `md_render`, `md_math`; the `mdx-gui`
shell with an editor pane, preview and diagnostics.

### Acceptance criteria (met)

- `mdx render doc.md -o doc.pdf` writes a PDF whose text is selectable and whose
  headings are PDF bookmarks.
- `mdx render doc.md --format html -o doc.html` writes **one** self-contained
  file: assets are embedded, so it opens with no sibling files and no network.
- The pipeline covers CommonMark + GFM, YAML front matter, GitHub-style heading
  slugs, images (`relative`, `file:`, `data:`, gated `http(s)`), LaTeX math,
  Mermaid diagrams (all merman diagram families with the `complete-svg` feature),
  tables, footnotes and raw HTML.
- Every failure is a diagnostic with a stable code and a process exit code of
  `0`/`1`/`2`/`3`; `--json` prints exactly one object per run.
- Rendering the fixtures twice produces byte-identical output (`--deterministic`
  pins the clock, diagrams keep deterministic text metrics, ids are content
  addressed).
- `cargo test --workspace` green; the integration test drives the real binary
  over `fixtures/notes/**`.
- No `unsafe`, no `unwrap`/`expect` on user input, no network without the
  explicit remote-asset path.

---

## M2 — daily-usable editor and print-quality output · **in flight**

The pipeline works; M2 makes it pleasant to live in — rebuild on save, a PDF
that survives a print shop, a GUI that does not fight the user — and measures
what it costs.

**Nothing in this section is in [`CONTRACT.md`](./CONTRACT.md) yet.** The surface
names below are the planned spellings, and each item moves into `CONTRACT.md`
and `CHANGELOG.md` when it ships, with tests in the same change.

### M2.1 `mdx watch` — rebuild on save

Planned surface: `mdx watch <INPUT>` with `-o, --out OUT`,
`--format pdf|html` (default `pdf`), `--debounce-ms N` (default `400`, `0`
allowed), `--once`, plus every `render` build flag (`--toc`, `--paper`,
`--pdf-standard`, `--font`, `--ignore-system-fonts`, `--offline`,
`--deterministic`, `--jobs`, `--dump-typst`). No `--json`.

- A single file is watched (not a directory), with its mtime polled every 250 ms.
- The input is built **immediately** once when it is readable at startup, so a
  fresh watch shows its result right away; a later change rebuilds only after
  `--debounce-ms` of quiet, so a burst of writes collapses into one rebuild.
- `--once` is exactly that initial build followed by an exit.
- Each build prints exactly one stdout line — `built <path> in <ms> ms` or
  `error <CODE>: <message>` — with diagnostics and warnings also on stderr.
- A missing or unreadable input is reported once and polling continues: the
  watcher never exits on its own, and a partial write must not panic it.
- Continuous mode runs until `Ctrl-C` (default `SIGINT`, no handler); only
  `--once` exits, with that build's code `0`/`1`/`2`/`3`.
- A failed build never leaves a stale successful artifact looking current.

### M2.2 `mdx outline` — the heading tree

Planned surface: `mdx outline <INPUT> [--json]`.

- Text mode prints an indented tree, two spaces per heading level below 1, each
  line `<title> (<id>)`.
- `--json` prints a bare array `[{"level":1,"id":"intro","title":"Intro"}]` — not
  the `{ok, exit, diagnostics}` envelope; diagnostics stay on stderr.
- A missing input is `E_IO` on stderr with exit `2`.
- The outline is `doc.toc` exactly as the writers consume it, so it cannot drift
  from what the PDF bookmark tree and the HTML anchors use.

### M2.3 MCP tools

Planned additions to the stdio server, which will advertise 8 tools in
`tools/list`; the existing five keep their names, inputs and outputs.

- `md_outline {path}` → `{toc:[{level,id,title}]}`.
- `md_render_many {paths:[], format?, out_dir?}` →
  `{results:[{input,output,bytes,ok,code?}]}`, processed sequentially so one
  file's failure does not abort the rest.
- `md_version {}` → `{version, crates:{<crate>:<version>}}`.

### M2.4 Print-quality PDF structure

- Page geometry lives in `RenderOptions`/`PageSetup` (`paper`, `margin_mm`,
  `numbered`) and the generated `#set page(…)` fragment comes from one place
  (`mdx_typst::page::page_source_rules`) instead of being assembled at each call
  site.
- **Gap to close:** no CLI flag exposes `PageSetup::margin_mm` or
  `PageSetup::numbered`. `mdx render` hardcodes a 20 mm margin and numbered pages,
  so only `--paper` and `--toc` reach the page set-up today. A print-quality
  milestone has to surface them, and whatever flag lands must then be documented
  in [`CONTRACT.md`](./CONTRACT.md).
- PDF export stamps the deterministic date rather than omitting or inventing one,
  so `--pdf-standard a-2b` validates **and** output stays byte-reproducible.
- Page-level structure is verifiable in tests rather than by eye: a `/Outlines`
  dictionary exists for a document with headings (`pdf::has_outlines`), the page
  count matches the compiled document (`pdf::page_count`), and the PDF/A part is
  readable from the XMP metadata (`pdf::pdfa_part`).
- Writer-level layout polish: headings stay with their content, list spacing and
  the footnote separator rule are deliberate, and images are capped to the text
  width.

### M2.5 GUI usability

- Drag & drop: a dropped `.md`/`.markdown` file opens; any other file is ignored
  and reported in the status bar.
- A `File` menu with a `Recent files` list (last 10 paths), persisted under
  `$XDG_STATE_HOME/mdx/recent.json` (fallback `~/.local/state/mdx/recent.json`);
  a corrupt or missing state file is ignored, never fatal.
- An `Outline` panel listing `doc.toc`; clicking a heading scrolls the source
  pane to that heading's line.
- Clicking a diagnostic moves the caret to its `Diagnostic.line` and highlights
  it, so a code in the list is a place in the document.
- The status bar reports word count, character count, page count and the last
  build time.
- Keyboard: `Ctrl+O` (focus the path field), `Ctrl+S` (save), `F5` (rebuild),
  `Esc` (close menus / the reload dialog / drop focus).
- Preview toolbar: `Fit width`, zoom presets `50 %` `100 %` `150 %` `200 %`,
  a `Go to page` field plus `Go`, and a `Page N / M` counter.
- Diagnostics pane: a filter box, `N errors` / `N warnings` / `N info` toggle
  chips, `Copy all`, and a `N of M shown` count.
- Empty states name the action that fixes them (`No pages yet — press Ctrl+R`).

### M2.6 Performance baselines

- `scripts/bench.sh` times `mdx render` over a 1 MB and an 8 MB document at
  `--jobs 1` and `--jobs 4`.
- A `benchmarks/` crate (excluded from the workspace) provides an
  `mdx-bench <INPUT.md> [--runs N] [--jobs N] [--toc] [--offline]` binary for
  repeatable local runs: `cargo run --release --manifest-path
  benchmarks/Cargo.toml`.
- Numbers are recorded with the machine and toolchain that produced them, so a
  regression is a diff against a known baseline rather than a feeling.
- **No performance numbers exist yet — the figures are TBD.** Any threshold
  claimed in the README or `CONTRACT.md` must be backed by a measured run.

### M2.7 Packaging

- `cargo install --path crates/mdx-cli` and the release build both produce a
  working `mdx` with no TeX, browser or system-font requirement.
- Arch recipe `packaging/PKGBUILD` builds `mdx`, `mdx-mcp` and `mdx-gui` and
  installs `/usr/share/applications/mdx.desktop`,
  `/usr/share/icons/hicolor/scalable/apps/mdx.svg` and
  `/usr/lib/systemd/user/mdx-mcp@.service`.
- `scripts/install-fonts.sh` prints the `--font PATH` argument for the CJK font
  found on the host; `scripts/release.sh` builds `--release --workspace`,
  strips, and writes `dist/mdx-<version>-<triple>.tar.gz`.
- The license audit (`scripts/license-audit.sh`) passes for the whole dependency
  graph, including the M2 additions.

### M2.8 Test coverage and fixtures

- End-to-end tests drive the real binary over new fixtures (adversarial input,
  math in tables, very long lines, Unicode, empty document, math-only document)
  and assert the exit-code contract per failure class.
- The math corpus grows to 85 valid and 10 malformed formulas, with
  `fixtures/math/COVERAGE.md` documenting the supported LaTeX constructs, the
  RaTeX fallback and the known gaps.

### M2.9 Diagram robustness and on-disk cache

- Rendering the same Mermaid source twice reuses the previous SVG instead of
  re-rendering it: an on-disk cache keyed by the source digest, written
  atomically (temp file + rename) with a one-byte format marker, so a truncated
  or stale file is detected rather than trusted.
- The cache lives in `$XDG_CACHE_HOME/mdx/diagrams` (fallback
  `$HOME/.cache/mdx/diagrams`) and honours an `MDX_CACHE_DIR` override. **This is
  the second environment variable any code path reads** — `HOME` is the other,
  for the CJK font scan.
- New surface: `mdx_diagram::supported_types()` (the diagram kinds this build can
  render) and `mdx_diagram::cache::{dir, load, store, load_from, store_in}`.
- A cache failure is advisory and never fails a render; `render_svg` keeps its
  signature.
- `E_DIAGRAM_PARSE` messages name the diagram type and quote the offending first
  line: `E_DIAGRAM_PARSE: <type keyword>: \`<first line>\` (max 60): <cause>`
  (the empty-source message is unchanged).

### M2.10 Documentation

- `CONTRACT.md` describes the real APIs and flags at HEAD, with no aspirational
  signatures.
- `ARCHITECTURE.md` explains the data flow, the invariants (determinism, no
  ambient file system, font policy) and the failure recovery.
- This roadmap and `CHANGELOG.md` are current, and every documented flag or API
  exists in the code — anything not yet implemented is marked here as planned.

---

## M3 — candidates

Not started. Listed with the criteria that would make each one *done*.

### M3.1 PDF/A verification

Claim a conformance level only after validating the artifact.

- An in-tree verifier (or a documented external check wired into the test suite)
  inspects a rendered PDF/A-2b file and fails the build when a required property
  is missing.
- `--pdf-standard a-2b` output passes the verifier for the image, math, diagram
  and font fixtures.
- The verification step is documented as part of the release checklist, not left
  to the reader.

### M3.2 Remote assets over TLS

Extend the gated remote path beyond what M1 allows.

- `https://` assets fetch over TLS with certificate validation; there is no
  "ignore certificate errors" switch.
- Content and cache are content-addressed exactly like local assets, so a cached
  remote payload renders identically to a local one.
- `--offline` remains a hard refusal, and a network failure is a typed
  `E_ASSET_*` diagnostic, never a hang or a panic.
- A documented size ceiling and timeout bound every fetch.

### M3.3 Editor plugin

Bring the pipeline into a real editor rather than a bespoke shell.

- The plugin reuses `mdx-core` and the stage crates directly (no shelling out per
  keystroke), so diagnostics match `mdx check` exactly.
- Save → render → preview works in the host editor with the same determinism
  guarantees.
- The plugin's supported host version and install steps are documented and
  tested.

### M3.4 LSP server

Expose the pipeline as a language server so any editor gets the same experience.

- `textDocument/publishDiagnostics` carries the same stable `DiagCode`s and
  positions as the CLI.
- `textDocument/completion` and hover cover at least front matter keys, heading
  slugs and math/diagram fences.
- Formatting/document symbols are out of scope unless a later milestone adds
  them; the server is read-only with respect to the document.

---

## Explicitly out of scope (so far)

- Incremental/partial Typst compilation caching across process runs.
- Bibliography/citation handling and a `.bib` pipeline.
- A template/theme language beyond the writer's built-in page setup.
- PDF forms, annotations or signatures.

These are not commitments; they are here so that "why isn't X in the roadmap"
has an answer.

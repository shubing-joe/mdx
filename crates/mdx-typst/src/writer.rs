//! Markdown AST → Typst markup writer.
//!
//! [`build`] turns a parsed [`mdx_core::Doc`], its resolved assets, the
//! converted math expressions and the rendered diagrams into one
//! self-contained Typst source file (the world's `main.typ`). It is a pure
//! function: no I/O, no clock, no randomness — identical inputs always produce
//! byte-identical output, so the compile step stays reproducible.
//!
//! Virtual paths emitted here are owned by this module and served by
//! `crate::world`:
//!
//! - resolved images → [`asset_path`] (`/assets/<asset id>`),
//! - diagrams → [`diagram_path`] (`/diagrams/<key>.svg`), keyed by
//!   [`diagram_key`] (FNV-1a 64 of the exact mermaid source).
//!
//! Anything the world cannot serve degrades instead of dangling: an image whose
//! asset did not resolve becomes its alternative text, a diagram that was not
//! rendered becomes a `mermaid` code block, an unknown footnote reference
//! becomes a footnote holding its label, and raw HTML blocks are dropped
//! (Typst has no HTML passthrough).
//!
//! Escaping rules implemented here (Typst markup lexer):
//!
//! - `` \ # $ @ < > [ ] ` * _ `` are always escaped in text runs; `@` and `<`
//!   would otherwise start a reference/label, `$` an equation, `[` a content
//!   block, ``` ` ``` raw text and `*`/`_` strong/emphasis.
//! - A text run that begins an output line with `-`, `+`, `=`, `/` or with
//!   `digits.` gets one extra escape so it stays a paragraph instead of
//!   becoming a list/enum/heading/term item.
//! - Code text is never escaped — it goes into raw blocks/inline raw, where
//!   Typst takes it verbatim.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use mdx_assets::AssetStore;
use mdx_core::{Align, AssetId, AssetKind, Block, Doc, Inline};

use super::{DiagramMap, MathMap, Paper, RenderOptions};

/// Virtual path prefix for resolved assets (`asset_path`).
const ASSETS_DIR: &str = "/assets/";
/// Virtual path prefix for rendered diagrams (`diagram_path`).
const DIAGRAMS_DIR: &str = "/diagrams/";

/// Latin first, CJK-capable after: Typst walks the list per glyph, so Latin
/// text uses the first family that has the glyph while CJK falls through to a
/// Han family. `Libertinus Serif` is bundled with `typst-assets` and therefore
/// always present; the remaining families are the usual system candidates and
/// simply produce "unknown font family" warnings when a system lacks them.
/// API: https://typst.app/docs/reference/text/text/#parameters-font
const FONT_STACK: &[&str] = &[
    "Libertinus Serif",
    "New Computer Modern",
    "Noto Serif CJK SC",
    "Noto Serif CJK TC",
    "Source Han Serif SC",
    "Noto Sans CJK SC",
    "Noto Sans CJK TC",
    "Source Han Sans SC",
    "PingFang SC",
    "Hiragino Sans GB",
    "Heiti SC",
    "Microsoft YaHei",
    "SimSun",
    "WenQuanYi Micro Hei",
    "DejaVu Sans",
];

/// Stand-in text for an unresolved inline `data:` image, whose URI would
/// otherwise be poured into the PDF verbatim.
const IMAGE_PLACEHOLDER: &str = "[image]";

/// Build the complete Typst source for `doc`.
///
/// The result is deterministic and self-contained: page setup, text/paragraph
/// defaults, an optional table of contents, the document body in document
/// order, and the trailing newline. An empty document still yields a valid
/// Typst file containing just the setup directives.
pub fn build(
    doc: &Doc,
    assets: &AssetStore,
    math: &MathMap,
    diagrams: &DiagramMap,
    opts: &RenderOptions,
) -> String {
    let notes = Footnotes::collect(&doc.blocks);

    let mut out = String::with_capacity(4096);
    push_setup(&mut out, opts);
    if opts.toc {
        push_directive(&mut out, "#outline(title: [目录], indent: auto)");
    }
    // Directives are separated by newlines but carry no trailing one; the body
    // always starts on its own line.
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }

    let mut writer = Writer {
        assets,
        math,
        diagrams,
        notes: &notes,
        placed_notes: BTreeSet::new(),
    };
    writer.blocks(&doc.blocks, &mut out);

    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Virtual path of a resolved asset: `/assets/<asset id>`.
///
/// `id` is an [`mdx_core::AssetId`] as a string (`img-a1b2c3d4`).
pub fn asset_path(id: &str) -> String {
    let mut path = String::with_capacity(ASSETS_DIR.len() + id.len());
    path.push_str(ASSETS_DIR);
    path.push_str(id);
    path
}

/// Stable file name for one diagram: `diagram-<16 lowercase hex digits>`.
///
/// FNV-1a 64 over the UTF-8 bytes of the exact mermaid source (the
/// [`DiagramMap`] key). Content-addressed, so reordering or editing unrelated
/// diagrams never renames this one.
pub fn diagram_key(src: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in src.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut key = String::with_capacity(24);
    key.push_str("diagram-");
    let _ = write!(key, "{hash:016x}");
    key
}

/// Virtual path of one rendered diagram: `/diagrams/<key>.svg`.
pub fn diagram_path(src: &str) -> String {
    let key = diagram_key(src);
    let mut path = String::with_capacity(DIAGRAMS_DIR.len() + key.len() + 4);
    path.push_str(DIAGRAMS_DIR);
    path.push_str(&key);
    path.push_str(".svg");
    path
}

/// Every diagram file the world must register, in deterministic order.
///
/// Returns `(virtual path, SVG bytes)` pairs matching the paths [`build`]
/// emits for `Block::Diagram`.
pub fn diagram_files(diagrams: &DiagramMap) -> Vec<(String, Vec<u8>)> {
    diagrams
        .iter()
        .map(|(src, svg)| (diagram_path(src), svg.as_bytes().to_vec()))
        .collect()
}

// ---------------------------------------------------------------------------
// Document setup
// ---------------------------------------------------------------------------

/// Append one top-level directive, separated from the previous one by a
/// newline. Nothing is emitted for an empty string.
fn push_directive(out: &mut String, directive: &str) {
    if directive.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(directive);
}

fn push_setup(out: &mut String, opts: &RenderOptions) {
    if let Some(title) = opts.title.as_deref() {
        let mut document = String::from("#set document(title: ");
        push_string(title, &mut document);
        document.push(')');
        push_directive(out, &document);
    }

    let mut page = String::from("#set page(paper: ");
    push_string(paper_name(opts.page.paper), &mut page);
    page.push_str(", margin: ");
    push_mm(&mut page, opts.page.margin_mm);
    if opts.page.numbered {
        page.push_str(", numbering: \"1\"");
    }
    page.push(')');
    push_directive(out, &page);

    let mut text = String::from("#set text(font: (");
    for (i, family) in FONT_STACK.iter().enumerate() {
        if i > 0 {
            text.push_str(", ");
        }
        push_string(family, &mut text);
    }
    text.push_str("))");
    push_directive(out, &text);

    push_directive(out, "#set par(justify: true)");
}

/// Typst paper name for a [`Paper`] variant.
/// API: https://typst.app/docs/reference/layout/page/#parameters-paper
fn paper_name(paper: Paper) -> &'static str {
    match paper {
        Paper::A4 => "a4",
        Paper::Letter => "us-letter",
    }
}

/// Append a millimetre length. Negative, non-finite and `-0` margins collapse
/// to `0mm` so the emitted length is always well formed.
fn push_mm(out: &mut String, mm: f32) {
    let mm = if mm.is_finite() && mm > 0.0 { mm } else { 0.0 };
    let _ = write!(out, "{mm}mm");
}

// ---------------------------------------------------------------------------
// Escaping helpers
// ---------------------------------------------------------------------------

/// Append a Typst string literal (quotes included).
/// API: https://typst.app/docs/reference/foundations/str/ (escape sequences)
fn push_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Append plain text escaped for Typst markup.
///
/// Any character can be escaped with a backslash in Typst (`\` followed by a
/// non-whitespace character is an escape token), so the guards below are
/// lossless: the escaped character still renders as itself.
fn push_text(text: &str, out: &mut String) {
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        push_text_line(line, out);
    }
}

/// Escape one line of a plain-text run, including its line-start guard.
fn push_text_line(line: &str, out: &mut String) {
    let guarded_char = matches!(line.chars().next(), Some('-' | '+' | '=' | '/'));
    if guarded_char {
        out.push('\\');
        push_escaped(line, out);
        return;
    }

    // `12. ` at the start of a line is an enumeration marker; escaping the dot
    // keeps `1. `-style text a paragraph.
    let mut digits = 0;
    for c in line.chars() {
        if c.is_ascii_digit() {
            digits += c.len_utf8();
        } else {
            break;
        }
    }
    if digits > 0 && line[digits..].starts_with('.') {
        let rest = &line[digits + 1..];
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            push_escaped(&line[..digits], out);
            out.push_str("\\.");
            push_escaped(rest, out);
            return;
        }
    }

    push_escaped(line, out);
}

/// Escape the characters that carry markup meaning inside a text run.
fn push_escaped(s: &str, out: &mut String) {
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '#' | '$' | '@' | '<' | '>' | '[' | ']' | '`' | '*' | '_'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Strip `$` fences and surrounding whitespace from a raw LaTeX source.
fn trim_math(src: &str) -> &str {
    src.trim_matches(|c: char| c == '$' || c.is_whitespace())
}

/// Longest prefix of `lang` that is a valid Typst raw language tag.
///
/// The tag must start an identifier and continue with identifier characters;
/// anything else would leak into the code block as content, so it is cut off.
/// API: https://typst.app/docs/reference/text/raw/ (`lang` parameter)
fn language_tag(lang: &str) -> &str {
    let mut end = 0;
    for (i, c) in lang.char_indices() {
        let ok = if i == 0 {
            c.is_alphabetic() || c == '_'
        } else {
            c.is_alphanumeric() || c == '_' || c == '-'
        };
        if !ok {
            break;
        }
        end = i + c.len_utf8();
    }
    &lang[..end]
}

/// Append a fenced raw block. The fence is longer than any backtick run in the
/// content, so the block can never be closed early.
fn push_code_block(out: &mut String, lang: Option<&str>, text: &str) {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(3.max(longest + 1));

    out.push_str(&fence);
    if let Some(tag) = lang.map(language_tag).filter(|tag| !tag.is_empty()) {
        out.push_str(tag);
    }
    out.push('\n');
    if !text.is_empty() {
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str(&fence);
}

// ---------------------------------------------------------------------------
// Footnotes
// ---------------------------------------------------------------------------

/// Footnote definitions and reference bookkeeping for one document.
struct Footnotes<'a> {
    /// label → definition body; the first definition of a label wins.
    defs: BTreeMap<&'a str, &'a [Block]>,
    /// label → unique Typst label literal used to point at the footnote.
    names: BTreeMap<&'a str, String>,
    /// labels that are referenced at least once.
    referenced: BTreeSet<&'a str>,
}

impl<'a> Footnotes<'a> {
    /// Collect definitions and references from the whole block tree.
    ///
    /// In Typst a footnote lives at its reference, so the body has to be known
    /// before the body is written; a definition is therefore emitted at its
    /// first reference (with a label, see `footnote_ref`) instead of in place.
    fn collect(blocks: &'a [Block]) -> Self {
        let mut defs: BTreeMap<&'a str, &'a [Block]> = BTreeMap::new();
        let mut referenced: BTreeSet<&'a str> = BTreeSet::new();
        collect_blocks(blocks, &mut defs, &mut referenced);

        let mut all: BTreeSet<&'a str> = BTreeSet::new();
        all.extend(defs.keys().copied());
        all.extend(referenced.iter().copied());

        // Labels are assigned in sorted order so colliding sanitized names
        // always resolve to the same suffixes.
        let mut names = BTreeMap::new();
        let mut used: BTreeSet<String> = BTreeSet::new();
        for label in all {
            let base = format!("fn-{}", sanitize_label(label));
            let mut name = base.clone();
            let mut n = 1u32;
            while used.contains(&name) {
                n += 1;
                name = format!("{base}-{n}");
            }
            used.insert(name.clone());
            names.insert(label, name);
        }

        Self {
            defs,
            names,
            referenced,
        }
    }
}

/// Keep only characters that are valid inside a Typst label literal.
/// API: https://typst.app/docs/reference/foundations/label/ (label charset)
fn sanitize_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        out.push_str("note");
    }
    out
}

fn collect_blocks<'a>(
    blocks: &'a [Block],
    defs: &mut BTreeMap<&'a str, &'a [Block]>,
    referenced: &mut BTreeSet<&'a str>,
) {
    for block in blocks {
        match block {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines } => {
                collect_inlines(inlines, referenced);
            }
            Block::List { items, .. } => {
                for item in items {
                    collect_blocks(item, defs, referenced);
                }
            }
            Block::BlockQuote { blocks } => collect_blocks(blocks, defs, referenced),
            Block::Table { header, rows, .. } => {
                for cell in header {
                    collect_inlines(cell, referenced);
                }
                for row in rows {
                    for cell in row {
                        collect_inlines(cell, referenced);
                    }
                }
            }
            Block::FootnoteDef { label, blocks } => {
                defs.entry(label.as_str()).or_insert(blocks.as_slice());
                collect_blocks(blocks, defs, referenced);
            }
            Block::Code { .. }
            | Block::Math { .. }
            | Block::Diagram { .. }
            | Block::Image { .. }
            | Block::ThematicBreak
            | Block::Html { .. } => {}
        }
    }
}

fn collect_inlines<'a>(inlines: &'a [Inline], referenced: &mut BTreeSet<&'a str>) {
    for inline in inlines {
        match inline {
            Inline::FootnoteRef { label } => {
                referenced.insert(label.as_str());
            }
            Inline::Emph { inlines }
            | Inline::Strong { inlines }
            | Inline::Strike { inlines }
            | Inline::Link { inlines, .. } => collect_inlines(inlines, referenced),
            Inline::Text { .. }
            | Inline::Code { .. }
            | Inline::Image { .. }
            | Inline::Math { .. }
            | Inline::LineBreak { .. } => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Block / inline rendering
// ---------------------------------------------------------------------------

struct Writer<'a> {
    assets: &'a AssetStore,
    math: &'a MathMap,
    diagrams: &'a DiagramMap,
    notes: &'a Footnotes<'a>,
    /// labels whose body has already been typeset at a reference site.
    placed_notes: BTreeSet<String>,
}

impl Writer<'_> {
    /// Render a block sequence, separated by blank lines. Blocks that produce
    /// no output (raw HTML, an already-placed footnote definition, an empty
    /// paragraph) leave no separator behind.
    fn blocks(&mut self, blocks: &[Block], out: &mut String) {
        let base = out.len();
        for block in blocks {
            let before = out.len();
            if out.len() > base {
                out.push_str("\n\n");
            }
            let body = out.len();
            self.block(block, out);
            if out.len() == body {
                out.truncate(before);
            }
        }
    }

    fn block(&mut self, block: &Block, out: &mut String) {
        match block {
            Block::Heading { level, inlines, .. } => {
                // Markdown levels are 1..=6; clamp so a malformed level can
                // never emit zero markers (which would be plain text).
                for _ in 0..(*level).clamp(1, 6) {
                    out.push('=');
                }
                out.push(' ');
                self.inlines(inlines, out);
                while out.ends_with(' ') {
                    out.pop();
                }
            }
            Block::Paragraph { inlines } => self.inlines(inlines, out),
            Block::Code { lang, text, .. } => push_code_block(out, lang.as_deref(), text),
            Block::Math { latex, display } => self.math(latex, *display, out),
            Block::Diagram { source } => {
                if self.diagrams.contains_key(source) {
                    out.push_str("#image(");
                    push_string(&diagram_path(source), out);
                    out.push_str(", width: 100%)");
                } else {
                    // Not rendered: show the source instead of referencing a
                    // file the world cannot serve.
                    push_code_block(out, Some("mermaid"), source);
                }
            }
            Block::Image {
                asset, src, alt, ..
            } => {
                self.image(asset.as_ref(), src, alt, out);
            }
            Block::List {
                ordered,
                start,
                items,
                ..
            } => {
                self.list(*ordered, *start, items, out);
            }
            Block::BlockQuote { blocks } => self.block_quote(blocks, out),
            Block::Table {
                aligns,
                header,
                rows,
            } => self.table(aligns, header, rows, out),
            Block::ThematicBreak => out.push_str("#line(length: 100%)"),
            // Typst has no HTML passthrough; the block is dropped.
            Block::Html { .. } => {}
            Block::FootnoteDef { label, blocks } => self.footnote_def(label, blocks, out),
        }
    }

    /// Render one equation: the converted Typst body when the map has one,
    /// otherwise the raw LaTeX as verbatim text (Typst cannot execute LaTeX, so
    /// emitting it inside `$…$` would fail the whole compile).
    fn math(&self, latex: &str, display: bool, out: &mut String) {
        let Some(expr) = self.lookup_math(latex) else {
            out.push_str("#raw(");
            push_string(trim_math(latex), out);
            out.push(')');
            return;
        };
        let expr = expr.trim();
        if expr.is_empty() {
            return;
        }
        if display {
            out.push_str("$ ");
            out.push_str(expr);
            out.push_str(" $");
        } else {
            out.push('$');
            out.push_str(expr);
            out.push('$');
        }
    }

    /// Look a formula up by its exact raw source first, then without `$` fences
    /// or surrounding whitespace.
    fn lookup_math<'m>(&'m self, latex: &str) -> Option<&'m str> {
        if let Some(expr) = self.math.get(latex) {
            return Some(expr.as_str());
        }
        let trimmed = trim_math(latex);
        if trimmed != latex
            && let Some(expr) = self.math.get(trimmed)
        {
            return Some(expr.as_str());
        }
        None
    }

    fn list(&mut self, ordered: bool, start: Option<u64>, items: &[Vec<Block>], out: &mut String) {
        let first = start.unwrap_or(1);
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            if ordered {
                let _ = write!(out, "{}.", first.saturating_add(i as u64));
            } else {
                out.push('-');
            }
            // Continuation lines must be indented past the marker column;
            // Typst ends an item at the first line that is not (parser:
            // `AtNewline::RequireColumn`).
            let mut body = String::new();
            self.blocks(item, &mut body);
            if !body.is_empty() {
                out.push(' ');
                push_item_body(&body, out);
            }
        }
    }

    fn block_quote(&mut self, blocks: &[Block], out: &mut String) {
        let mut inner = String::new();
        self.blocks(blocks, &mut inner);
        if inner.is_empty() {
            return;
        }
        out.push_str("#quote(block: true)[\n");
        push_indented(&inner, 2, out);
        out.push_str("\n]");
    }

    fn table(
        &mut self,
        aligns: &[Align],
        header: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        out: &mut String,
    ) {
        let cols = header
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
        if cols == 0 {
            return;
        }

        let _ = write!(out, "#table(\n  columns: {cols},\n");
        if !header.is_empty() {
            out.push_str("  table.header(\n");
            for (i, cell) in header.iter().enumerate() {
                out.push_str("    ");
                self.cell(aligns.get(i).copied().unwrap_or(Align::None), cell, out);
                out.push_str(",\n");
            }
            // The header is a row too: pad it so every row has `cols` cells.
            for _ in header.len()..cols {
                out.push_str("    [],\n");
            }
            out.push_str("  ),\n");
        }
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                out.push_str("  ");
                self.cell(aligns.get(i).copied().unwrap_or(Align::None), cell, out);
                out.push_str(",\n");
            }
            // Keep the table rectangular so every row fills all columns.
            for _ in row.len()..cols {
                out.push_str("  [],\n");
            }
        }
        out.push(')');
    }

    fn cell(&mut self, align: Align, inlines: &[Inline], out: &mut String) {
        match align {
            Align::None => {
                out.push('[');
                self.inlines(inlines, out);
                out.push(']');
            }
            Align::Left => self.aligned("left", inlines, out),
            Align::Center => self.aligned("center", inlines, out),
            Align::Right => self.aligned("right", inlines, out),
        }
    }

    fn aligned(&mut self, position: &str, inlines: &[Inline], out: &mut String) {
        // Called from inside `#table(...)`, i.e. code mode: no `#` prefix.
        out.push_str("align(");
        out.push_str(position);
        out.push_str(")[");
        self.inlines(inlines, out);
        out.push(']');
    }

    fn inlines(&mut self, inlines: &[Inline], out: &mut String) {
        for inline in inlines {
            self.inline(inline, out);
        }
    }

    fn inline(&mut self, inline: &Inline, out: &mut String) {
        match inline {
            Inline::Text { text } => push_text(text, out),
            // Raw text is verbatim: a backtick inside the code would end a
            // fenced inline raw, so it goes through a string literal.
            Inline::Code { text } => {
                out.push_str("#raw(");
                push_string(text, out);
                out.push(')');
            }
            Inline::Emph { inlines } => self.function("emph", inlines, out),
            Inline::Strong { inlines } => self.function("strong", inlines, out),
            Inline::Strike { inlines } => self.function("strike", inlines, out),
            Inline::Link { url, inlines, .. } => {
                if url.is_empty() {
                    self.inlines(inlines, out);
                } else {
                    out.push_str("#link(");
                    push_string(url, out);
                    out.push_str(")[");
                    self.inlines(inlines, out);
                    out.push(']');
                }
            }
            Inline::Image {
                asset, src, alt, ..
            } => self.image(asset.as_ref(), src, alt, out),
            Inline::Math { latex, display } => self.math(latex, *display, out),
            Inline::FootnoteRef { label } => self.footnote_ref(label, out),
            Inline::LineBreak { hard } => {
                if *hard {
                    out.push_str("#linebreak()");
                } else {
                    out.push(' ');
                }
            }
        }
    }

    /// `#<function>[<inlines>]` for the content-carrying inline functions.
    fn function(&mut self, function: &str, inlines: &[Inline], out: &mut String) {
        out.push('#');
        out.push_str(function);
        out.push('[');
        self.inlines(inlines, out);
        out.push(']');
    }

    /// Emit an image, or its alternative text when the asset is missing or not
    /// an image: a dangling virtual path would fail the compile.
    fn image(&self, asset: Option<&AssetId>, src: &str, alt: &str, out: &mut String) {
        let resolved = asset.and_then(|id| self.assets.get(id));
        match (asset, resolved) {
            (Some(id), Some(asset)) if asset.kind == AssetKind::Image => {
                out.push_str("#image(");
                push_string(&asset_path(id.as_str()), out);
                if !alt.is_empty() {
                    out.push_str(", alt: ");
                    push_string(alt, out);
                }
                out.push(')');
            }
            _ => {
                let label = if !alt.is_empty() {
                    alt
                } else if src.starts_with("data:") {
                    IMAGE_PLACEHOLDER
                } else {
                    src
                };
                push_text(label, out);
            }
        }
    }

    /// Emit a footnote reference.
    ///
    /// The body is typeset at the first reference and labelled, so later
    /// references point at the same note instead of duplicating it.
    /// API: https://typst.app/docs/reference/model/footnote/ (labelled reference)
    fn footnote_ref(&mut self, label: &str, out: &mut String) {
        let notes = self.notes;
        let Some(name) = notes.names.get(label) else {
            return;
        };

        if let Some(&body) = notes.defs.get(label) {
            if self.placed_notes.insert(label.to_owned()) {
                out.push_str("#footnote[");
                self.note_body(body, out);
                out.push_str("] <");
                out.push_str(name);
                out.push('>');
            } else {
                out.push_str("#footnote(<");
                out.push_str(name);
                out.push_str(">)");
            }
            return;
        }

        // Referenced but never defined: keep the label visible.
        out.push_str("#footnote[");
        push_text(label, out);
        out.push(']');
    }

    /// Emit a definition that no reference points at, so no text is lost.
    fn footnote_def(&mut self, label: &str, blocks: &[Block], out: &mut String) {
        let notes = self.notes;
        if notes.referenced.contains(label) {
            return;
        }
        let Some(name) = notes.names.get(label) else {
            return;
        };
        out.push_str("#footnote[");
        self.note_body(blocks, out);
        out.push_str("] <");
        out.push_str(name);
        out.push('>');
    }

    /// Render a footnote body: inline when it is a single paragraph, otherwise
    /// as a block body inside the footnote's content block.
    fn note_body(&mut self, blocks: &[Block], out: &mut String) {
        if let [Block::Paragraph { inlines }] = blocks {
            self.inlines(inlines, out);
            return;
        }
        let mut inner = String::new();
        self.blocks(blocks, &mut inner);
        if !inner.is_empty() {
            out.push('\n');
            out.push_str(&inner);
            out.push('\n');
        }
    }
}

/// Append the continuation lines of a list item: the first line stays on the
/// marker line, every following non-empty line is indented by two spaces.
fn push_item_body(body: &str, out: &mut String) {
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            if !line.is_empty() {
                out.push_str("  ");
            }
        }
        out.push_str(line);
    }
}

/// Append `text` with every non-empty line indented by `indent` spaces. Blank
/// lines stay blank so paragraph breaks survive.
fn push_indented(text: &str, indent: usize, out: &mut String) {
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if !line.is_empty() {
            for _ in 0..indent {
                out.push(' ');
            }
        }
        out.push_str(line);
    }
}

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
//! Layout follows print conventions instead of Typst's bare defaults, all of it
//! emitted in `push_setup` and in the block writer:
//!
//! - headings are unbreakable and sticky, so none of them ever ends a page
//!   alone,
//! - tables and page-sized images are kept in one piece (`breakable: false`),
//! - an image wider than the text column is scaled down to it, and never
//!   enlarged past its intrinsic size,
//! - lists are tightened to `0.65em` between items,
//! - a document that has footnotes separates them from the body with a
//!   full-width rule.
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

use super::page::{margins, page_size};
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

/// Share of the page height an image may take up and still be kept in one
/// piece. A taller image stays breakable so it can spill onto the following
/// page instead of overflowing this one.
const IMAGE_PAGE_FRACTION: f32 = 0.8;

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
    // The footnote separator rule is only worth emitting for a document that
    // has a footnote at all.
    let has_footnotes = !notes.is_empty();
    let (text_width_pt, page_height_pt) = page_geometry(opts);

    let mut out = String::with_capacity(4096);
    push_setup(&mut out, opts, has_footnotes);
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
        text_width_pt,
        page_height_pt,
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

fn push_setup(out: &mut String, opts: &RenderOptions, has_footnotes: bool) {
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

    // Print default for both kinds of list: items sit closer together than the
    // paragraph leading/spacing Typst would otherwise use between them.
    // API: https://typst.app/docs/reference/model/list/#parameters-spacing
    // API: https://typst.app/docs/reference/model/enum/#parameters-spacing
    push_directive(out, "#set list(spacing: 0.65em)");
    push_directive(out, "#set enum(spacing: 0.65em)");

    // A heading must never end a page on its own: `sticky` keeps it with the
    // block that follows it, `breakable: false` keeps a multi-line heading in
    // one piece. Typst already sets `sticky: true` on heading blocks through
    // its built-in show-set rule, so this states the requirement — and the
    // `breakable` half Typst does not set at all — instead of relying on a
    // default that may move.
    // API: https://typst.app/docs/reference/layout/block/#parameters-sticky
    // API: https://typst.app/docs/reference/layout/block/#parameters-breakable
    push_directive(
        out,
        "#show heading: set block(breakable: false, sticky: true)",
    );

    // Footnotes get a full-width rule above their listing on every page that
    // has one, replacing Typst's 30%-wide default. `line()` defaults to 30pt,
    // so the length is spelled out.
    // API: https://typst.app/docs/reference/model/footnote/ (footnote.entry)
    // API: https://typst.app/docs/reference/visualize/line/#parameters-length
    //
    // Only a document that mentions a footnote needs it, and it has to be a
    // top-level rule: footnote entry properties must be uniform across each
    // page run and are meant to be set before any page content.
    if has_footnotes {
        // A separator is content, so the rule it holds can be written in
        // markup, with the `#line` call a reader expects.
        let separator = "#set footnote.entry(separator: [#line(length: 100%)])";
        push_directive(out, separator);
    }
}

/// The text column width and the page height of `opts`, in points.
///
/// The margin is normalised exactly like the emitted `#set page(…)` length, so
/// the geometry the writer reasons about is the geometry Typst lays out. Typst
/// clips a margin larger than the page, hence the floor at zero: an empty
/// column is the right model for it, not a negative one.
fn page_geometry(opts: &RenderOptions) -> (f32, f32) {
    let (page_width, page_height) = page_size(&opts.page.paper);
    let (margin, ..) = margins(normalized_mm(opts.page.margin_mm));
    ((page_width - 2.0 * margin).max(0.0), page_height)
}

/// Typst paper name for a [`Paper`] variant.
/// API: https://typst.app/docs/reference/layout/page/#parameters-paper
fn paper_name(paper: Paper) -> &'static str {
    match paper {
        Paper::A4 => "a4",
        Paper::Letter => "us-letter",
    }
}

/// Normalise a length in millimetres: negative, non-finite and `-0` values
/// collapse to `0`, so both the formatted length and any geometry derived from
/// it are well formed.
fn normalized_mm(mm: f32) -> f32 {
    if mm.is_finite() && mm > 0.0 { mm } else { 0.0 }
}

/// Append a millimetre length, normalised by [`normalized_mm`] so the emitted
/// length is always well formed.
fn push_mm(out: &mut String, mm: f32) {
    let mm = normalized_mm(mm);
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

    /// Whether the document has no footnote at all — neither a definition nor
    /// a reference. The separator rule is only worth emitting when it is not
    /// empty.
    fn is_empty(&self) -> bool {
        self.defs.is_empty() && self.referenced.is_empty()
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
    /// Width of the text column in points (page width minus both margins).
    text_width_pt: f32,
    /// Height of a full page in points.
    page_height_pt: f32,
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
                //
                // The heading stays `=`-markup (rather than a `#block(…)`
                // wrapper) so the outline and the PDF bookmarks still see a
                // real heading element; the setup rule is what makes its block
                // unbreakable and sticky.
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
                asset,
                src,
                alt,
                width,
                height,
                ..
            } => {
                self.image(asset.as_ref(), src, alt, *width, *height, true, out);
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
            } => self.table_block(aligns, header, rows, out),
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
            self.list_item(item, &mut body);
            if !body.is_empty() {
                out.push(' ');
                push_item_body(&body, out);
            }
        }
    }

    /// Render one list item body.
    ///
    /// An item whose first line is a paragraph of its own is kept together with
    /// the rest of the item: the paragraph becomes a sticky block, so a break
    /// between it and the item's remaining blocks migrates both to the next
    /// page instead of leaving the item's first line alone at the bottom of
    /// this one. Items that hold a single block need no such glue — wrapping
    /// them would glue the item to the *next* item as well and make a long list
    /// unbreakable — and neither do items that open with an image or a table,
    /// which [`Writer::image`] and [`Writer::table_block`] already keep whole.
    /// API: https://typst.app/docs/reference/layout/block/#parameters-sticky
    fn list_item(&mut self, blocks: &[Block], out: &mut String) {
        let [Block::Paragraph { inlines }, rest @ ..] = blocks else {
            return self.blocks(blocks, out);
        };
        // An empty first paragraph has no line to keep, and a single-block item
        // has nothing to keep it with.
        if inlines.is_empty() || rest.is_empty() {
            return self.blocks(blocks, out);
        }
        // The first line is rendered before the rest, so anything stateful —
        // a footnote definition placed at its first reference, for one — stays
        // in document order.
        out.push_str("#block(sticky: true)[");
        self.inlines(inlines, out);
        out.push(']');
        // The remaining blocks are the item's own, one blank line later.
        // `blocks` never separates from content already in the buffer, so the
        // separator has to come from here.
        out.push_str("\n\n");
        self.blocks(rest, out);
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

    /// Render one table as a single unbreakable block.
    ///
    /// A table is read as one unit, so it is never split across pages: its
    /// caption-less body goes into `#block(breakable: false)`, which keeps
    /// rows and their header together on one page.
    /// API: https://typst.app/docs/reference/layout/block/#parameters-breakable
    fn table_block(
        &mut self,
        aligns: &[Align],
        header: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        out: &mut String,
    ) {
        let mut table = String::new();
        self.table(aligns, header, rows, &mut table);
        if table.is_empty() {
            return;
        }
        out.push_str("#block(breakable: false)[");
        out.push_str(&table);
        out.push(']');
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
            } => self.image(asset.as_ref(), src, alt, None, None, false, out),
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
    ///
    /// A resolved image is scaled down to the text column when its intrinsic
    /// size is wider than the column — a percentage width is relative to the
    /// text width — and is never enlarged, because a narrower image keeps its
    /// intrinsic size. A block image that fits on one page additionally goes
    /// into an unbreakable block: a picture split by a page break is not what a
    /// reader expects, while one taller than a page has to stay breakable to
    /// avoid overflowing.
    ///
    /// `width`/`height` are the pixel dimensions the markdown carried or
    /// `mdx-assets` filled in from the payload; whatever they leave open is
    /// taken from the resolved asset.
    // Every field is an independent Markdown image attribute; grouping them
    // into a struct would only move the same list to the call sites.
    // API: https://typst.app/docs/reference/visualize/image/#parameters-width
    #[allow(clippy::too_many_arguments)]
    fn image(
        &self,
        asset: Option<&AssetId>,
        src: &str,
        alt: &str,
        width: Option<u32>,
        height: Option<u32>,
        block: bool,
        out: &mut String,
    ) {
        let resolved = asset.and_then(|id| self.assets.get(id));
        let (Some(id), Some(stored)) = (asset, resolved) else {
            return image_alt(src, alt, out);
        };
        if stored.kind != AssetKind::Image {
            return image_alt(src, alt, out);
        }

        let pixels = match (width.or(stored.width), height.or(stored.height)) {
            (Some(width), Some(height)) => Some((width, height)),
            _ => None,
        };

        let mut image = String::from("#image(");
        push_string(&asset_path(id.as_str()), &mut image);
        if !alt.is_empty() {
            image.push_str(", alt: ");
            push_string(alt, &mut image);
        }
        if pixels.is_some_and(|pixels| self.overflows_text_width(pixels)) {
            image.push_str(", width: 100%");
        }
        image.push(')');

        if block && pixels.is_some_and(|pixels| self.fits_one_page(pixels)) {
            out.push_str("#block(breakable: false)[");
            out.push_str(&image);
            out.push(']');
        } else {
            out.push_str(&image);
        }
    }

    /// Whether an image renders wider than the text column.
    ///
    /// Typst lays an image without an explicit `width` out at its intrinsic
    /// size, `pixels / dpi` inches per axis, where `dpi` falls back to
    /// `Image::DEFAULT_DPI` — 72 — for a payload without resolution metadata.
    /// One pixel is therefore one point here: the closest the manifest can get,
    /// since it records pixel dimensions but no pixel density.
    // API: https://docs.rs/typst-library/0.15.1/typst_library/visualize/struct.Image.html#associatedconstant.DEFAULT_DPI
    fn overflows_text_width(&self, pixels: (u32, u32)) -> bool {
        let (width, _) = pixels;
        width as f32 > self.text_width_pt
    }

    /// Whether an image stays within [`IMAGE_PAGE_FRACTION`] of the page height
    /// at the width it is rendered at, i.e. whether it can be kept in one
    /// piece.
    ///
    /// The rendered width is the one [`Writer::overflows_text_width`] selects:
    /// the text column for a scaled-down image, the intrinsic width otherwise.
    /// An image whose pixel size is unknown cannot be shown to fit and stays
    /// breakable.
    fn fits_one_page(&self, pixels: (u32, u32)) -> bool {
        let (width, height) = pixels;
        if width == 0 || height == 0 {
            return false;
        }
        let intrinsic_width = width as f32;
        let rendered_width = intrinsic_width.min(self.text_width_pt);
        let rendered_height = rendered_width * (height as f32 / intrinsic_width);
        rendered_height <= self.page_height_pt * IMAGE_PAGE_FRACTION
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

/// Append the text an image degrades to when it has no usable asset: the
/// alternative text, the placeholder for a `data:` URI (whose payload would
/// otherwise be poured into the PDF verbatim) or the source path.
fn image_alt(src: &str, alt: &str, out: &mut String) {
    let label = if !alt.is_empty() {
        alt
    } else if src.starts_with("data:") {
        IMAGE_PLACEHOLDER
    } else {
        src
    };
    push_text(label, out);
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

#[cfg(test)]
mod tests {
    use mdx_assets::{Asset, AssetStore};
    use mdx_core::{Align, AssetId, AssetKind, Block, Doc, Inline};

    use crate::{DiagramMap, MathMap, PageSetup, RenderOptions};

    use super::build;

    /// The page set-up the CLI renders with: A4, 20 mm margins, numbered.
    ///
    /// The text column of that set-up is 481.89 pt wide and the page 841.89 pt
    /// high, which is what the image assertions below assume.
    fn options() -> RenderOptions {
        RenderOptions {
            page: PageSetup::default(),
            toc: false,
            pdf_standard: None,
            fonts: Vec::new(),
            ignore_system_fonts: true,
            deterministic_today: None,
            title: None,
        }
    }

    fn source_of(blocks: Vec<Block>, assets: &AssetStore) -> String {
        let doc = Doc::new(blocks);
        build(
            &doc,
            assets,
            &MathMap::new(),
            &DiagramMap::new(),
            &options(),
        )
    }

    /// Build against an empty asset store.
    fn build_blocks(blocks: Vec<Block>) -> String {
        source_of(blocks, &AssetStore::new())
    }

    fn inlines(text: &str) -> Vec<Inline> {
        vec![Inline::Text {
            text: text.to_owned(),
        }]
    }

    fn paragraph(text: &str) -> Block {
        Block::Paragraph {
            inlines: inlines(text),
        }
    }

    fn list(items: Vec<Vec<Block>>) -> Block {
        Block::List {
            ordered: false,
            start: None,
            tight: false,
            items,
        }
    }

    /// A store holding one image asset, with `pixels` as its manifest size.
    fn image_store(id: &str, pixels: Option<(u32, u32)>) -> AssetStore {
        let (width, height) = match pixels {
            Some((width, height)) => (Some(width), Some(height)),
            None => (None, None),
        };
        let mut store = AssetStore::new();
        store.insert(Asset {
            id: AssetId(id.to_owned()),
            kind: AssetKind::Image,
            origin: "pic.png".to_owned(),
            mime: "image/png".to_owned(),
            bytes: Vec::new(),
            sha256: String::new(),
            width,
            height,
        });
        store
    }

    /// A block image whose asset is resolved, with the manifest's pixel size.
    fn image_block(id: &str, width: Option<u32>, height: Option<u32>) -> Block {
        Block::Image {
            asset: Some(AssetId(id.to_owned())),
            src: "pic.png".to_owned(),
            alt: String::new(),
            title: None,
            width,
            height,
        }
    }

    #[test]
    fn headings_are_unbreakable_and_sticky() {
        let blocks = vec![
            Block::Heading {
                level: 1,
                id: "intro".to_owned(),
                inlines: inlines("Intro"),
            },
            paragraph("Body"),
        ];
        let source = build_blocks(blocks);
        assert!(
            source.contains("#show heading: set block(breakable: false, sticky: true)"),
            "{source}"
        );

        // The heading stays markup, so the outline and the PDF bookmarks see a
        // real heading element.
        assert!(source.contains("= Intro"), "{source}");
    }

    #[test]
    fn both_kinds_of_list_are_tightened() {
        let source = build_blocks(vec![list(vec![vec![paragraph("only")]])]);
        assert!(source.contains("#set list(spacing: 0.65em)"), "{source}");
        assert!(source.contains("#set enum(spacing: 0.65em)"), "{source}");
    }

    #[test]
    fn a_multi_block_item_keeps_its_first_line_with_the_item() {
        let source = build_blocks(vec![list(vec![vec![
            paragraph("first"),
            paragraph("second"),
        ]])]);
        assert!(source.contains("- #block(sticky: true)[first]"), "{source}");
        assert!(source.contains("\n\n  second"), "{source}");
    }

    #[test]
    fn a_single_block_item_stays_plain_markup() {
        let source = build_blocks(vec![list(vec![vec![paragraph("only")]])]);
        assert!(source.contains("- only"), "{source}");
        assert!(!source.contains("#block(sticky: true)"), "{source}");
    }

    #[test]
    fn tables_are_kept_in_one_piece() {
        let table = Block::Table {
            aligns: vec![Align::None],
            header: vec![inlines("H")],
            rows: vec![vec![inlines("D")]],
        };
        let source = build_blocks(vec![table]);
        assert!(
            source.contains("#block(breakable: false)[#table("),
            "{source}"
        );
        // The wrapper closes after the table's own closing parenthesis.
        assert!(source.contains("\n)]"), "{source}");
    }

    #[test]
    fn a_wide_image_is_scaled_to_the_text_column() {
        let store = image_store("img-wide", Some((1200, 600)));
        let source = source_of(vec![image_block("img-wide", Some(1200), Some(600))], &store);
        // 1200 px is wider than the 481.89 pt column, and 600/1200 of that
        // column is far below 80 % of the page height.
        assert!(
            source.contains(r#"[#image("/assets/img-wide", width: 100%)]"#),
            "{source}"
        );
        assert!(source.contains("#block(breakable: false)"), "{source}");
    }

    #[test]
    fn an_image_keeps_its_alt_text_ahead_of_the_width() {
        let store = image_store("img-alt", Some((1200, 600)));
        let block = Block::Image {
            asset: Some(AssetId("img-alt".to_owned())),
            src: "pic.png".to_owned(),
            alt: "a photo".to_owned(),
            title: None,
            width: Some(1200),
            height: Some(600),
        };
        let source = source_of(vec![block], &store);
        assert!(
            source.contains(r#"[#image("/assets/img-alt", alt: "a photo", width: 100%)]"#),
            "{source}"
        );
    }

    #[test]
    fn a_narrow_image_keeps_its_intrinsic_size() {
        let store = image_store("img-narrow", Some((320, 200)));
        let source = source_of(
            vec![image_block("img-narrow", Some(320), Some(200))],
            &store,
        );
        // No `width` argument: Typst uses the intrinsic size and never scales
        // the image up to the column.
        assert!(
            source.contains(r#"#block(breakable: false)[#image("/assets/img-narrow")]"#),
            "{source}"
        );
    }

    #[test]
    fn an_image_taller_than_a_page_stays_breakable() {
        let store = image_store("img-tall", Some((300, 4000)));
        let source = source_of(vec![image_block("img-tall", Some(300), Some(4000))], &store);
        assert!(source.contains(r#"#image("/assets/img-tall")"#), "{source}");
        assert!(!source.contains("#block(breakable: false)"), "{source}");
    }

    #[test]
    fn an_image_wider_than_the_column_but_taller_than_a_page_is_only_scaled() {
        let store = image_store("img-both", Some((1200, 4000)));
        let source = source_of(
            vec![image_block("img-both", Some(1200), Some(4000))],
            &store,
        );
        assert!(
            source.contains(r#"#image("/assets/img-both", width: 100%)"#),
            "{source}"
        );
        assert!(!source.contains("#block(breakable: false)"), "{source}");
    }

    #[test]
    fn an_image_without_dimensions_is_left_alone() {
        let store = image_store("img-unknown", None);
        let source = source_of(vec![image_block("img-unknown", None, None)], &store);
        assert!(
            source.contains(r#"#image("/assets/img-unknown")"#),
            "{source}"
        );
        assert!(!source.contains("width:"), "{source}");
        assert!(!source.contains("#block(breakable: false)"), "{source}");
    }

    #[test]
    fn the_manifest_fills_in_the_size_the_block_left_open() {
        let store = image_store("img-fill", Some((1200, 600)));
        let source = source_of(vec![image_block("img-fill", None, None)], &store);
        assert!(
            source.contains(r#"#image("/assets/img-fill", width: 100%)"#),
            "{source}"
        );
    }

    #[test]
    fn an_inline_image_is_never_wrapped_in_a_block() {
        let store = image_store("img-inline", Some((1200, 600)));
        let only_image = Block::Paragraph {
            inlines: vec![Inline::Image {
                asset: Some(AssetId("img-inline".to_owned())),
                src: "pic.png".to_owned(),
                alt: String::new(),
                title: None,
            }],
        };
        let source = source_of(vec![only_image], &store);
        assert!(
            source.contains(r#"#image("/assets/img-inline", width: 100%)"#),
            "{source}"
        );
        assert!(!source.contains("#block(breakable: false)"), "{source}");
    }

    #[test]
    fn footnotes_get_a_separator_rule() {
        let blocks = vec![
            Block::Paragraph {
                inlines: vec![Inline::FootnoteRef {
                    label: "1".to_owned(),
                }],
            },
            Block::FootnoteDef {
                label: "1".to_owned(),
                blocks: vec![paragraph("note")],
            },
        ];
        let source = build_blocks(blocks);
        assert!(
            source.contains("#set footnote.entry(separator: [#line(length: 100%)])"),
            "{source}"
        );
    }

    #[test]
    fn a_document_without_footnotes_gets_no_separator_rule() {
        let source = build_blocks(vec![paragraph("body")]);
        assert!(!source.contains("footnote.entry"), "{source}");
    }

    #[test]
    fn an_empty_document_is_just_the_setup() {
        let source = build_blocks(Vec::new());
        assert!(
            source.starts_with("#set page(paper: \"a4\", margin: 20mm, numbering: \"1\")\n"),
            "{source}"
        );
        assert!(source.ends_with('\n'), "{source}");
        assert!(!source.contains("footnote.entry"), "{source}");
    }

    #[test]
    fn an_unresolved_image_degrades_to_its_alternative_text() {
        let block = Block::Image {
            asset: None,
            src: "missing.png".to_owned(),
            alt: "a photo".to_owned(),
            title: None,
            width: None,
            height: None,
        };
        let source = build_blocks(vec![block]);
        assert!(source.trim_end().ends_with("a photo"), "{source}");
    }

    #[test]
    fn an_unresolved_data_uri_image_degrades_to_the_placeholder() {
        let block = Block::Image {
            asset: None,
            src: "data:image/png;base64,AAAA".to_owned(),
            alt: String::new(),
            title: None,
            width: None,
            height: None,
        };
        let source = build_blocks(vec![block]);
        // `[` and `]` are escaped so the placeholder stays plain text.
        assert!(source.trim_end().ends_with(r"\[image\]"), "{source}");
    }

    #[test]
    fn the_same_document_always_builds_the_same_source() {
        let store = image_store("img-wide", Some((1200, 600)));
        let blocks = || {
            vec![
                image_block("img-wide", Some(1200), Some(600)),
                paragraph("body"),
            ]
        };
        assert_eq!(source_of(blocks(), &store), source_of(blocks(), &store));
    }
}

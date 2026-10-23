//! Rendering of a parsed document into one self-contained HTML document.
//!
//! The output carries no external references: the stylesheet is inlined, math
//! keeps its original LaTeX (so a client-side renderer can pick it up later),
//! diagrams are inlined as sanitised SVG and, when
//! [`HtmlOptions::embed_assets`] is set, images are inlined as `data:` URIs.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use mdx_assets::{Asset, AssetStore};
use mdx_core::{Align, AssetId, Block, Diagnostic, Doc, Inline};

use crate::escape::{escape_into, is_safe_img_url, is_safe_link_url, slugify};
use crate::sanitize::sanitize_fragment;
use crate::{DiagramMap, HtmlOptions, MathMap};

/// Built-in stylesheet: a readable 72-character measure, a system + CJK font
/// stack, table/code/blockquote styling and print rules with page breaks.
const DEFAULT_CSS: &str = r##":root { color-scheme: light; }
body {
  margin: 0;
  padding: 3rem 1.25rem 5rem;
  background: #ffffff;
  color: #1c1e21;
  font-family: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, "Noto Sans", "Noto Sans CJK SC", "Noto Sans CJK TC", "Noto Sans CJK JP", "Source Han Sans SC", "Source Han Sans TC", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif;
  font-size: 16px;
  line-height: 1.7;
  text-rendering: optimizeLegibility;
}
.mdx { max-width: 72ch; margin: 0 auto; }
.mdx > :first-child { margin-top: 0; }
.mdx > :last-child { margin-bottom: 0; }
h1, h2, h3, h4, h5, h6 { margin: 1.8em 0 0.6em; line-height: 1.3; font-weight: 650; }
h1 { font-size: 1.9rem; }
h2 { font-size: 1.5rem; }
h3 { font-size: 1.25rem; }
h4 { font-size: 1.1rem; }
h5, h6 { font-size: 1rem; }
h1, h2 { padding-bottom: 0.25em; border-bottom: 1px solid #e3e6ea; }
p, ul, ol, dl, blockquote, pre, table, figure { margin: 0 0 1.1rem; }
ul, ol { padding-left: 1.6rem; }
li > ul, li > ol { margin-bottom: 0; }
li + li { margin-top: 0.3rem; }
a { color: #0b5cad; text-decoration: underline; text-underline-offset: 2px; }
a:hover { color: #073b73; }
.anchor { margin-left: 0.4em; opacity: 0; color: inherit; text-decoration: none; font-weight: 400; }
h1:hover .anchor, h2:hover .anchor, h3:hover .anchor, h4:hover .anchor, h5:hover .anchor, h6:hover .anchor, .anchor:focus { opacity: 0.4; }
hr { border: 0; border-top: 1px solid #e3e6ea; margin: 2rem 0; }
code, kbd, samp { font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace; font-size: 0.9em; }
:not(pre) > code { background: #f2f3f5; border-radius: 4px; padding: 0.15em 0.35em; }
pre { background: #f6f7f9; border: 1px solid #e3e6ea; border-radius: 6px; padding: 0.9rem 1.05rem; overflow-x: auto; }
pre code { display: block; font-size: 0.875rem; line-height: 1.55; white-space: pre; }
blockquote { margin-inline: 0; padding: 0.1rem 0 0.1rem 1.05rem; border-left: 4px solid #d7dbe0; color: #4c5157; }
blockquote > :last-child { margin-bottom: 0; }
table { border-collapse: collapse; width: 100%; font-size: 0.95rem; }
th, td { border: 1px solid #dfe3e8; padding: 0.4rem 0.65rem; vertical-align: top; text-align: left; }
thead th { background: #f6f7f9; font-weight: 650; }
img, svg { max-width: 100%; height: auto; }
figure { margin-inline: 0; }
.md-image, .diagram { text-align: center; }
figcaption { margin-top: 0.45rem; color: #5a6068; font-size: 0.85rem; }
.math { font-family: "STIX Two Math", "Latin Modern Math", "Cambria Math", "Times New Roman", serif; white-space: nowrap; }
.math-display { display: block; margin: 1.1rem 0; text-align: center; overflow-x: auto; }
.footnotes { margin-top: 3rem; padding-top: 0.75rem; border-top: 1px solid #e3e6ea; font-size: 0.9rem; color: #454a50; }
.footnotes ol { padding-left: 1.4rem; }
.footnote-ref { font-size: 0.75em; vertical-align: super; line-height: 0; }
.footnote-back { margin-left: 0.35em; text-decoration: none; }
.task-list { list-style: none; padding-left: 0.2rem; }
li.task { list-style: none; }
input[type="checkbox"] { margin: 0 0.5em 0 0; vertical-align: middle; }
.image-blocked, .mermaid-source { color: #7a2f2f; }
@media (prefers-color-scheme: dark) {
  :root { color-scheme: dark; }
  body { background: #16181c; color: #dfe3e8; }
  a { color: #7fb3ff; }
  h1, h2 { border-bottom-color: #2b3038; }
  pre, thead th { background: #1d2026; border-color: #2b3038; }
  :not(pre) > code { background: #23262d; }
  blockquote { border-left-color: #3a4048; color: #aeb5bd; }
  th, td { border-color: #2b3038; }
  hr, .footnotes { border-color: #2b3038; }
}
@media print {
  :root { color-scheme: light; }
  body { background: #ffffff; color: #000000; padding: 0; font-size: 11pt; line-height: 1.5; }
  .mdx { max-width: none; }
  a { color: #000000; text-decoration: none; }
  .anchor { display: none; }
  h1, h2 { break-before: page; border-bottom-color: #000000; }
  h1:first-child, h2:first-child { break-before: auto; }
  h1, h2, h3, h4 { break-after: avoid; }
  pre, blockquote, table, figure, img, svg, tr { break-inside: avoid; }
  thead { display: table-header-group; }
  @page { margin: 18mm 16mm; }
}
"##;

/// Render `doc` as a standalone HTML document.
///
/// `math` is accepted for signature parity with `mdx_typst::render_pdf`; the
/// HTML writer emits the *original* LaTeX in `data-tex` instead of the
/// converted Typst expression. `diagrams` maps mermaid source to SVG; a
/// diagram without an entry degrades to an escaped code block rather than
/// failing the whole render.
///
/// The `Result` is part of the frozen crate contract: every representable
/// document renders successfully, and no code path currently produces an
/// error.
pub fn render_html(
    doc: &Doc,
    assets: &AssetStore,
    _math: &MathMap,
    diagrams: &DiagramMap,
    opts: &HtmlOptions,
) -> Result<String, Diagnostic> {
    let mut writer = HtmlWriter::new(assets, diagrams, opts);
    writer.document(doc);
    Ok(writer.out)
}

/// Document title: explicit option, then front matter, then first heading.
fn document_title(doc: &Doc, opts: &HtmlOptions) -> String {
    if let Some(title) = opts.title.as_deref()
        && !title.trim().is_empty()
    {
        return title.to_string();
    }
    if let Some(title) = doc.front.title.as_deref()
        && !title.trim().is_empty()
    {
        return title.to_string();
    }
    if let Some(entry) = mdx_core::doc::collect_toc(&doc.blocks).into_iter().next()
        && !entry.title.trim().is_empty()
    {
        return entry.title;
    }
    String::new()
}

/// Class suffix for a fenced code block language (`rust` → `language-rust`).
fn lang_class(lang: &str) -> String {
    let mut out = String::with_capacity(lang.len());
    for c in lang.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '#') {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// MIME type used for an embedded asset, falling back to the file extension.
fn asset_mime(asset: &Asset) -> String {
    let declared = asset.mime.trim();
    if !declared.is_empty() {
        return declared.to_string();
    }
    mime_guess::from_path(asset.origin.as_str())
        .first_or_octet_stream()
        .to_string()
}

/// `data:` URI for an asset, ready to drop into an `src` attribute.
fn data_uri(asset: &Asset) -> String {
    let mime = asset_mime(asset);
    let mut uri = String::with_capacity(asset.bytes.len() * 4 / 3 + mime.len() + 16);
    uri.push_str("data:");
    uri.push_str(&mime);
    uri.push_str(";base64,");
    STANDARD.encode_string(&asset.bytes, &mut uri);
    uri
}

/// Detects a GFM task marker (`[ ]` / `[x]`) at the start of item text.
fn task_marker(text: &str) -> Option<(bool, &str)> {
    let mut chars = text.chars();
    if chars.next() != Some('[') {
        return None;
    }
    let checked = match chars.next()? {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None,
    };
    if chars.next() != Some(']') {
        return None;
    }
    let rest = text.get(3..).unwrap_or("");
    Some((checked, rest.strip_prefix(' ').unwrap_or(rest)))
}

/// Whether a list item starts with a task marker.
fn item_task(item: &[Block]) -> Option<bool> {
    match item.first() {
        Some(Block::Paragraph { inlines }) => match inlines.first() {
            Some(Inline::Text { text }) => task_marker(text).map(|(checked, _)| checked),
            _ => None,
        },
        _ => None,
    }
}

/// Alignment for column `index`, defaulting to [`Align::None`].
fn align_at(aligns: &[Align], index: usize) -> Align {
    aligns.get(index).copied().unwrap_or(Align::None)
}

struct HtmlWriter<'a> {
    opts: &'a HtmlOptions,
    assets: &'a AssetStore,
    diagrams: &'a DiagramMap,
    out: String,
    footnotes: Vec<(&'a str, &'a [Block])>,
    footnote_numbers: BTreeMap<String, usize>,
    footnote_next: usize,
    footnote_ref_counts: BTreeMap<String, usize>,
    footnote_refs: BTreeMap<String, Vec<String>>,
}

impl<'a> HtmlWriter<'a> {
    fn new(assets: &'a AssetStore, diagrams: &'a DiagramMap, opts: &'a HtmlOptions) -> Self {
        Self {
            opts,
            assets,
            diagrams,
            out: String::with_capacity(4096),
            footnotes: Vec::new(),
            footnote_numbers: BTreeMap::new(),
            footnote_next: 1,
            footnote_ref_counts: BTreeMap::new(),
            footnote_refs: BTreeMap::new(),
        }
    }

    fn raw(&mut self, text: &str) {
        self.out.push_str(text);
    }

    fn esc(&mut self, text: &str) {
        escape_into(text, &mut self.out);
    }

    /// Write ` name="value"` with the value escaped.
    fn attr(&mut self, name: &str, value: &str) {
        self.raw(" ");
        self.raw(name);
        self.raw("=\"");
        self.esc(value);
        self.raw("\"");
    }

    fn opt_attr(&mut self, name: &str, value: Option<&str>) {
        if let Some(value) = value {
            self.attr(name, value);
        }
    }

    fn num_attr(&mut self, name: &str, value: Option<u32>) {
        if let Some(value) = value {
            let _ = write!(self.out, " {name}=\"{value}\"");
        }
    }

    fn document(&mut self, doc: &'a Doc) {
        let title = document_title(doc, self.opts);
        self.raw("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
        self.raw("<meta charset=\"utf-8\">\n");
        self.raw("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
        self.raw("<title>");
        self.esc(&title);
        self.raw("</title>\n");
        if let Some(author) = doc.front.author.as_deref()
            && !author.trim().is_empty()
        {
            self.raw("<meta name=\"author\"");
            self.attr("content", author);
            self.raw(">\n");
        }
        if let Some(date) = doc.front.date.as_deref()
            && !date.trim().is_empty()
        {
            self.raw("<meta name=\"date\"");
            self.attr("content", date);
            self.raw(">\n");
        }
        self.raw("<style>\n");
        self.raw(DEFAULT_CSS);
        if let Some(css) = self.opts.css.as_deref()
            && !css.trim().is_empty()
        {
            self.raw("\n/* caller-supplied css */\n");
            self.raw(css);
            if !css.ends_with('\n') {
                self.raw("\n");
            }
        }
        self.raw("</style>\n</head>\n<body>\n<article class=\"mdx\">\n");
        self.blocks(&doc.blocks, false);
        self.footnote_section();
        self.raw("</article>\n</body>\n</html>\n");
    }

    fn blocks(&mut self, blocks: &'a [Block], tight: bool) {
        for block in blocks {
            self.block(block, tight);
        }
    }

    fn block(&mut self, block: &'a Block, tight: bool) {
        match block {
            Block::Heading { level, id, inlines } => {
                let level = (*level).clamp(1, 6);
                let _ = write!(self.out, "<h{level}");
                let has_id = !id.trim().is_empty();
                if has_id {
                    self.attr("id", id);
                }
                self.raw(">");
                self.inlines(inlines);
                if has_id {
                    self.raw("<a class=\"anchor\" href=\"#");
                    self.esc(id);
                    self.raw("\" aria-hidden=\"true\">#</a>");
                }
                let _ = writeln!(self.out, "</h{level}>");
            }
            Block::Paragraph { inlines } => {
                if tight {
                    self.inlines(inlines);
                    self.raw("\n");
                } else {
                    self.raw("<p>");
                    self.inlines(inlines);
                    self.raw("</p>\n");
                }
            }
            Block::Code { lang, text, meta } => {
                self.raw("<pre><code");
                if let Some(lang) = lang.as_deref() {
                    let class = lang_class(lang);
                    if !class.is_empty() {
                        self.attr("class", &format!("language-{class}"));
                    }
                    self.attr("data-lang", lang);
                }
                self.opt_attr("data-meta", meta.as_deref());
                self.raw(">");
                self.esc(text);
                self.raw("</code></pre>\n");
            }
            Block::Math { latex, display } => {
                if *display {
                    // The `span` shape is what the contract pins down for math;
                    // the wrapper only gives it block-level spacing.
                    self.raw("<div class=\"math-display\">");
                    self.math_span(latex, false);
                    self.raw("</div>\n");
                } else {
                    self.raw("<p>");
                    self.math_span(latex, false);
                    self.raw("</p>\n");
                }
            }
            Block::Diagram { source } => self.diagram(source),
            Block::Image {
                asset,
                src,
                alt,
                title,
                width,
                height,
            } => {
                self.image(asset, src, alt, title.as_deref(), *width, *height, true);
            }
            Block::List {
                ordered,
                start,
                tight: item_tight,
                items,
            } => {
                let tag = if *ordered { "ol" } else { "ul" };
                let task_list = items.iter().any(|item| item_task(item).is_some());
                self.raw("<");
                self.raw(tag);
                if *ordered
                    && let Some(start) = start
                    && *start != 1
                {
                    let _ = write!(self.out, " start=\"{start}\"");
                }
                if task_list {
                    self.attr("class", "task-list");
                }
                self.raw(">\n");
                for item in items {
                    self.raw("<li");
                    if item_task(item).is_some() {
                        self.attr("class", "task");
                    }
                    self.raw(">");
                    self.list_item(item.as_slice(), *item_tight);
                    self.raw("</li>\n");
                }
                self.raw("</");
                self.raw(tag);
                self.raw(">\n");
            }
            Block::BlockQuote { blocks } => {
                self.raw("<blockquote>\n");
                self.blocks(blocks.as_slice(), false);
                self.raw("</blockquote>\n");
            }
            Block::Table {
                aligns,
                header,
                rows,
            } => {
                self.raw("<table>\n");
                if !header.is_empty() {
                    self.raw("<thead>\n<tr>\n");
                    for (index, cell) in header.iter().enumerate() {
                        self.table_cell("th", cell, align_at(aligns, index));
                    }
                    self.raw("</tr>\n</thead>\n");
                }
                if !rows.is_empty() {
                    self.raw("<tbody>\n");
                    for row in rows {
                        self.raw("<tr>\n");
                        for (index, cell) in row.iter().enumerate() {
                            self.table_cell("td", cell, align_at(aligns, index));
                        }
                        self.raw("</tr>\n");
                    }
                    self.raw("</tbody>\n");
                }
                self.raw("</table>\n");
            }
            Block::ThematicBreak => self.raw("<hr>\n"),
            Block::Html { raw } => {
                self.raw(&sanitize_fragment(raw));
                self.raw("\n");
            }
            Block::FootnoteDef { label, blocks } => {
                self.footnote_number(label);
                self.footnotes.push((label.as_str(), blocks.as_slice()));
            }
        }
    }

    /// Render one list item, turning a leading task marker into a checkbox.
    fn list_item(&mut self, item: &'a [Block], tight: bool) {
        for (index, block) in item.iter().enumerate() {
            if index == 0
                && let Block::Paragraph { inlines } = block
                && let Some(Inline::Text { text }) = inlines.first()
                && let Some((checked, rest)) = task_marker(text)
            {
                self.raw("<input type=\"checkbox\" disabled");
                if checked {
                    self.raw(" checked");
                }
                self.raw("> ");
                self.esc(rest);
                self.inlines(&inlines[1..]);
                self.raw("\n");
                continue;
            }
            self.block(block, tight);
        }
    }

    fn table_cell(&mut self, tag: &str, cell: &[Inline], align: Align) {
        self.raw("<");
        self.raw(tag);
        match align {
            Align::None => {}
            Align::Left => {
                self.attr("class", "align-left");
                self.attr("style", "text-align: left");
            }
            Align::Center => {
                self.attr("class", "align-center");
                self.attr("style", "text-align: center");
            }
            Align::Right => {
                self.attr("class", "align-right");
                self.attr("style", "text-align: right");
            }
        }
        self.raw(">");
        self.inlines(cell);
        self.raw("</");
        self.raw(tag);
        self.raw(">\n");
    }

    fn diagram(&mut self, source: &str) {
        let diagrams = self.diagrams;
        match diagrams.get(source) {
            Some(svg) if !svg.trim().is_empty() => {
                self.raw("<figure class=\"diagram\">\n");
                self.raw(&sanitize_fragment(svg));
                self.raw("\n</figure>\n");
            }
            _ => {
                self.raw("<pre class=\"mermaid-source\"><code");
                self.attr("class", "language-mermaid");
                self.raw(">");
                self.esc(source);
                self.raw("</code></pre>\n");
            }
        }
    }

    fn math_span(&mut self, latex: &str, display: bool) {
        self.raw("<span class=\"math");
        if display {
            self.raw(" math-display");
        }
        self.raw("\"");
        self.attr("data-tex", latex);
        self.raw(">");
        self.esc(latex);
        self.raw("</span>");
    }

    /// Emit an image, preferring an embedded `data:` URI over the source path.
    // Every field is an independent Markdown image attribute; grouping them
    // into a struct would only move the same list to the call sites.
    #[allow(clippy::too_many_arguments)]
    fn image(
        &mut self,
        asset: &Option<AssetId>,
        src: &str,
        alt: &str,
        title: Option<&str>,
        width: Option<u32>,
        height: Option<u32>,
        block: bool,
    ) {
        let assets = self.assets;
        let resolved = asset.as_ref().and_then(|id| assets.get(id));
        let mut candidates: Vec<String> = Vec::with_capacity(3);
        if self.opts.embed_assets
            && let Some(asset) = resolved
            && !asset.bytes.is_empty()
        {
            candidates.push(data_uri(asset));
        }
        if !src.trim().is_empty() {
            candidates.push(src.to_string());
        }
        if let Some(asset) = resolved
            && !asset.origin.trim().is_empty()
        {
            candidates.push(asset.origin.clone());
        }
        let chosen = candidates
            .into_iter()
            .find(|candidate| is_safe_img_url(candidate.as_str()));
        let width = width.or_else(|| resolved.and_then(|asset| asset.width));
        let height = height.or_else(|| resolved.and_then(|asset| asset.height));
        if block {
            self.raw("<figure class=\"md-image\">\n");
        }
        match chosen {
            Some(url) => {
                self.raw("<img");
                self.attr("src", &url);
                self.attr("alt", alt);
                self.opt_attr("title", title);
                self.num_attr("width", width);
                self.num_attr("height", height);
                self.raw(if block { ">\n" } else { ">" });
            }
            None => {
                // The only reachable URL was script-bearing: keep the text.
                self.raw("<span class=\"image-blocked\">");
                self.esc(alt);
                self.raw("</span>");
                if block {
                    self.raw("\n");
                }
            }
        }
        if block {
            if let Some(title) = title
                && !title.trim().is_empty()
            {
                self.raw("<figcaption>");
                self.esc(title);
                self.raw("</figcaption>\n");
            }
            self.raw("</figure>\n");
        }
    }

    fn inlines(&mut self, inlines: &[Inline]) {
        for inline in inlines {
            self.inline(inline);
        }
    }

    fn inline(&mut self, inline: &Inline) {
        match inline {
            Inline::Text { text } => self.esc(text),
            Inline::Code { text } => {
                self.raw("<code>");
                self.esc(text);
                self.raw("</code>");
            }
            Inline::Emph { inlines } => {
                self.raw("<em>");
                self.inlines(inlines);
                self.raw("</em>");
            }
            Inline::Strong { inlines } => {
                self.raw("<strong>");
                self.inlines(inlines);
                self.raw("</strong>");
            }
            Inline::Strike { inlines } => {
                self.raw("<del>");
                self.inlines(inlines);
                self.raw("</del>");
            }
            Inline::Link {
                url,
                title,
                inlines,
            } => {
                if is_safe_link_url(url) {
                    self.raw("<a");
                    self.attr("href", url);
                    self.opt_attr("title", title.as_deref());
                    self.raw(">");
                    self.inlines(inlines);
                    self.raw("</a>");
                } else {
                    // Script-bearing scheme: keep the label, drop the link.
                    self.inlines(inlines);
                }
            }
            Inline::Image {
                asset,
                src,
                alt,
                title,
            } => {
                self.image(asset, src, alt, title.as_deref(), None, None, false);
            }
            Inline::Math { latex, display } => self.math_span(latex, *display),
            Inline::FootnoteRef { label } => self.footnote_ref(label),
            Inline::LineBreak { hard } => {
                if *hard {
                    self.raw("<br>\n");
                } else {
                    self.raw(" ");
                }
            }
        }
    }

    /// Stable number for a footnote label, assigned in first-seen order.
    fn footnote_number(&mut self, label: &str) -> usize {
        if let Some(number) = self.footnote_numbers.get(label) {
            return *number;
        }
        let number = self.footnote_next;
        self.footnote_next += 1;
        self.footnote_numbers.insert(label.to_string(), number);
        number
    }

    fn footnote_ref(&mut self, label: &str) {
        let number = self.footnote_number(label);
        let slug = slugify(label);
        let count = self
            .footnote_ref_counts
            .entry(label.to_string())
            .or_insert(0);
        *count += 1;
        let count = *count;
        let anchor = if count == 1 {
            format!("fnref-{slug}")
        } else {
            format!("fnref-{slug}-{count}")
        };
        self.footnote_refs
            .entry(label.to_string())
            .or_default()
            .push(anchor.clone());
        let _ = write!(
            self.out,
            "<sup class=\"footnote-ref\"><a href=\"#fn-{slug}\" id=\"{anchor}\">{number}</a></sup>"
        );
    }

    fn footnote_section(&mut self) {
        let footnotes = std::mem::take(&mut self.footnotes);
        if footnotes.is_empty() {
            return;
        }
        self.raw("<section class=\"footnotes\">\n<ol>\n");
        for &(label, blocks) in &footnotes {
            let number = self.footnote_numbers.get(label).copied().unwrap_or(0);
            let slug = slugify(label);
            let _ = writeln!(self.out, "<li id=\"fn-{slug}\" value=\"{number}\">");
            self.blocks(blocks, false);
            if let Some(anchors) = self.footnote_refs.get(label).cloned() {
                for anchor in &anchors {
                    self.raw("<a class=\"footnote-back\" href=\"#");
                    self.raw(anchor);
                    self.raw("\" aria-label=\"back to reference\">\u{21a9}</a> ");
                }
                self.raw("\n");
            }
            self.raw("</li>\n");
        }
        self.raw("</ol>\n</section>\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdx_assets::{AssetPolicy, resolve_all};

    const PNG_DATA_URI: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR42mP4z8AAAAMBAQD3A0FDAAAAAElFTkSuQmCC";

    fn store_for(doc: &mut Doc) -> AssetStore {
        let policy = AssetPolicy {
            offline: true,
            allow_remote: false,
            cache_dir: None,
            max_bytes: 8 * 1024 * 1024,
        };
        let (store, _report) = resolve_all(doc, &std::env::temp_dir(), &policy);
        store
    }

    fn render_ok(
        doc: &Doc,
        store: &AssetStore,
        diagrams: &DiagramMap,
        opts: &HtmlOptions,
    ) -> String {
        let rendered = render_html(doc, store, &MathMap::new(), diagrams, opts);
        assert!(rendered.is_ok(), "unexpected render failure: {rendered:?}");
        rendered.unwrap_or_default()
    }

    fn paragraph(text: &str) -> Block {
        Block::Paragraph {
            inlines: vec![Inline::Text {
                text: text.to_string(),
            }],
        }
    }

    fn render_blocks(blocks: Vec<Block>, opts: &HtmlOptions) -> String {
        let mut doc = Doc::new(blocks);
        let store = store_for(&mut doc);
        render_ok(&doc, &store, &DiagramMap::new(), opts)
    }

    #[test]
    fn text_and_attributes_are_escaped() {
        let html = render_blocks(
            vec![paragraph(
                "<script>alert(\"x&y\")</script> 'q' \u{4e2d}\u{6587}\u{6d4b}\u{8bd5}",
            )],
            &HtmlOptions::default(),
        );
        assert!(html.contains("&lt;script&gt;alert(&quot;x&amp;y&quot;)&lt;/script&gt;"));
        assert!(html.contains("&#39;q&#39;"));
        assert!(html.contains("\u{4e2d}\u{6587}\u{6d4b}\u{8bd5}"));
        assert!(!html.contains("<script"));
    }

    #[test]
    fn headings_get_ids_and_anchors() {
        let blocks = vec![Block::Heading {
            level: 2,
            id: "intro-1".to_string(),
            inlines: vec![Inline::Text {
                text: "Intro".to_string(),
            }],
        }];
        let html = render_blocks(blocks, &HtmlOptions::default());
        assert!(html.contains(
            "<h2 id=\"intro-1\">Intro<a class=\"anchor\" href=\"#intro-1\" aria-hidden=\"true\">#</a></h2>"
        ));
    }

    #[test]
    fn table_cells_carry_alignment() {
        let cell = |text: &str| {
            vec![Inline::Text {
                text: text.to_string(),
            }]
        };
        let blocks = vec![Block::Table {
            aligns: vec![Align::Center, Align::Right, Align::None],
            header: vec![cell("a"), cell("b"), cell("c")],
            rows: vec![vec![cell("1"), cell("2"), cell("3")]],
        }];
        let html = render_blocks(blocks, &HtmlOptions::default());
        assert!(html.contains("<th class=\"align-center\" style=\"text-align: center\">a</th>"));
        assert!(html.contains("<th class=\"align-right\" style=\"text-align: right\">b</th>"));
        assert!(html.contains("<th>c</th>"));
        assert!(html.contains("<td class=\"align-center\" style=\"text-align: center\">1</td>"));
    }

    #[test]
    fn assets_are_embedded_as_data_uris() {
        let mut doc = Doc::new(vec![Block::Image {
            asset: None,
            src: PNG_DATA_URI.to_string(),
            alt: "pixel".to_string(),
            title: None,
            width: None,
            height: None,
        }]);
        let store = store_for(&mut doc);
        let html = render_ok(&doc, &store, &DiagramMap::new(), &HtmlOptions::default());
        assert!(html.contains("src=\"data:image/"));
        assert!(html.contains(";base64,"));
        assert!(html.contains("base64,iVBORw0KGgo"));
        assert!(html.contains("alt=\"pixel\""));
    }

    #[test]
    fn relative_src_is_kept_when_embedding_is_off() {
        let mut doc = Doc::new(vec![Block::Image {
            asset: None,
            src: "img/pic.png".to_string(),
            alt: "pic".to_string(),
            title: Some("A picture".to_string()),
            width: Some(4),
            height: Some(3),
        }]);
        let store = store_for(&mut doc);
        let opts = HtmlOptions {
            embed_assets: false,
            ..HtmlOptions::default()
        };
        let html = render_ok(&doc, &store, &DiagramMap::new(), &opts);
        assert!(html.contains("<figure class=\"md-image\">"));
        assert!(html.contains("src=\"img/pic.png\""));
        assert!(html.contains("alt=\"pic\""));
        assert!(html.contains("width=\"4\" height=\"3\""));
        assert!(html.contains("<figcaption>A picture</figcaption>"));
        assert!(!html.contains("data:image/"));
    }

    #[test]
    fn diagrams_are_inlined_as_sanitised_svg() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script><style>.n{fill:red}</style><rect width=\"10\" height=\"10\"/></svg>";
        let mut diagrams = DiagramMap::new();
        diagrams.insert("graph TD; A-->B".to_string(), svg.to_string());
        let mut doc = Doc::new(vec![Block::Diagram {
            source: "graph TD; A-->B".to_string(),
        }]);
        let store = store_for(&mut doc);
        let html = render_ok(&doc, &store, &diagrams, &HtmlOptions::default());
        assert!(html.contains("<figure class=\"diagram\">"));
        assert!(html.contains("<svg"));
        assert!(html.contains("<style>.n{fill:red}</style>"));
        assert!(html.contains("<rect width=\"10\" height=\"10\"/>"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("alert(1)"));
    }

    #[test]
    fn diagrams_without_svg_fall_back_to_a_code_block() {
        let mut doc = Doc::new(vec![Block::Diagram {
            source: "graph TD; A-->B".to_string(),
        }]);
        let store = store_for(&mut doc);
        let html = render_ok(&doc, &store, &DiagramMap::new(), &HtmlOptions::default());
        assert!(html.contains("<pre class=\"mermaid-source\"><code class=\"language-mermaid\">"));
        assert!(html.contains("graph TD; A--&gt;B"));
    }

    #[test]
    fn code_blocks_and_inline_code() {
        let blocks = vec![
            Block::Code {
                lang: Some("rust".to_string()),
                text: "let x = 1 < 2;".to_string(),
                meta: Some("title=a.rs".to_string()),
            },
            Block::Paragraph {
                inlines: vec![Inline::Code {
                    text: "a&b".to_string(),
                }],
            },
        ];
        let html = render_blocks(blocks, &HtmlOptions::default());
        assert!(html.contains(
            "<pre><code class=\"language-rust\" data-lang=\"rust\" data-meta=\"title=a.rs\">let x = 1 &lt; 2;</code></pre>"
        ));
        assert!(html.contains("<p><code>a&amp;b</code></p>"));
    }

    #[test]
    fn task_list_items_render_checkboxes() {
        let blocks = vec![Block::List {
            ordered: false,
            start: None,
            tight: true,
            items: vec![vec![paragraph("[x] done")], vec![paragraph("[ ] todo")]],
        }];
        let html = render_blocks(blocks, &HtmlOptions::default());
        assert!(html.contains("<ul class=\"task-list\">"));
        assert!(
            html.contains("<li class=\"task\"><input type=\"checkbox\" disabled checked> done")
        );
        assert!(html.contains("<li class=\"task\"><input type=\"checkbox\" disabled> todo"));
        assert!(!html.contains("[x]"));
    }

    #[test]
    fn lists_quote_start_and_nest() {
        let blocks = vec![Block::List {
            ordered: true,
            start: Some(3),
            tight: false,
            items: vec![vec![
                paragraph("third"),
                Block::List {
                    ordered: false,
                    start: None,
                    tight: true,
                    items: vec![vec![paragraph("nested")]],
                },
            ]],
        }];
        let html = render_blocks(blocks, &HtmlOptions::default());
        assert!(html.contains("<ol start=\"3\">"));
        assert!(html.contains("<p>third</p>"));
        assert!(html.contains("<ul>\n<li>nested\n</li>\n</ul>"));
    }

    #[test]
    fn footnotes_collect_into_a_section() {
        let blocks = vec![
            Block::Paragraph {
                inlines: vec![
                    Inline::Text {
                        text: "see".to_string(),
                    },
                    Inline::FootnoteRef {
                        label: "note-1".to_string(),
                    },
                ],
            },
            Block::FootnoteDef {
                label: "note-1".to_string(),
                blocks: vec![paragraph("the note")],
            },
        ];
        let html = render_blocks(blocks, &HtmlOptions::default());
        assert!(html.contains(
            "<sup class=\"footnote-ref\"><a href=\"#fn-note-1\" id=\"fnref-note-1\">1</a></sup>"
        ));
        assert!(html.contains("<section class=\"footnotes\">"));
        assert!(html.contains("<li id=\"fn-note-1\" value=\"1\">"));
        assert!(html.contains("<a class=\"footnote-back\" href=\"#fnref-note-1\" aria-label=\"back to reference\">\u{21a9}</a>"));
        assert!(html.contains("the note"));
    }

    #[test]
    fn math_keeps_latex_in_data_tex() {
        let blocks = vec![
            Block::Math {
                latex: "E = mc^2".to_string(),
                display: true,
            },
            Block::Paragraph {
                inlines: vec![Inline::Math {
                    latex: "a^2".to_string(),
                    display: false,
                }],
            },
        ];
        let html = render_blocks(blocks, &HtmlOptions::default());
        assert!(html.contains(
            "<div class=\"math-display\"><span class=\"math\" data-tex=\"E = mc^2\">E = mc^2</span></div>"
        ));
        assert!(html.contains("<span class=\"math\" data-tex=\"a^2\">a^2</span>"));
    }

    #[test]
    fn raw_html_blocks_pass_through_and_unsafe_urls_are_neutralised() {
        let blocks = vec![
            Block::Html {
                raw: "<div class=\"callout\">hi<script>bad()</script></div>".to_string(),
            },
            Block::Paragraph {
                inlines: vec![
                    Inline::Link {
                        url: "javascript:alert(1)".to_string(),
                        title: None,
                        inlines: vec![Inline::Text {
                            text: "click".to_string(),
                        }],
                    },
                    Inline::Text {
                        text: " ".to_string(),
                    },
                    Inline::Link {
                        url: "https://example.com/?a=1&b=2".to_string(),
                        title: Some("t\"t".to_string()),
                        inlines: vec![Inline::Text {
                            text: "ok".to_string(),
                        }],
                    },
                    Inline::Image {
                        asset: None,
                        src: "javascript:alert(2)".to_string(),
                        alt: "alt text".to_string(),
                        title: None,
                    },
                ],
            },
        ];
        let html = render_blocks(blocks, &HtmlOptions::default());
        assert!(html.contains("<div class=\"callout\">hi</div>"));
        assert!(!html.contains("bad()"));
        assert!(html.contains(
            "<p>click <a href=\"https://example.com/?a=1&amp;b=2\" title=\"t&quot;t\">ok</a><span class=\"image-blocked\">alt text</span></p>"
        ));
        assert!(!html.contains("href=\"javascript"));
        assert!(!html.contains("src=\"javascript"));
    }

    #[test]
    fn quotes_titles_and_front_matter() {
        let mut doc = Doc::new(vec![
            Block::Heading {
                level: 1,
                id: "t".to_string(),
                inlines: vec![Inline::Text {
                    text: "Heading <Title>".to_string(),
                }],
            },
            Block::BlockQuote {
                blocks: vec![paragraph("quoted")],
            },
            Block::ThematicBreak,
        ]);
        doc.front.title = Some("Front \"Matter\"".to_string());
        doc.front.author = Some("A & B".to_string());
        let store = store_for(&mut doc);
        let html = render_ok(&doc, &store, &DiagramMap::new(), &HtmlOptions::default());
        assert!(html.contains("<title>Front &quot;Matter&quot;</title>"));
        assert!(html.contains("<meta name=\"author\" content=\"A &amp; B\">"));
        assert!(html.contains("<blockquote>\n<p>quoted</p>\n</blockquote>"));
        assert!(html.contains("<hr>"));
        let opts = HtmlOptions {
            title: Some("Explicit".to_string()),
            ..HtmlOptions::default()
        };
        let html = render_ok(&doc, &store, &DiagramMap::new(), &opts);
        assert!(html.contains("<title>Explicit</title>"));
        doc.front.title = None;
        let html = render_ok(&doc, &store, &DiagramMap::new(), &HtmlOptions::default());
        assert!(html.contains("<title>Heading &lt;Title&gt;</title>"));
    }

    #[test]
    fn output_is_self_contained_and_deterministic() {
        let mut doc = Doc::new(vec![
            Block::Heading {
                level: 1,
                id: "t".to_string(),
                inlines: vec![Inline::Text {
                    text: "T".to_string(),
                }],
            },
            paragraph("<b>bold</b>"),
            Block::Code {
                lang: None,
                text: "x".to_string(),
                meta: None,
            },
        ]);
        let store = store_for(&mut doc);
        let opts = HtmlOptions::default();
        let html = render_ok(&doc, &store, &DiagramMap::new(), &opts);
        let again = render_ok(&doc, &store, &DiagramMap::new(), &opts);
        assert_eq!(html, again);
        assert!(html.starts_with("<!DOCTYPE html>\n<html lang=\"en\">"));
        assert!(html.contains("<meta charset=\"utf-8\">"));
        assert!(html.contains("@media print"));
        assert!(html.contains("max-width: 72ch"));
        assert!(html.contains("Noto Sans CJK SC"));
        assert!(!html.contains("<link "));
        assert!(!html.contains("<script"));
        assert!(!html.contains("src=\"http"));
        assert!(html.contains("&lt;b&gt;bold&lt;/b&gt;"));
    }

    #[test]
    fn caller_css_is_appended_after_the_defaults() {
        let opts = HtmlOptions {
            css: Some(".mdx { color: red; }".to_string()),
            ..HtmlOptions::default()
        };
        let html = render_blocks(vec![paragraph("x")], &opts);
        assert!(html.contains("/* caller-supplied css */\n.mdx { color: red; }\n"));
        assert!(html.contains("@media print"));
    }
}

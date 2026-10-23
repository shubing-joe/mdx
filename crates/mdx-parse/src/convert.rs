//! `comrak` AST → [`mdx_core`] document model.
//!
//! [`parse`] is the crate's only entry point. It splits optional YAML front
//! matter off the source, parses the remaining body with `comrak`, and maps
//! every [`NodeValue`] onto [`mdx_core::Block`] / [`mdx_core::Inline`].
//!
//! Conventions the rest of the toolchain relies on (all of them are observable
//! in `mdx ast --json`, so they are effectively part of the contract):
//!
//! * `mermaid` fences become [`Block::Diagram`] with the literal body
//!   verbatim; every other fenced or indented code block becomes
//!   [`Block::Code`] with `lang` = first info-string word, `meta` = the
//!   trimmed remainder, `text` = the literal body.
//! * fences tagged `math` become [`Block::Math`] when [`ParseOptions::math`]
//!   is set. `$…$` / `$$…$$` spans become [`Inline::Math`], or [`Block::Math`]
//!   when the paragraph holds nothing else. LaTeX is copied verbatim: no `$`
//!   fences are added, stripped or converted here (`mdx-math` owns that).
//! * an image that is the only inline of a paragraph becomes [`Block::Image`]
//!   so the asset pass has a block to attach size information to; images mixed
//!   into running text stay [`Inline::Image`]. Both keep `src` verbatim and
//!   `asset: None`.
//! * heading ids are github-style slugs ([`crate::slug::heading_id`]) deduped
//!   in document order ([`crate::slug::dedupe`], `-1`, `-2`, …) and mirrored
//!   into [`Doc::toc`] by [`crate::toc::collect`].
//! * the model has no checkbox and no inline-HTML variant, so task list
//!   markers survive as a leading `[x] ` / `[ ] ` text inline (never dropped)
//!   and inline HTML is dropped with a [`DiagCode::UnsupportedHtml`] warning
//!   that carries its source position.
//!
//! Nothing in an input document can panic this module: node shapes it does not
//! model degrade to their children (or are reported as diagnostics), and the
//! returned [`Report`] always explains whatever was lost.

use comrak::nodes::{ListType, Node, NodeList, NodeTable, NodeValue, Sourcepos, TableAlignment};
use comrak::{Arena, Options, parse_document};
use mdx_core::doc::plain_text;
use mdx_core::{Align, Block, DiagCode, Diagnostic, Doc, FrontMatter, Inline, Report};

use crate::{ParseOptions, front, slug, toc};

/// Parse `source` into a [`Doc`] plus the diagnostics collected on the way.
///
/// `Doc::source_path` is left `None`: the caller (CLI, MCP, GUI) knows the path
/// and owns diagnostics labelling. The returned report holds warnings only —
/// Markdown has no hard failure mode; `E_PARSE` diagnostics are emitted for AST
/// shapes the model cannot express faithfully.
pub fn parse(source: &str, opts: &ParseOptions) -> (Doc, Report) {
    let mut ctx = Ctx {
        opts,
        report: Report::default(),
        body_line: 1,
        heading_slugs: Vec::new(),
    };

    // Front matter is handled before comrak sees the input, so the body's own
    // `---` lines keep their plain Markdown meaning.
    let (front_yaml, body) = if opts.frontmatter {
        let (yaml, body, body_line) = front::split_front_matter(source);
        ctx.body_line = body_line;
        (yaml, body)
    } else {
        (None, source)
    };

    let mut front_matter = FrontMatter::default();
    if let Some(yaml) = front_yaml {
        let (parsed, diagnostic) = front::parse_front(yaml);
        front_matter = parsed;
        if let Some(mut diagnostic) = diagnostic {
            // `parse_front` reports lines relative to the YAML block, whose
            // first line is file line 2 (right after the opening `---`).
            diagnostic.line = diagnostic.line.map(|line| line.saturating_add(1));
            ctx.report.push(diagnostic);
        }
    }

    let arena = Arena::new();
    let root = parse_document(&arena, body, &comrak_options(opts));
    let mut blocks = blocks_from(&mut ctx, root);

    let ids = slug::dedupe(std::mem::take(&mut ctx.heading_slugs));
    apply_heading_ids(&mut blocks, &ids);

    let toc_entries = toc::collect(&blocks);
    let doc = Doc {
        source_path: None,
        front: front_matter,
        blocks,
        toc: toc_entries,
    };
    (doc, ctx.report)
}

/// Translate [`ParseOptions`] into a `comrak` option set.
///
/// Only parsing is configured — this crate never renders Markdown, and options
/// that change text (smart punctuation, default info strings) stay off so the
/// same source always yields the same model.
fn comrak_options(opts: &ParseOptions) -> Options<'static> {
    let mut options = Options::default();
    if opts.gfm {
        options.extension.strikethrough = true;
        options.extension.table = true;
        options.extension.autolink = true;
        options.extension.tasklist = true;
        options.extension.footnotes = true;
    }
    if opts.math {
        options.extension.math_dollars = true;
    }
    options
}

/// Per-parse state: the options, the report being filled, the line offset that
/// turns body-relative source positions into file positions, and the heading
/// slugs in document order (deduped once the walk is complete).
struct Ctx<'a> {
    opts: &'a ParseOptions,
    report: Report,
    /// 1-based line of the body's first line inside the original source.
    body_line: usize,
    heading_slugs: Vec<String>,
}

impl Ctx<'_> {
    /// Body-relative 1-based line → absolute file line. `0` means "unknown".
    fn abs_line(&self, line: usize) -> Option<u32> {
        if line == 0 {
            return None;
        }
        u32::try_from(line.saturating_add(self.body_line).saturating_sub(1)).ok()
    }

    fn warn(&mut self, code: DiagCode, message: impl Into<String>, sourcepos: Sourcepos) {
        let mut diagnostic = Diagnostic::warning(code, message);
        if let Some(line) = self.abs_line(sourcepos.start.line) {
            diagnostic.line = Some(line);
            if let Some(column) = u32::try_from(sourcepos.start.column)
                .ok()
                .filter(|c| *c > 0)
            {
                diagnostic.column = Some(column);
            }
        }
        self.report.push(diagnostic);
    }
}

/// Convert a node's children in document order.
fn blocks_from<'a>(ctx: &mut Ctx<'_>, parent: Node<'a>) -> Vec<Block> {
    let mut blocks = Vec::new();
    for child in parent.children() {
        push_block(ctx, child, &mut blocks);
    }
    blocks
}

fn push_block<'a>(ctx: &mut Ctx<'_>, node: Node<'a>, blocks: &mut Vec<Block>) {
    let ast = node.data();
    match &ast.value {
        NodeValue::Document => blocks.extend(blocks_from(ctx, node)),
        // Consumed by `crate::front` before comrak runs; only reachable if a
        // caller enabled comrak's own front matter extension.
        NodeValue::FrontMatter(_) => {}
        NodeValue::Paragraph => match promoted_block(ctx, node) {
            Some(block) => blocks.push(block),
            None => blocks.push(Block::Paragraph {
                inlines: inlines_from(ctx, node),
            }),
        },
        NodeValue::Heading(heading) => {
            let inlines = inlines_from(ctx, node);
            let id = slug::heading_id(&plain_text(&inlines));
            ctx.heading_slugs.push(id.clone());
            blocks.push(Block::Heading {
                level: heading.level,
                id,
                inlines,
            });
        }
        NodeValue::CodeBlock(code) => {
            let (lang, meta) = split_info(&code.info);
            if lang.eq_ignore_ascii_case("mermaid") {
                blocks.push(Block::Diagram {
                    source: code.literal.clone(),
                });
            } else if ctx.opts.math && lang == "math" {
                blocks.push(Block::Math {
                    latex: code.literal.clone(),
                    display: true,
                });
            } else {
                blocks.push(Block::Code {
                    lang: (!lang.is_empty()).then(|| lang.to_owned()),
                    text: code.literal.clone(),
                    meta: (!meta.is_empty()).then(|| meta.to_owned()),
                });
            }
        }
        NodeValue::HtmlBlock(html) => blocks.push(Block::Html {
            raw: html.literal.clone(),
        }),
        NodeValue::ThematicBreak => blocks.push(Block::ThematicBreak),
        NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
            blocks.push(Block::BlockQuote {
                blocks: blocks_from(ctx, node),
            })
        }
        NodeValue::List(list) => blocks.push(list_block(ctx, node, list)),
        NodeValue::FootnoteDefinition(definition) => blocks.push(Block::FootnoteDef {
            label: definition.name.clone(),
            blocks: blocks_from(ctx, node),
        }),
        NodeValue::Table(table) => blocks.push(table_block(ctx, node, table)),
        // Container block nodes that only appear with extensions this crate
        // leaves off: keep their content rather than dropping it.
        NodeValue::Item(_)
        | NodeValue::DescriptionList
        | NodeValue::DescriptionItem(_)
        | NodeValue::DescriptionDetails
        | NodeValue::BlockDirective(_) => blocks.extend(blocks_from(ctx, node)),
        NodeValue::DescriptionTerm | NodeValue::Subtext => blocks.push(Block::Paragraph {
            inlines: inlines_from(ctx, node),
        }),
        NodeValue::Alert(_) => blocks.push(Block::BlockQuote {
            blocks: blocks_from(ctx, node),
        }),
        NodeValue::Raw(raw) => blocks.push(Block::Html { raw: raw.clone() }),
        // Comments are invisible by design; inline variants never occur in
        // block position.
        NodeValue::MultilineComment(_) | NodeValue::CommentInline(_) => {}
        NodeValue::TableRow(_) | NodeValue::TableCell => {}
        _ => {}
    }
}

/// A paragraph holding exactly one image or math span is a block, not a
/// paragraph: the model carries `asset`/`width`/`height` for images and a
/// `display` flag for math on its block variants only.
fn promoted_block<'a>(ctx: &mut Ctx<'_>, node: Node<'a>) -> Option<Block> {
    let child = sole_child(node)?;
    let ast = child.data();
    match &ast.value {
        NodeValue::Image(link) => {
            let inlines = inlines_from(ctx, child);
            Some(Block::Image {
                asset: None,
                src: link.url.clone(),
                alt: plain_text(&inlines),
                title: link_title(link),
                width: None,
                height: None,
            })
        }
        NodeValue::Math(math) => Some(Block::Math {
            latex: math.literal.clone(),
            display: math.display_math,
        }),
        _ => None,
    }
}

fn list_block<'a>(ctx: &mut Ctx<'_>, node: Node<'a>, list: &NodeList) -> Block {
    let ordered = matches!(list.list_type, ListType::Ordered);
    let mut items = Vec::new();
    for item in node.children() {
        let task_symbol = match &item.data().value {
            NodeValue::TaskItem(task) => Some(task.symbol),
            _ => None,
        };
        let mut blocks = blocks_from(ctx, item);
        if let Some(symbol) = task_symbol {
            let marker = match symbol {
                None => "[ ] ".to_owned(),
                Some(checked) => format!("[{checked}] "),
            };
            prepend_task_marker(&mut blocks, marker);
        }
        items.push(blocks);
    }
    Block::List {
        ordered,
        start: ordered.then_some(list.start as u64),
        tight: list.tight,
        items,
    }
}

/// Record task list state in the only lossless way the model allows. comrak
/// strips the `[x] ` marker from the item text, so it is re-inserted as the
/// first inline of the item's first paragraph.
fn prepend_task_marker(blocks: &mut Vec<Block>, marker: String) {
    if !matches!(blocks.first(), Some(Block::Paragraph { .. })) {
        blocks.insert(
            0,
            Block::Paragraph {
                inlines: Vec::new(),
            },
        );
    }
    if let Some(Block::Paragraph { inlines }) = blocks.first_mut() {
        inlines.insert(0, Inline::Text { text: marker });
    }
}

fn table_block<'a>(ctx: &mut Ctx<'_>, node: Node<'a>, table: &NodeTable) -> Block {
    let mut aligns: Vec<Align> = table.alignments.iter().map(align_of).collect();
    let mut header: Vec<Vec<Inline>> = Vec::new();
    let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
    let mut saw_header = false;

    for row in node.children() {
        let is_header = matches!(row.data().value, NodeValue::TableRow(true));
        let cells: Vec<Vec<Inline>> = row.children().map(|cell| inlines_from(ctx, cell)).collect();
        if is_header && !saw_header {
            saw_header = true;
            header = cells;
        } else {
            rows.push(cells);
        }
    }

    if !saw_header && !rows.is_empty() {
        ctx.warn(
            DiagCode::Parse,
            "table has no header row; its first row was used as the header",
            node.data().sourcepos,
        );
        header = rows.remove(0);
    }

    // comrak pads body rows to the delimiter row's width already; stay
    // defensive so writers can never index past the end of a row.
    let width = header.len().max(aligns.len());
    header.resize(width, Vec::new());
    aligns.resize(width, Align::None);
    for row in rows.iter_mut() {
        row.resize(width, Vec::new());
    }

    Block::Table {
        aligns,
        header,
        rows,
    }
}

fn align_of(align: &TableAlignment) -> Align {
    match align {
        TableAlignment::None => Align::None,
        TableAlignment::Left => Align::Left,
        TableAlignment::Center => Align::Center,
        TableAlignment::Right => Align::Right,
    }
}

fn inlines_from<'a>(ctx: &mut Ctx<'_>, parent: Node<'a>) -> Vec<Inline> {
    let mut inlines = Vec::new();
    for child in parent.children() {
        push_inline(ctx, child, &mut inlines);
    }
    inlines
}

fn push_inline<'a>(ctx: &mut Ctx<'_>, node: Node<'a>, inlines: &mut Vec<Inline>) {
    let ast = node.data();
    let sourcepos = ast.sourcepos;
    match &ast.value {
        NodeValue::Text(text) => {
            if !text.is_empty() {
                inlines.push(Inline::Text {
                    text: text.to_string(),
                });
            }
        }
        NodeValue::SoftBreak => inlines.push(Inline::LineBreak { hard: false }),
        NodeValue::LineBreak => inlines.push(Inline::LineBreak { hard: true }),
        NodeValue::Code(code) => inlines.push(Inline::Code {
            text: code.literal.clone(),
        }),
        NodeValue::Emph => inlines.push(Inline::Emph {
            inlines: inlines_from(ctx, node),
        }),
        NodeValue::Strong => inlines.push(Inline::Strong {
            inlines: inlines_from(ctx, node),
        }),
        NodeValue::Strikethrough => inlines.push(Inline::Strike {
            inlines: inlines_from(ctx, node),
        }),
        NodeValue::Link(link) => inlines.push(Inline::Link {
            url: link.url.clone(),
            title: link_title(link),
            inlines: inlines_from(ctx, node),
        }),
        NodeValue::Image(link) => {
            let children = inlines_from(ctx, node);
            inlines.push(Inline::Image {
                asset: None,
                src: link.url.clone(),
                alt: plain_text(&children),
                title: link_title(link),
            });
        }
        NodeValue::Math(math) => inlines.push(Inline::Math {
            latex: math.literal.clone(),
            display: math.display_math,
        }),
        NodeValue::FootnoteReference(reference) => inlines.push(Inline::FootnoteRef {
            label: reference.name.clone(),
        }),
        NodeValue::HtmlInline(raw) => {
            ctx.warn(
                DiagCode::UnsupportedHtml,
                format!("inline HTML dropped: {raw}"),
                sourcepos,
            );
        }
        NodeValue::WikiLink(wiki) => inlines.push(Inline::Link {
            url: wiki.url.clone(),
            title: None,
            inlines: inlines_from(ctx, node),
        }),
        NodeValue::Raw(raw) => inlines.push(Inline::Text { text: raw.clone() }),
        // Inline wrappers from extensions this crate leaves off: keep the text,
        // drop the wrapper the model cannot express.
        NodeValue::Underline
        | NodeValue::Subscript
        | NodeValue::Superscript
        | NodeValue::Highlight
        | NodeValue::Insert
        | NodeValue::SpoileredText
        | NodeValue::Escaped
        | NodeValue::EscapedTag(_) => inlines.extend(inlines_from(ctx, node)),
        NodeValue::CommentInline(_) | NodeValue::MultilineComment(_) => {}
        _ => {}
    }
}

/// `Some` only when `node` has exactly one child.
fn sole_child<'a>(node: Node<'a>) -> Option<Node<'a>> {
    let mut children = node.children();
    let first = children.next()?;
    if children.next().is_some() {
        None
    } else {
        Some(first)
    }
}

/// `title` is `Some` only when the source actually carried one: comrak models
/// "no title" as an empty string.
fn link_title(link: &comrak::nodes::NodeLink) -> Option<String> {
    (!link.title.is_empty()).then(|| link.title.clone())
}

/// Split a code fence info string into its language and the rest, mirroring
/// comrak's own renderer: `"rust ignore"` → `("rust", "ignore")`.
fn split_info(info: &str) -> (&str, &str) {
    let end = match info.find(|c: char| c.is_ascii_whitespace()) {
        Some(index) => index,
        None => info.len(),
    };
    let (lang, rest) = info.split_at(end);
    (lang, rest.trim())
}

/// Apply deduped ids to heading blocks, in the same document order the slugs
/// were collected in — the order [`crate::toc`] walks: containers first, then
/// their contents.
fn apply_heading_ids(blocks: &mut [Block], ids: &[String]) {
    let mut next = 0usize;
    fn walk(blocks: &mut [Block], ids: &[String], next: &mut usize) {
        for block in blocks.iter_mut() {
            match block {
                Block::Heading { id, .. } => {
                    if let Some(deduped) = ids.get(*next) {
                        *id = deduped.clone();
                    }
                    *next += 1;
                }
                Block::BlockQuote { blocks } => walk(blocks, ids, next),
                Block::List { items, .. } => {
                    for item in items.iter_mut() {
                        walk(item, ids, next);
                    }
                }
                Block::FootnoteDef { blocks, .. } => walk(blocks, ids, next),
                _ => {}
            }
        }
    }
    walk(blocks, ids, &mut next);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks_of(source: &str) -> (Vec<Block>, Report) {
        let (doc, report) = parse(source, &ParseOptions::default());
        (doc.blocks, report)
    }

    fn first_paragraph_inlines(blocks: &[Block]) -> Vec<Inline> {
        for block in blocks {
            match block {
                Block::Paragraph { inlines } => return inlines.clone(),
                Block::List { items, .. } => {
                    for item in items {
                        let found = first_paragraph_inlines(item);
                        if !found.is_empty() {
                            return found;
                        }
                    }
                }
                _ => {}
            }
        }
        Vec::new()
    }

    #[test]
    fn nested_lists_and_quotes_keep_their_structure() {
        let (blocks, report) = blocks_of("- outer\n  - inner\n\n> quoted\n> text\n\n1. one\n");
        assert!(report.diagnostics.is_empty());
        match &blocks[0] {
            Block::List {
                ordered: false,
                tight: true,
                items,
                ..
            } => {
                assert_eq!(items.len(), 1);
                assert!(matches!(
                    items[0].last(),
                    Some(Block::List { ordered: false, .. })
                ));
            }
            other => panic!("expected bullet list, got {other:?}"),
        }
        assert!(
            matches!(&blocks[1], Block::BlockQuote { blocks } if matches!(blocks[0], Block::Paragraph { .. }))
        );
        assert!(matches!(
            &blocks[2],
            Block::List {
                ordered: true,
                start: Some(1),
                ..
            }
        ));
    }

    #[test]
    fn tables_carry_alignment_and_rows() {
        let (blocks, report) = blocks_of("| a | b |\n|:--|--:|\n| 1 | 2 |\n");
        assert!(report.diagnostics.is_empty());
        match &blocks[0] {
            Block::Table {
                aligns,
                header,
                rows,
            } => {
                assert_eq!(aligns.len(), 2);
                assert_eq!(aligns[0], Align::Left);
                assert_eq!(aligns[1], Align::Right);
                assert_eq!(header.len(), 2);
                assert_eq!(plain_text(&header[0]), "a");
                assert_eq!(rows.len(), 1);
                assert_eq!(plain_text(&rows[0][1]), "2");
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn footnotes_become_refs_and_definitions() {
        let (blocks, _) = blocks_of("Hi[^1].\n\n[^1]: A note.\n");
        let inlines = first_paragraph_inlines(&blocks);
        assert!(
            inlines
                .iter()
                .any(|i| matches!(i, Inline::FootnoteRef { label } if label == "1"))
        );
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::FootnoteDef { label, .. } if label == "1"))
        );
    }

    #[test]
    fn task_items_keep_their_marker() {
        let (blocks, _) = blocks_of("- [x] Done\n- [ ] Todo\n");
        match &blocks[0] {
            Block::List { items, .. } => {
                assert_eq!(items.len(), 2);
                assert_eq!(plain_text(&first_paragraph_inlines(&items[0])), "[x] Done");
                assert_eq!(plain_text(&first_paragraph_inlines(&items[1])), "[ ] Todo");
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn inline_code_strike_and_autolinks() {
        let (blocks, _) = blocks_of("Use `code`, ~~gone~~ and www.example.com now.\n");
        let inlines = first_paragraph_inlines(&blocks);
        assert!(
            inlines
                .iter()
                .any(|i| matches!(i, Inline::Code { text } if text == "code"))
        );
        assert!(inlines.iter().any(|i| matches!(i, Inline::Strike { .. })));
        assert!(inlines.iter().any(|i| matches!(
            i,
            Inline::Link { url, .. } if url.starts_with("http://www.example.com")
        )));
    }

    #[test]
    fn html_blocks_pass_through_and_inline_html_warns() {
        let (blocks, report) = blocks_of("<div>\nraw\n</div>\n");
        assert!(matches!(&blocks[0], Block::Html { raw } if raw.contains("<div>")));
        assert!(report.diagnostics.is_empty());

        let (blocks, report) = blocks_of("a <b>bold</b> b\n");
        assert!(report.has_code(DiagCode::UnsupportedHtml));
        assert_eq!(plain_text(&first_paragraph_inlines(&blocks)), "a bold b");
    }

    #[test]
    fn frontmatter_is_split_off_and_parsed() {
        let (doc, report) = parse("---\ntitle: Doc\n---\n\n# Head\n", &ParseOptions::default());
        assert!(report.diagnostics.is_empty());
        assert_eq!(doc.front.title.as_deref(), Some("Doc"));
        assert_eq!(doc.blocks.len(), 1);
        assert!(matches!(&doc.blocks[0], Block::Heading { level: 1, .. }));

        // Without front matter support the `---` must stay plain Markdown.
        let opts = ParseOptions {
            frontmatter: false,
            ..ParseOptions::default()
        };
        let (doc, _) = parse("---\ntitle: Doc\n---\n\n# Head\n", &opts);
        assert!(doc.front.title.is_none());
        assert!(doc.blocks.len() > 1);
    }

    #[test]
    fn duplicate_headings_get_deduped_ids() {
        let (doc, _) = parse("# Same\n\n# Same\n\n> # Same\n", &ParseOptions::default());
        let ids: Vec<&str> = doc.toc.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[1], format!("{}-1", ids[0]));
        assert_eq!(ids[2], format!("{}-2", ids[0]));
    }

    #[test]
    fn fences_map_to_diagram_code_and_math() {
        let (blocks, _) = blocks_of("```mermaid\ngraph TD;\n```\n");
        assert!(matches!(&blocks[0], Block::Diagram { source } if source == "graph TD;\n"));

        let (blocks, _) = blocks_of("```rust ignore\nfn main() {}\n```\n");
        match &blocks[0] {
            Block::Code { lang, text, meta } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(meta.as_deref(), Some("ignore"));
                assert_eq!(text, "fn main() {}\n");
            }
            other => panic!("expected code block, got {other:?}"),
        }

        let (blocks, _) = blocks_of("$$\ne = mc^2\n$$\n");
        assert!(matches!(&blocks[0], Block::Math { display: true, .. }));

        let (blocks, _) = blocks_of("value $x^2$ inside\n");
        assert!(
            first_paragraph_inlines(&blocks)
                .iter()
                .any(|i| matches!(i, Inline::Math { latex, display: false } if latex == "x^2"))
        );
    }

    #[test]
    fn images_are_blocks_alone_and_inline_in_text() {
        let (blocks, _) = blocks_of("![alt text](img/pic.png \"t\")\n");
        match &blocks[0] {
            Block::Image {
                asset: None,
                src,
                alt,
                title,
                ..
            } => {
                assert_eq!(src, "img/pic.png");
                assert_eq!(alt, "alt text");
                assert_eq!(title.as_deref(), Some("t"));
            }
            other => panic!("expected block image, got {other:?}"),
        }

        let (blocks, _) = blocks_of("before ![alt](pic.png) after\n");
        assert!(matches!(&blocks[0], Block::Paragraph { .. }));
        assert!(first_paragraph_inlines(&blocks).iter().any(
            |i| matches!(i, Inline::Image { src, alt, .. } if src == "pic.png" && alt == "alt")
        ));
    }

    #[test]
    fn math_is_left_verbatim_for_the_math_crate() {
        let opts = ParseOptions {
            math: false,
            ..ParseOptions::default()
        };
        let (doc, _) = parse("costs $5 and $6\n", &opts);
        assert!(matches!(&doc.blocks[0], Block::Paragraph { .. }));
        let text = match &doc.blocks[0] {
            Block::Paragraph { inlines } => plain_text(inlines),
            other => panic!("expected paragraph, got {other:?}"),
        };
        assert_eq!(text, "costs $5 and $6");
    }
}

//! Table-of-contents collection.
//!
//! Which blocks carry headings, and in what order they are visited, is owned by
//! [`mdx_core::doc::collect_toc`] (headings nested in block quotes, list items
//! and footnote definitions count; everything else is skipped). This module
//! reuses that structure for levels, ids and ordering, but recomputes every
//! title with [`mdx_core::doc::plain_text`] so the parser's TOC does not depend
//! on how core happens to render a heading.

use mdx_core::doc::{collect_toc, plain_text};
use mdx_core::{Block, Inline, TocEntry};

/// Collect TOC entries from `blocks`.
///
/// Levels, ids and traversal order come from [`mdx_core::doc::collect_toc`];
/// each `title` is recomputed from the heading's inline content through
/// [`mdx_core::doc::plain_text`].
pub fn collect(blocks: &[Block]) -> Vec<TocEntry> {
    let mut entries = collect_toc(blocks);

    let mut headings: Vec<&[Inline]> = Vec::new();
    collect_heading_inlines(blocks, &mut headings);

    for (entry, inlines) in entries.iter_mut().zip(headings) {
        entry.title = plain_text(inlines);
    }

    entries
}

/// Walk the same block shapes as `mdx_core::doc::collect_toc_into`, in the same
/// order, gathering each heading's inlines.
fn collect_heading_inlines<'a>(blocks: &'a [Block], out: &mut Vec<&'a [Inline]>) {
    for block in blocks {
        match block {
            Block::Heading { inlines, .. } => out.push(inlines),
            Block::BlockQuote { blocks } => collect_heading_inlines(blocks, out),
            Block::List { items, .. } => {
                for item in items {
                    collect_heading_inlines(item, out);
                }
            }
            Block::FootnoteDef { blocks, .. } => collect_heading_inlines(blocks, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heading(level: u8, id: &str, text: &str) -> Block {
        Block::Heading {
            level,
            id: id.to_owned(),
            inlines: vec![Inline::Text {
                text: text.to_owned(),
            }],
        }
    }

    #[test]
    fn collects_levels_ids_and_titles_in_order() {
        let blocks = vec![
            heading(1, "intro", "Intro"),
            Block::Paragraph {
                inlines: vec![Inline::Text {
                    text: "body".into(),
                }],
            },
            heading(2, "details", "Details"),
        ];

        let toc = collect(&blocks);
        assert_eq!(toc.len(), 2);
        assert_eq!(
            toc[0],
            TocEntry {
                level: 1,
                id: "intro".into(),
                title: "Intro".into()
            }
        );
        assert_eq!(
            toc[1],
            TocEntry {
                level: 2,
                id: "details".into(),
                title: "Details".into()
            }
        );
    }

    #[test]
    fn titles_are_recomputed_with_plain_text() {
        let blocks = vec![Block::Heading {
            level: 2,
            id: "rich".into(),
            inlines: vec![
                Inline::Text { text: "A ".into() },
                Inline::Strong {
                    inlines: vec![Inline::Text {
                        text: "bold".into(),
                    }],
                },
                Inline::Text {
                    text: " and ".into(),
                },
                Inline::Code {
                    text: "code".into(),
                },
                Inline::Text {
                    text: " plus ".into(),
                },
                Inline::Math {
                    latex: "x^2".into(),
                    display: false,
                },
                Inline::Image {
                    asset: None,
                    src: "a.png".into(),
                    alt: "alt text".into(),
                    title: None,
                },
            ],
        }];

        let toc = collect(&blocks);
        assert_eq!(toc[0].title, "A bold and code plus x^2alt text");
    }

    #[test]
    fn nested_headings_are_collected() {
        let blocks = vec![
            heading(1, "top", "Top"),
            Block::BlockQuote {
                blocks: vec![heading(2, "quoted", "Quoted")],
            },
            Block::List {
                ordered: false,
                start: None,
                tight: true,
                items: vec![vec![heading(3, "item", "Item")]],
            },
            Block::FootnoteDef {
                label: "1".into(),
                blocks: vec![heading(4, "note", "Note")],
            },
        ];

        let toc = collect(&blocks);
        let ids: Vec<&str> = toc.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["top", "quoted", "item", "note"]);
        let levels: Vec<u8> = toc.iter().map(|e| e.level).collect();
        assert_eq!(levels, [1, 2, 3, 4]);
    }

    #[test]
    fn duplicate_heading_titles_keep_their_own_ids() {
        let blocks = vec![
            heading(2, "usage", "Usage"),
            heading(2, "usage-1", "Usage"),
            heading(2, "usage-2", "Usage"),
        ];

        let toc = collect(&blocks);
        let ids: Vec<&str> = toc.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["usage", "usage-1", "usage-2"]);
        assert!(toc.iter().all(|e| e.title == "Usage"));
    }

    #[test]
    fn empty_document_has_empty_toc() {
        assert!(collect(&[]).is_empty());
    }
}

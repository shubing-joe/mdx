//! Document model tests: TOC construction over nested blocks and inline
//! flattening for `plain_text`.
//!
//! Owned by slice 1 (`crates/mdx-core/tests/**`) — see `docs/CONTRACT.md`.

use mdx_core::doc::{collect_toc, plain_text};
use mdx_core::{Block, Doc, Inline, TocEntry};

fn text(s: &str) -> Inline {
    Inline::Text {
        text: s.to_string(),
    }
}

/// Headings appear at top level, inside a block quote, inside list items
/// (including a quote nested in a list item), and inside a footnote
/// definition. Non-heading blocks carry no TOC entry.
fn sample_blocks() -> Vec<Block> {
    vec![
        Block::Heading {
            level: 1,
            id: "intro".into(),
            inlines: vec![text("Intro")],
        },
        Block::Paragraph {
            inlines: vec![text("body text, not a heading")],
        },
        Block::BlockQuote {
            blocks: vec![
                Block::Heading {
                    level: 2,
                    id: "quoted".into(),
                    inlines: vec![
                        Inline::Emph {
                            inlines: vec![text("Quoted ")],
                        },
                        text("Head"),
                    ],
                },
                Block::Paragraph {
                    inlines: vec![text("quoted body")],
                },
            ],
        },
        Block::List {
            ordered: false,
            start: None,
            tight: true,
            items: vec![
                vec![Block::Heading {
                    level: 3,
                    id: "in-list".into(),
                    inlines: vec![text("In list")],
                }],
                vec![Block::BlockQuote {
                    blocks: vec![Block::Heading {
                        level: 4,
                        id: "deep".into(),
                        inlines: vec![text("Deep")],
                    }],
                }],
            ],
        },
        Block::FootnoteDef {
            label: "n1".into(),
            blocks: vec![Block::Heading {
                level: 5,
                id: "foot".into(),
                inlines: vec![Inline::Code {
                    text: "Foot".into(),
                }],
            }],
        },
        // Looks like a heading in text, but is a fenced code block.
        Block::Code {
            lang: Some("markdown".into()),
            text: "# not a heading".into(),
            meta: None,
        },
    ]
}

fn expected_toc() -> Vec<TocEntry> {
    vec![
        TocEntry {
            level: 1,
            id: "intro".into(),
            title: "Intro".into(),
        },
        TocEntry {
            level: 2,
            id: "quoted".into(),
            title: "Quoted Head".into(),
        },
        TocEntry {
            level: 3,
            id: "in-list".into(),
            title: "In list".into(),
        },
        TocEntry {
            level: 4,
            id: "deep".into(),
            title: "Deep".into(),
        },
        TocEntry {
            level: 5,
            id: "foot".into(),
            title: "Foot".into(),
        },
    ]
}

#[test]
fn doc_new_builds_toc_from_nested_headings_in_document_order() {
    let blocks = sample_blocks();
    let doc = Doc::new(blocks.clone());

    assert_eq!(
        doc.toc,
        expected_toc(),
        "heading walk must cover quote/list/footnote nesting"
    );
    assert_eq!(
        collect_toc(&blocks),
        expected_toc(),
        "free function agrees with Doc::new"
    );
    assert_eq!(
        doc.toc.len(),
        5,
        "code-block '#' line must not become a TOC entry"
    );

    // Headings are preserved verbatim in the block tree: same count and ids.
    assert_eq!(doc.blocks, blocks, "Doc::new must not rewrite blocks");
    assert_eq!(
        doc.blocks[2],
        Block::BlockQuote {
            blocks: vec![
                Block::Heading {
                    level: 2,
                    id: "quoted".into(),
                    inlines: vec![
                        Inline::Emph {
                            inlines: vec![text("Quoted ")]
                        },
                        text("Head")
                    ],
                },
                Block::Paragraph {
                    inlines: vec![text("quoted body")]
                },
            ]
        }
    );

    // Depths and ids are carried through unchanged.
    assert_eq!(
        doc.toc
            .iter()
            .map(|e| (e.level, e.id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (1, "intro"),
            (2, "quoted"),
            (3, "in-list"),
            (4, "deep"),
            (5, "foot")
        ]
    );
    // Titles come from `plain_text`, so inline markup is flattened but text kept.
    assert_eq!(doc.toc[1].title, "Quoted Head");
    assert_eq!(doc.toc[4].title, "Foot");
    assert_eq!(
        collect_toc(&[]),
        Vec::<TocEntry>::new(),
        "empty input yields empty TOC"
    );
}

#[test]
fn doc_new_sets_documented_defaults() {
    let doc = Doc::new(Vec::new());

    assert_eq!(doc.source_path, None);
    assert_eq!(doc.front, mdx_core::FrontMatter::default());
    assert_eq!(doc.front.title, None);
    assert_eq!(doc.front.author, None);
    assert_eq!(doc.front.date, None);
    assert!(doc.front.extra.is_empty());
    assert!(doc.blocks.is_empty());
    assert!(doc.toc.is_empty());
}

#[test]
fn plain_text_flattens_emphasis_link_image_and_math() {
    let inlines = vec![
        Inline::Emph {
            inlines: vec![text("a")],
        },
        Inline::Strong {
            inlines: vec![text("b")],
        },
        Inline::Link {
            url: "https://example.com".into(),
            title: Some("t".into()),
            inlines: vec![text("c")],
        },
        Inline::Image {
            asset: None,
            src: "img.png".into(),
            alt: "ALT".into(),
            title: None,
        },
        Inline::Math {
            latex: "x^2".into(),
            display: false,
        },
        text(" "),
        Inline::Code { text: "cc".into() },
        Inline::Strike {
            inlines: vec![text("d")],
        },
        Inline::FootnoteRef { label: "1".into() },
        Inline::LineBreak { hard: true },
        Inline::LineBreak { hard: false },
    ];

    assert_eq!(plain_text(&inlines), "abcALTx^2 ccd1  ");
    assert_eq!(plain_text(&[]), "");
    assert_eq!(
        plain_text(&[Inline::Math {
            latex: "\\frac{1}{2}".into(),
            display: true
        }]),
        "\\frac{1}{2}"
    );
    assert_eq!(
        plain_text(&[Inline::Image {
            asset: Some(mdx_core::AssetId("img-deadbeef".into())),
            src: "a.png".into(),
            alt: "logo".into(),
            title: Some("Logo".into()),
        }]),
        "logo",
        "alt text is used, not the src or title"
    );
    // Display math and inline math flatten identically (raw LaTeX preserved).
    assert_eq!(
        plain_text(&[Inline::Math {
            latex: "e=mc^2".into(),
            display: true
        }]),
        "e=mc^2"
    );
    assert_eq!(
        plain_text(&[text("plain"), Inline::Emph { inlines: vec![] }, text("end")]),
        "plainend"
    );
}

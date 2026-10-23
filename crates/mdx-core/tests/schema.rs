//! Serde schema tests for the `mdx-ast/1` document model: exact JSON shape,
//! `snake_case` tags, and `Doc` → JSON → `Doc` round-tripping.
//!
//! Owned by slice 1 (`crates/mdx-core/tests/**`) — see `docs/CONTRACT.md`.

use mdx_core::{Align, AssetId, AssetKind, Block, Doc, FrontMatter, Inline};

fn text(s: &str) -> Inline {
    Inline::Text {
        text: s.to_string(),
    }
}

/// Canonical minimal document, hand-written so the wire schema is locked.
const MINIMAL_JSON: &str = r#"{
  "source_path": null,
  "front": { "title": null, "author": null, "date": null, "extra": {} },
  "blocks": [
    {
      "type": "heading",
      "level": 1,
      "id": "intro",
      "inlines": [ { "type": "text", "text": "Intro" } ]
    },
    {
      "type": "paragraph",
      "inlines": [
        { "type": "strong", "inlines": [ { "type": "text", "text": "bold" } ] },
        { "type": "math", "latex": "a+b", "display": false }
      ]
    }
  ],
  "toc": [ { "level": 1, "id": "intro", "title": "Intro" } ]
}"#;

fn minimal_doc() -> Doc {
    Doc::new(vec![
        Block::Heading {
            level: 1,
            id: "intro".into(),
            inlines: vec![text("Intro")],
        },
        Block::Paragraph {
            inlines: vec![
                Inline::Strong {
                    inlines: vec![text("bold")],
                },
                Inline::Math {
                    latex: "a+b".into(),
                    display: false,
                },
            ],
        },
    ])
}

/// Every block/inline variant, plus front matter extras.
fn rich_doc() -> Doc {
    let blocks = vec![
        Block::Heading {
            level: 2,
            id: "h-two".into(),
            inlines: vec![
                text("H2"),
                Inline::Math {
                    latex: "a+b".into(),
                    display: false,
                },
            ],
        },
        Block::Paragraph {
            inlines: vec![
                Inline::Strong {
                    inlines: vec![text("bold")],
                },
                Inline::Emph {
                    inlines: vec![text("em")],
                },
                Inline::Strike {
                    inlines: vec![text("strike")],
                },
                Inline::Code {
                    text: "code()".into(),
                },
                Inline::Link {
                    url: "https://example.com/x?y=1".into(),
                    title: Some("Example".into()),
                    inlines: vec![text("link")],
                },
                Inline::Image {
                    asset: Some(AssetId("img-a1b2c3d4".into())),
                    src: "assets/logo.png".into(),
                    alt: "Logo".into(),
                    title: Some("The logo".into()),
                },
                Inline::LineBreak { hard: true },
                Inline::FootnoteRef {
                    label: "fn-1".into(),
                },
            ],
        },
        Block::Code {
            lang: None,
            text: "fn main() {}".into(),
            meta: Some("rust,no_run".into()),
        },
        Block::Math {
            latex: "\\int_0^1 x \\, dx".into(),
            display: true,
        },
        Block::Diagram {
            source: "graph TD; A-->B".into(),
        },
        Block::Image {
            asset: None,
            src: "missing.png".into(),
            alt: String::new(),
            title: None,
            width: Some(640),
            height: None,
        },
        Block::List {
            ordered: true,
            start: Some(3),
            tight: false,
            items: vec![
                vec![Block::Paragraph {
                    inlines: vec![text("one")],
                }],
                vec![Block::List {
                    ordered: false,
                    start: None,
                    tight: true,
                    items: vec![vec![Block::Paragraph {
                        inlines: vec![text("nested")],
                    }]],
                }],
            ],
        },
        Block::BlockQuote {
            blocks: vec![Block::ThematicBreak],
        },
        Block::Table {
            aligns: vec![Align::None, Align::Left, Align::Center, Align::Right],
            header: vec![vec![text("h1")], vec![text("h2")]],
            rows: vec![vec![vec![text("c1")], vec![text("c2")]]],
        },
        Block::ThematicBreak,
        Block::Html {
            raw: "<b>x</b>".into(),
        },
        Block::FootnoteDef {
            label: "fn-1".into(),
            blocks: vec![Block::Paragraph {
                inlines: vec![Inline::FootnoteRef {
                    label: "fn-1".into(),
                }],
            }],
        },
    ];

    let mut doc = Doc::new(blocks);
    doc.source_path = Some("docs/demo.md".into());
    let mut extra = serde_json::Map::new();
    extra.insert("draft".into(), serde_json::Value::Bool(true));
    extra.insert("tags".into(), serde_json::json!(["a", "b"]));
    doc.front = FrontMatter {
        title: Some("Demo".into()),
        author: Some("Joe".into()),
        date: Some("2026-01-01".into()),
        extra,
    };
    doc
}

#[test]
fn doc_round_trips_through_json() {
    let doc = rich_doc();

    let encoded = serde_json::to_string(&doc).expect("Doc must serialize");
    let decoded: Doc = serde_json::from_str(&encoded).expect("Doc must deserialize");
    assert_eq!(decoded, doc, "Doc -> JSON -> Doc must be the identity");

    // Encoding is stable (deterministic, no map-order churn for this model).
    assert_eq!(
        serde_json::to_string(&decoded).expect("re-serialize"),
        encoded
    );

    // Value-level round trip too.
    let value = serde_json::to_value(&doc).expect("Doc -> Value");
    let from_value: Doc = serde_json::from_value(value.clone()).expect("Value -> Doc");
    assert_eq!(from_value, doc);
    assert_eq!(value["front"]["title"], serde_json::json!("Demo"));
    assert_eq!(
        value["front"]["extra"]["tags"],
        serde_json::json!(["a", "b"])
    );
    assert_eq!(value["source_path"], serde_json::json!("docs/demo.md"));
    assert_eq!(
        value["blocks"][1]["inlines"][5]["asset"],
        serde_json::json!("img-a1b2c3d4")
    );
    assert_eq!(
        value["blocks"][5]["height"],
        serde_json::Value::Null,
        "Option<u32> None stays null"
    );
    assert_eq!(value["blocks"][5]["width"], serde_json::json!(640));
    // Derived TOC survives the trip unchanged.
    assert_eq!(decoded.toc.len(), 1);
    assert_eq!(decoded.toc[0].id, "h-two");
}

#[test]
fn json_tags_are_snake_case() {
    let doc = rich_doc();
    let value = serde_json::to_value(&doc).expect("Doc -> Value");
    let encoded = serde_json::to_string(&doc).expect("Doc -> JSON");

    let tags: Vec<&str> = value["blocks"]
        .as_array()
        .expect("blocks array")
        .iter()
        .map(|b| {
            b.get("type")
                .and_then(|t| t.as_str())
                .expect("every block is tagged")
        })
        .collect();
    assert_eq!(
        tags,
        vec![
            "heading",
            "paragraph",
            "code",
            "math",
            "diagram",
            "image",
            "list",
            "block_quote",
            "table",
            "thematic_break",
            "html",
            "footnote_def",
        ]
    );

    let inline_tags: Vec<&str> = value["blocks"][1]["inlines"]
        .as_array()
        .expect("inlines array")
        .iter()
        .map(|i| {
            i.get("type")
                .and_then(|t| t.as_str())
                .expect("every inline is tagged")
        })
        .collect();
    assert_eq!(
        inline_tags,
        vec![
            "strong",
            "emph",
            "strike",
            "code",
            "link",
            "image",
            "line_break",
            "footnote_ref"
        ]
    );

    // Enum values are snake_case scalars, not Rust variant names.
    assert_eq!(
        value["blocks"][8]["aligns"],
        serde_json::json!(["none", "left", "center", "right"])
    );
    assert_eq!(value["blocks"][5]["asset"], serde_json::Value::Null);
    assert!(
        encoded.contains(r#""type":"heading""#),
        "block tag must be 'heading', got: {encoded}"
    );
    assert!(encoded.contains(r#""type":"thematic_break""#));
    assert_eq!(
        value["toc"][0]["type"],
        serde_json::Value::Null,
        "TocEntry is a plain struct, untagged"
    );
    assert_eq!(value["toc"][0]["title"], serde_json::json!("H2a+b"));
}

#[test]
fn deserializing_the_canonical_fixture_matches_the_constructed_doc() {
    let parsed: Doc = serde_json::from_str(MINIMAL_JSON).expect("fixture must deserialize");
    assert_eq!(parsed, minimal_doc());
    assert_eq!(parsed.source_path, None);
    assert_eq!(parsed.front, FrontMatter::default());
    assert_eq!(
        parsed.toc,
        vec![mdx_core::TocEntry {
            level: 1,
            id: "intro".into(),
            title: "Intro".into()
        }]
    );

    // And the constructed doc serializes to exactly the fixture's shape.
    let produced = serde_json::to_value(minimal_doc()).expect("Doc -> Value");
    let expected: serde_json::Value =
        serde_json::from_str(MINIMAL_JSON).expect("fixture is valid JSON");
    assert_eq!(produced, expected);
    assert_eq!(
        produced["blocks"][0]["inlines"][0],
        serde_json::json!({"type": "text", "text": "Intro"})
    );
    assert_eq!(
        produced.as_object().expect("object").len(),
        4,
        "keys: source_path, front, blocks, toc"
    );
}

/// The block-level image produced by the parser (`asset: None`, no dimensions)
/// and the same block after `mdx-assets` fills it (`asset: Some(id)` + width
/// and height) both round-trip through JSON.
#[test]
fn parsed_and_asset_resolved_image_blocks_round_trip() {
    let unresolved = Block::Image {
        asset: None,
        src: "diagram.svg".into(),
        alt: "d".into(),
        title: None,
        width: None,
        height: None,
    };
    let resolved = Block::Image {
        asset: Some(AssetId("img-a1b2c3d4".into())),
        src: "diagram.svg".into(),
        alt: "d".into(),
        title: Some("Diagram".into()),
        width: Some(1024),
        height: Some(768),
    };

    let doc = Doc::new(vec![unresolved.clone(), resolved.clone()]);
    let value = serde_json::to_value(&doc).expect("Doc -> Value");
    assert_eq!(
        value["blocks"][0],
        serde_json::json!({
            "type": "image",
            "asset": null,
            "src": "diagram.svg",
            "alt": "d",
            "title": null,
            "width": null,
            "height": null,
        })
    );
    assert_eq!(
        value["blocks"][1],
        serde_json::json!({
            "type": "image",
            "asset": "img-a1b2c3d4",
            "src": "diagram.svg",
            "alt": "d",
            "title": "Diagram",
            "width": 1024,
            "height": 768,
        })
    );
    assert_eq!(doc.toc, Vec::new(), "image blocks carry no TOC entries");

    let back: Doc =
        serde_json::from_str(&serde_json::to_string(&doc).expect("json")).expect("back");
    assert_eq!(back, doc);
    assert_eq!(back.blocks, vec![unresolved, resolved]);
}

#[test]
fn scalar_model_types_serialize_to_documented_forms() {
    assert_eq!(
        serde_json::to_value(AssetKind::Image).expect("kind"),
        serde_json::json!("image")
    );
    assert_eq!(
        serde_json::to_value(AssetKind::Font).expect("kind"),
        serde_json::json!("font")
    );
    assert_eq!(
        serde_json::to_value(AssetKind::Other).expect("kind"),
        serde_json::json!("other")
    );
    assert_eq!(
        serde_json::to_value(Align::Right).expect("align"),
        serde_json::json!("right")
    );
    assert_eq!(
        serde_json::to_value(AssetId("img-a1b2c3d4".into())).expect("id"),
        serde_json::json!("img-a1b2c3d4")
    );

    // AssetId is a transparent newtype with Display/as_str agreement.
    let id = AssetId("img-deadbeef".into());
    assert_eq!(id.as_str(), "img-deadbeef");
    assert_eq!(id.to_string(), "img-deadbeef");
    assert_eq!(
        serde_json::to_value(&id).expect("id"),
        serde_json::json!("img-deadbeef")
    );

    // FrontMatter extras are free-form JSON and round-trip verbatim.
    let front = FrontMatter {
        title: Some("T".into()),
        author: None,
        date: None,
        extra: serde_json::Map::from_iter([
            ("page".into(), serde_json::json!(7)),
            ("nested".into(), serde_json::json!({"k": [1, 2]})),
        ]),
    };
    let back: FrontMatter =
        serde_json::from_str(&serde_json::to_string(&front).expect("front")).expect("front back");
    assert_eq!(back, front);
    assert_eq!(back.author, None);
    assert_eq!(back.extra["page"], serde_json::json!(7));
    assert_eq!(back.extra["nested"]["k"][1], serde_json::json!(2));
}

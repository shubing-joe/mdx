//! Stable document model (`mdx-ast/1`).

use serde::{Deserialize, Serialize};

/// Opaque content-addressed identifier for a resolved asset, e.g. `img-a1b2c3d4`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub String);

impl AssetId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Image,
    Font,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub author: Option<String>,
    pub date: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TocEntry {
    pub level: u8,
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doc {
    /// Source path as given by the caller (diagnostics only, never used to
    /// resolve assets).
    pub source_path: Option<String>,
    pub front: FrontMatter,
    pub blocks: Vec<Block>,
    pub toc: Vec<TocEntry>,
}

impl Doc {
    pub fn new(blocks: Vec<Block>) -> Self {
        let toc = collect_toc(&blocks);
        Self { source_path: None, front: FrontMatter::default(), blocks, toc }
    }
}

/// Heading walk used by every writer; parser may override ids.
pub fn collect_toc(blocks: &[Block]) -> Vec<TocEntry> {
    let mut out = Vec::new();
    collect_toc_into(blocks, &mut out);
    out
}

fn collect_toc_into(blocks: &[Block], out: &mut Vec<TocEntry>) {
    for block in blocks {
        match block {
            Block::Heading { level, id, inlines } => out.push(TocEntry {
                level: *level,
                id: id.clone(),
                title: crate::doc::plain_text(inlines),
            }),
            Block::BlockQuote { blocks } => collect_toc_into(blocks, out),
            Block::List { items, .. } => {
                for item in items {
                    collect_toc_into(item, out);
                }
            }
            Block::FootnoteDef { blocks, .. } => collect_toc_into(blocks, out),
            _ => {}
        }
    }
}

/// Flatten inline content to plain text (used for TOC, alt text, PDF metadata).
pub fn plain_text(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::Text { text } | Inline::Code { text } => s.push_str(text),
            Inline::Emph { inlines }
            | Inline::Strong { inlines }
            | Inline::Strike { inlines }
            | Inline::Link { inlines, .. } => s.push_str(&plain_text(inlines)),
            Inline::Image { alt, .. } => s.push_str(alt),
            Inline::Math { latex, .. } => s.push_str(latex),
            Inline::FootnoteRef { label } => s.push_str(label),
            Inline::LineBreak { .. } => s.push(' '),
        }
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Heading {
        level: u8,
        id: String,
        inlines: Vec<Inline>,
    },
    Paragraph {
        inlines: Vec<Inline>,
    },
    Code {
        lang: Option<String>,
        text: String,
        meta: Option<String>,
    },
    Math {
        latex: String,
        display: bool,
    },
    /// A fenced code block with `mermaid` info string.
    Diagram {
        source: String,
    },
    Image {
        /// Resolved asset; `None` only when resolution failed (diagnostic emitted).
        asset: Option<AssetId>,
        src: String,
        alt: String,
        title: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
    },
    List {
        ordered: bool,
        start: Option<u64>,
        tight: bool,
        items: Vec<Vec<Block>>,
    },
    BlockQuote {
        blocks: Vec<Block>,
    },
    Table {
        aligns: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    ThematicBreak,
    /// Raw HTML passed through when the target supports it, otherwise ignored.
    Html {
        raw: String,
    },
    FootnoteDef {
        label: String,
        blocks: Vec<Block>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inline {
    Text {
        text: String,
    },
    Code {
        text: String,
    },
    Emph {
        inlines: Vec<Inline>,
    },
    Strong {
        inlines: Vec<Inline>,
    },
    Strike {
        inlines: Vec<Inline>,
    },
    Link {
        url: String,
        title: Option<String>,
        inlines: Vec<Inline>,
    },
    Image {
        asset: Option<AssetId>,
        src: String,
        alt: String,
        title: Option<String>,
    },
    Math {
        latex: String,
        display: bool,
    },
    FootnoteRef {
        label: String,
    },
    LineBreak {
        hard: bool,
    },
}

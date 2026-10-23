//! Self-contained HTML rendering for `mdx`.
//!
//! [`render_html`] turns an [`mdx_core::Doc`] plus resolved assets and
//! pre-rendered math/diagram artefacts into a single HTML document that
//! references nothing on the network and nothing beside itself: the
//! stylesheet is inlined, images are embedded as `data:` URIs (by default),
//! diagrams are inlined as sanitised SVG and math keeps its original LaTeX so
//! the host application can render it later.
//!
//! Internals are split into [`render`] (document walking), `escape` (text,
//! identifier and URL escaping) and `sanitize` (removal of scriptable content
//! from inlined SVG/HTML).

use std::collections::BTreeMap;

mod escape;
pub mod render;
mod sanitize;

pub use render::render_html;

/// LaTeX → Typst expression map.
///
/// Identical in shape to `mdx_typst::MathMap` so the CLI can pass one value to
/// either writer.
pub type MathMap = BTreeMap<String, String>;

/// Mermaid source → SVG map.
///
/// Identical in shape to `mdx_typst::DiagramMap`.
pub type DiagramMap = BTreeMap<String, String>;

/// Options controlling [`render_html`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlOptions {
    /// Inline every resolved asset as a `data:` URI instead of pointing at the
    /// original path. Defaults to `true`: the HTML output is meant to be a
    /// single portable file.
    pub embed_assets: bool,
    /// Extra CSS, appended after the built-in stylesheet so it can override it.
    pub css: Option<String>,
    /// Document title; falls back to front matter and then to the first heading.
    pub title: Option<String>,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            embed_assets: true,
            css: None,
            title: None,
        }
    }
}

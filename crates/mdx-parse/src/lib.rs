//! Markdown → [`mdx_core`] document model.
//!
//! The crate is the only place that knows about Markdown syntax: everything
//! downstream (asset resolution, math/diagram conversion, Typst, HTML, GUI,
//! MCP) consumes [`mdx_core::Doc`] built by [`parse`].
//!
//! Layout:
//!
//! * [`convert`] — the `comrak` AST walk that fills [`mdx_core::Doc`].
//! * [`front`] — leading YAML front matter (`---` … `---`).
//! * [`slug`] — github-style heading slugs and their `-1`, `-2` deduplication.
//! * [`toc`] — table of contents derived from the parsed blocks.
//!
//! ```
//! use mdx_parse::{parse, ParseOptions};
//!
//! let (doc, report) = parse("# Title\n\nHello *world*.\n", &ParseOptions::default());
//! assert!(report.diagnostics.is_empty());
//! assert_eq!(doc.blocks.len(), 2);
//! ```

pub mod convert;
pub mod front;
pub mod slug;
pub mod toc;

pub use convert::parse;

/// Feature switches for [`parse`].
///
/// Every switch is independent: turning one off leaves the corresponding
/// syntax as ordinary text (or, for `frontmatter`, as ordinary Markdown
/// blocks) instead of reporting an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOptions {
    /// GitHub Flavored Markdown extensions: tables, strikethrough, autolinks,
    /// task list items and footnotes.
    pub gfm: bool,
    /// Math spans: `$…$` and `$$…$$` become [`mdx_core::Inline::Math`] /
    /// [`mdx_core::Block::Math`], and ` ```math ` fences become
    /// [`mdx_core::Block::Math`]. LaTeX is kept verbatim — conversion is
    /// `mdx-math`'s job.
    pub math: bool,
    /// A leading YAML block delimited by `---` lines is parsed into
    /// [`mdx_core::Doc::front`] instead of being parsed as Markdown.
    pub frontmatter: bool,
}

impl Default for ParseOptions {
    /// Everything on: this is what `mdx render` uses unless the caller
    /// (CLI flags, `md_parse` MCP tool) narrows it.
    fn default() -> Self {
        Self {
            gfm: true,
            math: true,
            frontmatter: true,
        }
    }
}

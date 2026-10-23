//! SVG previews of a laid-out Typst document.
//!
//! [`render_pages`] turns a compiled [`PagedDocument`] into exactly one
//! standalone SVG string per page, in document order, so the GUI preview pane
//! (and the MCP `md_render` SVG output) can show a page in an image widget
//! without touching the filesystem.
//!
//! Every produced document is self-contained: the `typst-svg` exporter inlines
//! glyph outlines, images as base64 `data:` URLs and all referenced
//! gradients/tilings inside the same string. Nothing is read from or written to
//! disk here, and the layout (fonts, images) has already been resolved by the
//! Typst world before this module is called.

use mdx_core::{DiagCode, Diagnostic};

// API: `PagedDocument` is defined by the `typst-layout` crate. typst 0.15.1
// only re-exports `typst-library` from its crate root, so the type is *not*
// reachable as `typst::layout::PagedDocument` (that module is
// `typst-library::layout`, which holds `Frame`/`Abs`/… but not the document).
// https://docs.rs/typst-layout/0.15.1/typst_layout/struct.PagedDocument.html
use typst_layout::PagedDocument;

/// The number of pages in a laid-out document.
///
/// A successfully compiled document always has at least one page.
pub fn page_count(doc: &PagedDocument) -> usize {
    doc.pages().len()
}

/// Render every page of `doc` to its own standalone SVG.
///
/// The returned vector holds exactly [`page_count`] entries in document order;
/// each entry starts with `<svg`, carries `width`/`height` (and `viewBox`)
/// attributes and embeds all of its resources, so it can be handed straight to
/// an image widget.
///
/// # Errors
///
/// Returns `E_TYPST` if a page fails to produce a standalone SVG document. The
/// `typst-svg` exporter itself is infallible (`fn svg(..) -> String`), so this
/// check is the audit of its output rather than a mapped library error.
pub fn render_pages(doc: &PagedDocument) -> Result<Vec<String>, Diagnostic> {
    // `SvgOptions::default()` is the compact form (no bleed, no pretty
    // printing): the exporter writes the `<svg>` root element first, with no
    // XML declaration and no leading whitespace.
    let opts = typst_svg::SvgOptions::default();
    let pages = doc.pages();
    let mut out = Vec::with_capacity(pages.len());

    for (index, page) in pages.iter().enumerate() {
        let svg = typst_svg::svg(page, &opts);
        if !svg.starts_with("<svg") {
            return Err(Diagnostic::error(
                DiagCode::Typst,
                format!(
                    "SVG export of page {} did not produce a standalone <svg> document",
                    index + 1
                ),
            ));
        }
        out.push(svg);
    }

    Ok(out)
}

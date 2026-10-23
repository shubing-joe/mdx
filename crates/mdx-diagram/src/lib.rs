//! Mermaid diagrams for mdx, rendered as resvg-safe SVG.
//!
//! The HTML and Typst/PDF outputs of `mdx` embed diagrams as SVG, so this crate
//! owns exactly two things:
//!
//! * [`render_svg`] turns one Mermaid source into a standalone SVG document with
//!   no `<foreignObject>` HTML labels, so resvg/usvg-based consumers — the Typst
//!   page preview and the self-contained HTML output — can rasterize it;
//! * [`cache`] derives the content-addressed key callers use to memoize that
//!   work and to give each diagram a document-unique id.
//!
//! Rendering is deterministic and host independent: the same source and options
//! produce the same bytes, ids included. See [`render`] for the metric sources
//! and the stable error codes.
//!
//! ```
//! # fn main() -> Result<(), String> {
//! let options = mdx_diagram::DiagramOptions::default();
//! let svg = mdx_diagram::render_svg("flowchart TD\nA[Start] --> B[Done]", &options)?;
//! assert!(svg.starts_with("<svg"));
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod cache;
pub mod render;

pub use cache::{Memo, key as cache_key};
pub use render::{DiagramOptions, render_svg};

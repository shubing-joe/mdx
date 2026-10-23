//! LaTeX math for the `mdx` toolchain.
//!
//! Two backends sit behind one contract, so that the Typst writer, the HTML
//! writer, the CLI and the MCP server all agree on what a formula becomes:
//!
//! * [`typst::latex_to_typst`] (re-exported at the crate root as
//!   `latex_to_typst`) converts LaTeX math into a **Typst math expression
//!   body** — the text that belongs between `$…$` — and is the primary path.
//! * [`svg::latex_to_svg`] (re-exported at the crate root as `latex_to_svg`)
//!   renders LaTeX math straight to a standalone SVG with RaTeX, the fallback
//!   for formulas the Typst path cannot express.
//!
//! Both entry points take the raw LaTeX a caller got from `mdx-parse` (fences
//! already removed by the parser, but tolerated either way) and return
//! `Result<_, String>` with a short, snippet-carrying reason on failure. Neither
//! panics on malformed input.

#![forbid(unsafe_code)]

pub mod svg;
pub mod typst;

pub use svg::latex_to_svg;
pub use typst::latex_to_typst;

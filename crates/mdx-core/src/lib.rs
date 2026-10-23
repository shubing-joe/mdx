//! Shared document model for the `mdx` toolchain.
//!
//! Everything downstream (Typst writer, HTML writer, GUI, MCP) consumes
//! [`Doc`] instead of re-parsing Markdown. Asset references are opaques
//! [`AssetId`]s resolved by `mdx-assets`; no downstream crate may touch
//! filesystem paths on its own.

pub mod diag;
pub mod doc;
pub mod exit;

pub use diag::{DiagCode, Diagnostic, Report, Severity};
pub use doc::{Align, AssetId, AssetKind, Block, Doc, FrontMatter, Inline, TocEntry};
pub use exit::exit_code;

//! PDF export: a laid-out Typst document becomes the bytes of a PDF file.
//!
//! [`export`] is the single translation step between Typst's [`PagedDocument`]
//! and what `mdx render -o doc.pdf` writes: it maps the CLI-facing
//! [`RenderOptions`] onto [`PdfOptions`] and hands the document to Typst's PDF
//! writer.
//!
//! **Bookmarks:** typst-pdf builds the PDF outline unconditionally from the
//! document's headings, so every heading the writer emits turns into a bookmark
//! node and no extra option is needed to enable them.
//! // API: https://docs.rs/typst-pdf/0.15.1/typst_pdf/fn.pdf.html
//!
//! **Determinism:** no wall clock ever reaches the PDF. `PdfOptions::timestamp`
//! is always `None`, so Typst only writes a creation date when the document sets
//! `date` explicitly; the deterministic clock lives in the Typst world, which
//! both fixes `today()` and keeps two exports of the same document
//! byte-identical.
//!
//! // API: https://docs.rs/typst-pdf/0.15.1/typst_pdf/struct.PdfOptions.html

use mdx_core::{DiagCode, Diagnostic};
use typst::diag::{HintedString, SourceDiagnostic};
use typst::foundations::Smart;
use typst_pdf::{PdfOptions, PdfStandard, PdfStandards};
// API: `PagedDocument` lives in the `typst-layout` crate as of Typst 0.15; the
// `typst` facade re-exports `typst-library` only, whose `layout` module carries
// the layout primitives but no document type.
// https://docs.rs/typst-layout/0.15.1/typst_layout/struct.PagedDocument.html
use typst_layout::PagedDocument;

use super::RenderOptions;

/// Maximum number of Typst errors folded into one [`Diagnostic`] message; the
/// remainder is reported as a count so a failing standard cannot produce a
/// message of unbounded length.
const MAX_REPORTED_ERRORS: usize = 5;

/// Writes `doc` to PDF bytes.
///
/// The PDF standard requested through [`RenderOptions::pdf_standard`] is passed
/// through verbatim; the document identifier is derived from
/// [`RenderOptions::title`] so repeated exports of the same document agree, and
/// no wall-clock timestamp is embedded.
///
/// Fails with [`DiagCode::Typst`] (`E_TYPST`) when Typst rejects the requested
/// standard or the PDF writer reports an error of its own (unsupported font,
/// failed standard validation, …).
pub fn export(doc: &PagedDocument, opts: &RenderOptions) -> Result<Vec<u8>, Diagnostic> {
    let options = pdf_options(opts)?;
    typst_pdf::pdf(doc, &options).map_err(|errors| export_failure(&errors))
}

/// Translates [`RenderOptions`] into Typst's PDF export settings.
///
/// Every field is spelled out (no `..PdfOptions::default()`) so a change in
/// Typst's defaults cannot silently alter mdx output.
///
/// // API: https://docs.rs/typst-pdf/0.15.1/typst_pdf/struct.PdfOptions.html
fn pdf_options(opts: &RenderOptions) -> Result<PdfOptions, Diagnostic> {
    Ok(PdfOptions {
        // A stable identifier keeps PDF viewers' and incremental tools' notion
        // of the document stable across recompilations. `Smart::Auto` hashes
        // title and author when no identifier is available.
        ident: ident(opts),
        // Keep Typst's default `/Creator` (`Typst <version>`).
        creator: Smart::Auto,
        // Never a wall-clock date; see the module documentation.
        timestamp: None,
        // Export the whole document; page selection is not a CLI feature.
        page_ranges: None,
        standards: standards(opts.pdf_standard.as_slice())?,
        // Tagged PDF is the accessibility baseline PDF/UA-1 builds on, and the
        // structure tree is what carries the headings; Typst defaults to
        // `true` and mdx keeps it that way for every export.
        tagged: true,
        // Compact output: humans review the SVG page previews, not the PDF.
        pretty: false,
    })
}

/// The PDF document identifier, derived from the document title when there is
/// one so that the same document always exports with the same identity.
fn ident(opts: &RenderOptions) -> Smart<String> {
    match opts.title.as_deref() {
        Some(title) if !title.trim().is_empty() => Smart::Custom(title.to_owned()),
        _ => Smart::Auto,
    }
}

/// Validates the requested PDF standards against Typst's configuration rules.
///
/// An empty list means "no conformance requirement" and yields Typst's
/// default configuration (PDF 1.7).
fn standards(list: &[PdfStandard]) -> Result<PdfStandards, Diagnostic> {
    if list.is_empty() {
        return Ok(PdfStandards::default());
    }

    PdfStandards::new(list).map_err(|error| {
        let detail = describe_hinted(&error);
        Diagnostic::error(
            DiagCode::Typst,
            format!("PDF standard not supported: {detail}"),
        )
    })
}

/// One Typst configuration error as a single line: message plus its hints.
fn describe_hinted(error: &HintedString) -> String {
    let mut line = error.message().to_string();
    for hint in error.hints() {
        line.push_str(" (hint: ");
        line.push_str(hint.as_str());
        line.push(')');
    }
    line
}

/// A failed export as one [`DiagCode::Typst`] diagnostic.
fn export_failure(errors: &[SourceDiagnostic]) -> Diagnostic {
    let mut parts = Vec::new();
    for error in errors.iter().take(MAX_REPORTED_ERRORS) {
        parts.push(describe_source(error));
    }
    if errors.len() > MAX_REPORTED_ERRORS {
        let remaining = errors.len() - MAX_REPORTED_ERRORS;
        parts.push(format!("and {remaining} more"));
    }

    let detail = parts.join("; ");
    let message = if detail.is_empty() {
        "PDF export failed".to_owned()
    } else {
        format!("PDF export failed: {detail}")
    };
    Diagnostic::error(DiagCode::Typst, message)
}

/// One Typst PDF error as a single line: message plus its hints.
fn describe_source(error: &SourceDiagnostic) -> String {
    let mut line = error.message.to_string();
    for hint in &error.hints {
        line.push_str(" (hint: ");
        line.push_str(hint.v.as_str());
        line.push(')');
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PageSetup, Paper};

    /// Default rendering options: A4, 20 mm margin, nothing requested.
    fn options() -> RenderOptions {
        RenderOptions {
            page: PageSetup {
                paper: Paper::A4,
                margin_mm: 20.0,
                numbered: true,
            },
            toc: false,
            pdf_standard: None,
            fonts: Vec::new(),
            ignore_system_fonts: false,
            deterministic_today: None,
            title: None,
        }
    }

    #[test]
    fn title_becomes_the_document_ident() {
        let mut opts = options();
        opts.title = Some("mdx notes".to_owned());
        assert_eq!(ident(&opts), Smart::Custom("mdx notes".to_owned()));
    }

    #[test]
    fn missing_or_blank_title_keeps_the_auto_ident() {
        let mut opts = options();
        assert_eq!(ident(&opts), Smart::Auto);
        opts.title = Some("   ".to_owned());
        assert_eq!(ident(&opts), Smart::Auto);
    }

    #[test]
    fn no_standard_means_typst_defaults_and_supported_ones_are_accepted() {
        assert!(standards(&[]).is_ok());
        assert!(standards(&[PdfStandard::A_2b]).is_ok());
        assert!(standards(&[PdfStandard::Ua_1]).is_ok());
    }

    #[test]
    fn export_options_stay_deterministic_and_tagged() {
        let built = pdf_options(&options());
        assert!(built.is_ok(), "default options must always build");
        let Ok(built) = built else { return };
        assert!(built.ident.is_auto());
        assert!(built.timestamp.is_none());
        assert!(built.page_ranges.is_none());
        assert!(built.tagged);
        assert!(!built.pretty);
    }

    #[test]
    fn deterministic_clock_still_yields_no_timestamp() {
        let mut opts = options();
        opts.deterministic_today = Some("2026-01-01".to_owned());
        opts.pdf_standard = Some(PdfStandard::A_2b);
        let built = pdf_options(&opts);
        assert!(built.is_ok(), "PDF/A-2b must be an accepted standard");
        let Ok(built) = built else { return };
        assert!(built.timestamp.is_none());
    }

    #[test]
    fn failed_export_is_a_bounded_e_typst_diagnostic() {
        let diagnostic = export_failure(&[]);
        assert_eq!(diagnostic.code, DiagCode::Typst);
        assert!(diagnostic.message.contains("PDF export failed"));
    }
}

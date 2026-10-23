//! PDF export: a laid-out Typst document becomes the bytes of a PDF file.
//!
//! [`export`] is the single translation step between Typst's [`PagedDocument`]
//! and what `mdx render -o doc.pdf` writes: it maps the CLI-facing
//! [`RenderOptions`] onto [`PdfOptions`] and hands the document to Typst's PDF
//! writer.
//!
//! **Bookmarks:** typst-pdf builds the PDF outline from the document's
//! headings — Typst's PDF writer emits the `/Outlines` dictionary whenever
//! there is at least one — so every heading the writer emits turns into a
//! bookmark node and no extra option is needed to enable them.
//! // API: https://docs.rs/typst-pdf/0.15.1/typst_pdf/fn.pdf.html
//!
//! **Determinism:** no wall clock ever reaches the PDF. The creation date is
//! [`RenderOptions::deterministic_today`] — the same fixed date the Typst world
//! reports to the document as `today()` — or [`crate::world::DEFAULT_TODAY`]
//! when the caller pins none, so two exports of the same options are
//! byte-identical and PDF/A, which refuses a document without a date, can be
//! validated.
//!
//! // API: https://docs.rs/typst-pdf/0.15.1/typst_pdf/struct.PdfOptions.html
//!
//! **Verification:** [`has_outlines`], [`pdfa_part`] and [`page_count`] read the
//! bytes [`export`] produced. They are pure byte scans — nothing is parsed as a
//! PDF object, nothing is allocated, no input can make them panic — so a test
//! or a CI check can assert the structure of an exported document without
//! pulling in a PDF library.

use mdx_core::{DiagCode, Diagnostic};
use typst::diag::{HintedString, SourceDiagnostic};
use typst::foundations::{Datetime, Smart};
use typst_pdf::{PdfOptions, PdfStandard, PdfStandards, Timestamp};
// API: `PagedDocument` lives in the `typst-layout` crate as of Typst 0.15; the
// `typst` facade re-exports `typst-library` only, whose `layout` module carries
// the layout primitives but no document type.
// https://docs.rs/typst-layout/0.15.1/typst_layout/struct.PagedDocument.html
use typst_layout::PagedDocument;

use super::RenderOptions;
use super::world::DEFAULT_TODAY;

/// Maximum number of Typst errors folded into one [`Diagnostic`] message; the
/// remainder is reported as a count so a failing standard cannot produce a
/// message of unbounded length.
const MAX_REPORTED_ERRORS: usize = 5;

/// Writes `doc` to PDF bytes.
///
/// The PDF standard requested through [`RenderOptions::pdf_standard`] is passed
/// through verbatim; the document identifier is derived from
/// [`RenderOptions::title`] so repeated exports of the same document agree, and
/// the creation date is the deterministic date rather than the wall clock, so
/// two exports of the same options produce the same bytes.
///
/// Fails with [`DiagCode::Typst`] (`E_TYPST`) when Typst rejects the requested
/// standard or the PDF writer reports an error of its own (unsupported font,
/// failed standard validation, …), and with [`DiagCode::Config`] (`E_CONFIG`)
/// for a [`RenderOptions::deterministic_today`] that is not a calendar date,
/// the same rejection [`crate::compile`] performs.
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
        // The deterministic date, never the wall clock: PDF/A requires a
        // document date and repeated exports must stay byte-identical.
        timestamp: creation_timestamp(opts)?,
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

/// The creation date Typst records in the document metadata, as
/// [`PdfOptions::timestamp`].
///
/// The date is [`RenderOptions::deterministic_today`], or
/// [`crate::world::DEFAULT_TODAY`] when the caller pins none: the same fixed
/// date the Typst world reports to the document as `today()`, so the metadata
/// date and the document's own date cannot disagree, and two exports of the
/// same options stay byte-identical. PDF/A conformance, which a document
/// without any date fails, is therefore validated by Typst as well.
///
/// A pinned date that is not a real calendar date is a configuration error
/// ([`DiagCode::Config`], `E_CONFIG`, the wording [`crate::world`] uses);
/// [`crate::world::World::new`] rejects such a date first, so this only fires
/// when [`export`] is called with options that never went through
/// [`crate::compile`].
///
/// `Ok(None)` is unreachable for [`crate::world::DEFAULT_TODAY`] as shipped and
/// would mean that constant is not a real date; Typst then reports the missing
/// date itself instead of the export silently recording a different one.
fn creation_timestamp(opts: &RenderOptions) -> Result<Option<Timestamp>, Diagnostic> {
    let Some(text) = opts.deterministic_today.as_deref() else {
        let (year, month, day) = DEFAULT_TODAY;
        return Ok(Datetime::from_ymd(year, month, day).map(Timestamp::new_utc));
    };

    match parse_date(text) {
        Some(date) => Ok(Some(Timestamp::new_utc(date))),
        None => Err(invalid_date(text)),
    }
}

/// Parses `YYYY-MM-DD` into a date: three digit-only fields, no trailing
/// field, and a day that exists on the calendar (Typst's own calendar check
/// rejects e.g. `2026-02-30`).
///
/// Mirrors `world::parse_iso_date`, which is private to that module: the date
/// the document sees as `today()` and the date recorded in the PDF metadata
/// must be parsed from the same strings.
fn parse_date(text: &str) -> Option<Datetime> {
    let mut fields = text.split('-');
    let year: i32 = parse_field(fields.next()?, 4)?.parse().ok()?;
    let month: u8 = parse_field(fields.next()?, 2)?.parse().ok()?;
    let day: u8 = parse_field(fields.next()?, 2)?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Datetime::from_ymd(year, month, day)
}

/// An unsigned decimal field of at most `max_digits` ASCII digits.
fn parse_field(text: &str, max_digits: usize) -> Option<&str> {
    let digits =
        !text.is_empty() && text.len() <= max_digits && text.bytes().all(|b| b.is_ascii_digit());
    digits.then_some(text)
}

/// A pinned date that is not a calendar date, as one [`DiagCode::Config`]
/// diagnostic with the wording `world::parse_today` uses.
fn invalid_date(text: &str) -> Diagnostic {
    Diagnostic::error(
        DiagCode::Config,
        format!("invalid date {text:?}: expected a calendar date in YYYY-MM-DD form"),
    )
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

// ---------------------------------------------------------------------------
// Structural checks on the produced bytes
// ---------------------------------------------------------------------------

/// The XMP property naming the PDF/A part a document conforms to.
const PDFA_PART_KEY: &[u8] = b"pdfaid:part";

/// The dictionary key that names a page or a page-tree node.
const TYPE_KEY: &[u8] = b"/Type";

/// The type name of a page; `/Pages`, `/PageLabel` and other names that merely
/// start the same way are not pages.
const PAGE_TYPE: &[u8] = b"/Page";

/// The digit width of a `u8`; a longer run of digits cannot be a part number.
const MAX_PART_DIGITS: usize = 3;

/// Whether the PDF bytes carry a document outline — the bookmark pane the
/// viewer shows for the document's headings.
///
/// Typst's PDF writer writes the `/Outlines` dictionary only when the document
/// produced at least one heading, so this is a direct check that headings
/// reached the PDF as bookmarks. The bytes are searched, never parsed, so any
/// slice is accepted and no input can make this panic.
pub fn has_outlines(pdf: &[u8]) -> bool {
    contains(pdf, b"/Outlines", 0).is_some()
}

/// The PDF/A part the document declares in its XMP metadata: `Some(2)` for a
/// PDF/A-2b export, `Some(1)` for PDF/A-1b; `None` when the bytes declare no
/// part at all (an ordinary PDF, or one conforming to PDF/UA alone), or when
/// they carry no readable declaration.
///
/// Typst writes the XMP `/Metadata` stream uncompressed, so the property is
/// found in the raw bytes, in either the element form
/// (`<pdfaid:part>2</pdfaid:part>`) or the attribute form (`pdfaid:part="2"`).
/// Only an occurrence whose value is a number terminated like an XML value
/// counts, so the property name appearing in prose or in an extension-schema
/// description cannot fake a result.
pub fn pdfa_part(pdf: &[u8]) -> Option<u8> {
    let mut from = 0;
    while let Some(offset) = contains(pdf, PDFA_PART_KEY, from) {
        if let Some(part) = property_number(pdf, offset + PDFA_PART_KEY.len()) {
            return Some(part);
        }
        from = offset + PDFA_PART_KEY.len();
    }
    None
}

/// The number of page objects in the PDF bytes, or `None` when the bytes
/// contain none at all — not a PDF, a page tree still to be written, or a
/// document whose page dictionaries are compressed into object streams that a
/// byte scan cannot see.
///
/// A page is a dictionary with a `/Type` entry naming exactly `Page`; the
/// page-tree root (`/Type /Pages`) and `/Type /PageLabel` are not pages.
/// Whitespace between the two names is optional, which matters because Typst's
/// writer emits `/Type/Page` without one. The bytes are searched, never parsed,
/// so any slice is accepted and no input can make this panic.
pub fn page_count(pdf: &[u8]) -> Option<usize> {
    let mut count = 0usize;
    let mut from = 0usize;
    while let Some(offset) = contains(pdf, TYPE_KEY, from) {
        let after_key = offset + TYPE_KEY.len();
        // `after_key` lies inside the buffer, so the tail exists; whatever is
        // skipped before the name is whitespace.
        let tail = pdf.get(after_key..).unwrap_or_default();
        let name_start = after_key + (tail.len() - skip_space(tail).len());

        let named_page = pdf
            .get(name_start..)
            .and_then(|rest| rest.strip_prefix(PAGE_TYPE));
        if let Some(after_name) = named_page {
            // `/Pages`, `/PageLabel`, … continue the name and are not pages.
            let continues = after_name
                .first()
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
            if !continues {
                count += 1;
            }
        }

        from = after_key;
    }
    (count > 0).then_some(count)
}

/// The offset of `needle` in `haystack`, searching from `from` onwards, or
/// `None` when it does not occur. An empty needle never matches, so no caller
/// can reach the panic `slice::windows(0)` would raise.
fn contains(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| offset + from)
}

/// The number an XMP property carries, where the property name ends at
/// `after_name`: `>2<` for the element form, `="2"` for the attribute form.
/// Anything else yields `None`.
fn property_number(pdf: &[u8], after_name: usize) -> Option<u8> {
    match skip_space(pdf.get(after_name..)?).split_first() {
        Some((&b'>', value)) => number_before(value, b'<'),
        Some((&b'=', value)) => {
            let quoted = skip_space(value).strip_prefix(b"\"")?;
            number_before(quoted, b'"')
        }
        _ => None,
    }
}

/// The unsigned number starting `bytes`, whose digits must be followed by
/// `terminator` once whitespace is skipped: the end of an XML value.
fn number_before(bytes: &[u8], terminator: u8) -> Option<u8> {
    let bytes = skip_space(bytes);
    let digits = bytes
        .iter()
        .take(MAX_PART_DIGITS)
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    if skip_space(bytes.get(digits..)?).first() != Some(&terminator) {
        return None;
    }
    std::str::from_utf8(bytes.get(..digits)?).ok()?.parse().ok()
}

/// The whitespace PDF and XML recognise between tokens: space, tab, CR, LF,
/// form feed and NUL.
fn skip_space(bytes: &[u8]) -> &[u8] {
    let mut rest = bytes;
    while let Some((byte, tail)) = rest.split_first() {
        if !matches!(*byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0C | 0x00) {
            break;
        }
        rest = tail;
    }
    rest
}

#[cfg(test)]
mod tests {
    use typst::foundations::Datetime;

    use super::*;
    use crate::{PageSetup, Paper, world::DEFAULT_TODAY};

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
        assert!(built.timestamp.is_some(), "every export carries a date");
        assert!(built.page_ranges.is_none());
        assert!(built.tagged);
        assert!(!built.pretty);
    }

    #[test]
    fn export_records_a_fixed_creation_date_for_identical_options() {
        let first = pdf_options(&options());
        let second = pdf_options(&options());
        assert!(first.is_ok() && second.is_ok());
        let (Ok(first), Ok(second)) = (first, second) else {
            return;
        };
        // `Timestamp` has no equality of its own, so its `Debug` form is the
        // observable: two exports of the same options must agree exactly.
        assert_eq!(
            format!("{:?}", first.timestamp),
            format!("{:?}", second.timestamp)
        );
    }

    #[test]
    fn the_default_creation_date_is_the_world_clock_date() {
        let (year, month, day) = DEFAULT_TODAY;
        let default = creation_timestamp(&options());
        assert!(default.is_ok(), "the default date must resolve");
        let Ok(default) = default else { return };
        assert!(default.is_some(), "the default date must be recorded");
        let expected = format!("{year}-{month:02}-{day:02}");
        assert!(
            format!("{default:?}").contains(&expected),
            "{default:?} does not carry {expected}"
        );
    }

    #[test]
    fn a_pinned_date_replaces_the_default_and_a_broken_one_is_a_config_error() {
        let mut opts = options();
        opts.deterministic_today = Some("2030-05-06".to_owned());
        let pinned = creation_timestamp(&opts);
        assert!(pinned.is_ok(), "a calendar date must be accepted");
        let Ok(pinned) = pinned else { return };
        assert!(format!("{pinned:?}").contains("2030-05-06"), "{pinned:?}");

        for text in ["2026-02-30", "today", "", "2026-01-01T09:00", "2026-1"] {
            let mut opts = options();
            opts.deterministic_today = Some(text.to_owned());
            let rejected = creation_timestamp(&opts);
            assert!(rejected.is_err(), "{text:?} must be rejected");
            let Err(diagnostic) = rejected else { return };
            assert_eq!(diagnostic.code, DiagCode::Config);
            assert!(
                diagnostic.message.contains("invalid date"),
                "{}",
                diagnostic.message
            );
        }
    }

    #[test]
    fn dates_are_parsed_like_the_world_parses_them() {
        assert_eq!(parse_date("2026-01-01"), Datetime::from_ymd(2026, 1, 1));
        assert_eq!(parse_date("2026-1-1"), Datetime::from_ymd(2026, 1, 1));
        assert_eq!(parse_date("2030-05-06"), Datetime::from_ymd(2030, 5, 6));
        // Only the maximum width is enforced, exactly as the world's own
        // parser accepts the date the document then sees as `today()`.
        assert_eq!(parse_date("26-01-01"), Datetime::from_ymd(26, 1, 1));
        assert_eq!(parse_date("2026-02-30"), None);
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("2026-01-00"), None);
        assert_eq!(parse_date("2026-01"), None);
        assert_eq!(parse_date("2026-01-01-01"), None);
        assert_eq!(parse_date("-2026-01-01"), None);
        assert_eq!(parse_date("99999-01-01"), None);
        assert_eq!(parse_date("2026-01-0a"), None);
        assert_eq!(parse_date("today"), None);
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn a_standard_export_keeps_its_date_and_tagging() {
        let mut opts = options();
        opts.deterministic_today = Some("2026-01-01".to_owned());
        opts.pdf_standard = Some(PdfStandard::A_2b);
        let built = pdf_options(&opts);
        assert!(built.is_ok(), "PDF/A-2b must be an accepted standard");
        let Ok(built) = built else { return };
        assert!(
            built.timestamp.is_some(),
            "PDF/A rejects a date-less document"
        );
        assert!(built.tagged);
    }

    #[test]
    fn failed_export_is_a_bounded_e_typst_diagnostic() {
        let diagnostic = export_failure(&[]);
        assert_eq!(diagnostic.code, DiagCode::Typst);
        assert!(diagnostic.message.contains("PDF export failed"));
    }

    /// A two-page document with a two-entry outline and a page-label
    /// dictionary, in the shape Typst's writer emits: compact dictionaries,
    /// `/Type/Page` without a space, the outline referenced from the catalog,
    /// no XMP PDF/A declaration.
    const SYNTHETIC: &[u8] = b"%PDF-1.7\n\
1 0 obj\n<</Type/Pages/Count 2/Kids[2 0 R 3 0 R]>>\nendobj\n\
2 0 obj\n<</Type/Page/Parent 1 0 R/MediaBox[0 0 595.28 841.89]>>\nendobj\n\
3 0 obj\n<</Type /Page /Parent 1 0 R>>\nendobj\n\
4 0 obj\n<</Type/PageLabel/S/D/St 1>>\nendobj\n\
5 0 obj\n<</Type/Outlines/First 6 0 R/Last 6 0 R/Count 1>>\nendobj\n\
6 0 obj\n<</Title(Intro)/Parent 5 0 R/Dest[2 0 R/XYZ 0 841.89 0]>>\nendobj\n\
7 0 obj\n<</Type/Catalog/Pages 1 0 R/Outlines 5 0 R>>\nendobj\n";

    #[test]
    fn structural_checks_read_one_document_together() {
        assert_eq!(page_count(SYNTHETIC), Some(2));
        assert!(has_outlines(SYNTHETIC));
        assert_eq!(pdfa_part(SYNTHETIC), None);
    }

    #[test]
    fn page_count_reads_pages_and_ignores_the_page_tree() {
        assert_eq!(page_count(b"<</Type/Page/MediaBox[0 0 612 792]>>"), Some(1));
        // The page-tree root, the page-label dictionary and longer names that
        // only start with `/Page` are not pages.
        assert_eq!(page_count(b"<</Type/Pages/Count 3/Kids[]>>"), None);
        assert_eq!(page_count(b"<</Type/PageLabel/S/D/St 1>>"), None);
    }

    #[test]
    fn page_count_accepts_optional_whitespace_and_never_panics() {
        assert_eq!(page_count(b"/Type /Page"), Some(1));
        assert_eq!(page_count(b"/Type\r\n/Page"), Some(1));
        assert_eq!(page_count(b"/Type/Page/Type /Page"), Some(2));

        for garbage in [
            &b""[..],
            b"%PDF-1.4\n",
            b"%%%%EOF",
            b"/Type",
            b"/Type/",
            b"/Type /Pag",
            b"/TypeX/Page",
            b"/Types/Page",
            b"/Pages",
            &[0xFF; 64][..],
        ] {
            assert_eq!(page_count(garbage), None, "{garbage:?}");
        }
    }

    #[test]
    fn has_outlines_follows_the_outline_dictionary() {
        assert!(has_outlines(b"<</Type/Outlines/Count 0>>"));
        assert!(has_outlines(b"<</Type/Catalog/Outlines 5 0 R>>"));
        assert!(!has_outlines(b"<</Type/Catalog/Pages 1 0 R>>"));

        for garbage in [&b""[..], b"%PDF-1.7\n%%EOF", b"/Outline", &[0x00; 32][..]] {
            assert!(!has_outlines(garbage), "{garbage:?}");
        }
    }

    #[test]
    fn pdfa_part_reads_the_xmp_property() {
        assert_eq!(pdfa_part(b"<pdfaid:part>1</pdfaid:part>"), Some(1));
        assert_eq!(pdfa_part(b"<pdfaid:part>2</pdfaid:part>"), Some(2));
        assert_eq!(pdfa_part(b"<pdfaid:part>4</pdfaid:part>"), Some(4));
        assert_eq!(pdfa_part(b"<rdf:Description pdfaid:part=\"3\"/>"), Some(3));
        assert_eq!(pdfa_part(b"<pdfaid:part>\n 2 </pdfaid:part>"), Some(2));
        assert_eq!(pdfa_part(b"pdfaid:part = \"2\""), Some(2));
        // The first occurrence carrying a value wins.
        assert_eq!(
            pdfa_part(b"<x>pdfaid:part</x><pdfaid:part>2</pdfaid:part>"),
            Some(2)
        );
    }

    #[test]
    fn pdfa_part_ignores_mentions_and_broken_values() {
        assert_eq!(
            pdfa_part(b"<pdfaExtension:name>pdfaid:part</pdfaExtension:name>"),
            None
        );
        assert_eq!(
            pdfa_part(b"<dc:description>pdfaid:part 2 of ISO 19005</dc:description>"),
            None
        );
        assert_eq!(pdfa_part(b"<pdfaid:part>two</pdfaid:part>"), None);
        assert_eq!(pdfa_part(b"<pdfaid:part>300</pdfaid:part>"), None);
        assert_eq!(pdfa_part(b"<pdfaid:part></pdfaid:part>"), None);
        assert_eq!(pdfa_part(b"<pdfaid:part>2"), None);
        assert_eq!(pdfa_part(b"<pdfuaid:part>1</pdfuaid:part>"), None);

        for garbage in [
            &b""[..],
            b"%PDF-1.7\n",
            b"pdfaid:part",
            b"pdfaid:part=",
            b"pdfaid:part=\"",
            &[0xFF; 16][..],
        ] {
            assert_eq!(pdfa_part(garbage), None, "{garbage:?}");
        }
    }
}

//! Page geometry: paper sizes and margins, expressed in points.
//!
//! Typst lays pages out in *points* (`typst::layout::Abs`, 1 pt = 1/72 in) while
//! [`super::PageSetup`] stores the margin in millimetres, the unit a user
//! writes (no CLI flag exposes it yet). These helpers do the conversion once so
//! the world and writer code never spell out the factor.
//!
//! All three functions are pure: no globals, no clock, no file system, so
//! identical inputs always produce bit-identical output.

use std::fmt::Write as _;

use super::{Paper, RenderOptions};

/// Points per millimetre: 1 in = 25.4 mm = 72 pt.
const PT_PER_MM: f32 = 72.0 / 25.4;

/// The size of `paper` in points as `(width, height)`.
///
/// Values are the standard definitions of the two supported papers:
///
/// | Paper  | Native size    | Points           |
/// |--------|----------------|------------------|
/// | A4     | 210 mm × 297 mm (ISO 216) | 595.28 × 841.89 |
/// | Letter | 8.5 in × 11 in (ANSI/ASME Y14.1) | 612 × 792 |
///
/// // API: https://typst.app/docs/reference/layout/page/#parameters-paper
pub fn page_size(paper: &Paper) -> (f32, f32) {
    match paper {
        Paper::A4 => (595.28, 841.89),
        Paper::Letter => (612.0, 792.0),
    }
}

/// Converts a uniform `mm` millimetre margin into points, ordered
/// `(top, right, bottom, left)` — the order Typst's `margin` parameter uses for
/// its four-element dictionary/list form.
///
/// The value is passed through as given; validating the margin (for example
/// rejecting negative or oversized values) belongs to the caller that owns the
/// configuration, and Typst clips a margin larger than the page itself.
///
/// // API: https://typst.app/docs/reference/layout/page/#parameters-margin
pub fn margins(mm: f32) -> (f32, f32, f32, f32) {
    let points = mm * PT_PER_MM;
    (points, points, points, points)
}

/// The Typst page-numbering pattern used when [`PageSetup::numbered`] is set:
/// the first level of the page counter as a plain decimal number, i.e. `1`,
/// `2`, `3`, … in the running footer.
///
/// // API: https://typst.app/docs/reference/layout/page/#parameters-numbering
const NUMBERING_PATTERN: &str = "1";

/// The `#set page(…)` rule the generated Typst document opens with, as one
/// line: paper, margin and — when the page setup asks for it — the page
/// numbering pattern.
///
/// ```
/// # use mdx_typst::{PageSetup, Paper, RenderOptions};
/// # use mdx_typst::page::page_source_rules;
/// # let opts = RenderOptions {
/// #     page: PageSetup { paper: Paper::A4, margin_mm: 20.0, numbered: true },
/// #     toc: false, pdf_standard: None, fonts: Vec::new(),
/// #     ignore_system_fonts: false, deterministic_today: None, title: None,
/// # };
/// assert_eq!(
///     page_source_rules(&opts),
///     "#set page(paper: \"a4\", margin: 20mm, numbering: \"1\")",
/// );
/// ```
///
/// This is the page-numbering contract: the returned string is byte-for-byte
/// what [`crate::writer::build`] emits for the same [`RenderOptions`] — the
/// writer currently assembles the rule inline — and it is the canonical
/// statement of the numbering pattern, so the tests in this module compare the
/// two and fail if either side drifts. Margins are formatted with the writer's
/// rule: negative, non-finite and `-0` values collapse to `0mm`, so the emitted
/// length is always well formed and never `NaNmm`.
pub fn page_source_rules(opts: &RenderOptions) -> String {
    let mut rules = String::with_capacity(PAGE_RULE_CAPACITY);
    rules.push_str("#set page(paper: \"");
    rules.push_str(paper_name(opts.page.paper));
    rules.push_str("\", margin: ");
    push_mm(&mut rules, opts.page.margin_mm);
    if opts.page.numbered {
        rules.push_str(", numbering: \"");
        rules.push_str(NUMBERING_PATTERN);
        rules.push('"');
    }
    rules.push(')');
    rules
}

/// Pre-sized for the rules this module usually emits (`#set page(paper:
/// "us-letter", margin: 25.4mm, numbering: "1")`), so the common case fills one
/// allocation; a longer margin length simply grows the string once more.
const PAGE_RULE_CAPACITY: usize = 64;

/// Typst paper name for a [`Paper`] variant, as the generated document spells
/// it. Kept equal to `writer::paper_name`; the tests assert the agreement.
/// // API: https://typst.app/docs/reference/layout/page/#parameters-paper
fn paper_name(paper: Paper) -> &'static str {
    match paper {
        Paper::A4 => "a4",
        Paper::Letter => "us-letter",
    }
}

/// Appends a millimetre length, normalising exactly like the writer: negative,
/// non-finite and `-0` margins collapse to `0mm`.
fn push_mm(out: &mut String, mm: f32) {
    let mm = if mm.is_finite() && mm > 0.0 { mm } else { 0.0 };
    let _ = write!(out, "{mm}mm");
}

#[cfg(test)]
mod tests {
    use mdx_assets::AssetStore;
    use mdx_core::Doc;

    use super::{PT_PER_MM, Paper, margins, page_size, page_source_rules};
    use crate::{DiagramMap, MathMap, PageSetup, RenderOptions};

    /// Float comparison tolerance in points: 0.01 pt is well below one device
    /// pixel at any sane resolution, yet above `f32` rounding noise.
    const EPS: f32 = 0.01;

    /// Rendering options for one page set-up, everything else at its default.
    fn options(paper: Paper, margin_mm: f32, numbered: bool) -> RenderOptions {
        RenderOptions {
            page: PageSetup {
                paper,
                margin_mm,
                numbered,
            },
            toc: false,
            pdf_standard: None,
            fonts: Vec::new(),
            ignore_system_fonts: false,
            deterministic_today: None,
            title: None,
        }
    }

    /// The `#set page(…)` line `writer::build` itself puts into the generated
    /// source for `opts` — the fragment [`page_source_rules`] is a contract
    /// for. An empty string when the writer emits no page rule at all, which
    /// makes every caller's comparison fail rather than pass vacuously.
    fn writer_page_rule(opts: &RenderOptions) -> String {
        let source = crate::writer::build(
            &Doc::new(Vec::new()),
            &AssetStore::new(),
            &MathMap::new(),
            &DiagramMap::new(),
            opts,
        );
        source
            .lines()
            .find(|line| line.starts_with("#set page("))
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn a4_is_iso_216_in_points() {
        assert_eq!(page_size(&Paper::A4), (595.28, 841.89));
    }

    #[test]
    fn letter_is_ansi_letter_in_points() {
        assert_eq!(page_size(&Paper::Letter), (612.0, 792.0));
    }

    #[test]
    fn a4_agrees_with_its_millimetre_size() {
        let (width, height) = page_size(&Paper::A4);
        assert!((width - 210.0 * PT_PER_MM).abs() < EPS);
        assert!((height - 297.0 * PT_PER_MM).abs() < EPS);
    }

    #[test]
    fn letter_agrees_with_its_inch_size() {
        let (width, height) = page_size(&Paper::Letter);
        assert!((width - 8.5 * 72.0).abs() < EPS);
        assert!((height - 11.0 * 72.0).abs() < EPS);
    }

    #[test]
    fn margins_map_millimetres_to_points() {
        // 25.4 mm is exactly one inch, i.e. exactly 72 pt.
        let (top, right, bottom, left) = margins(25.4);
        assert!((top - 72.0).abs() < 1e-3);
        assert_eq!(top, right);
        assert_eq!(right, bottom);
        assert_eq!(bottom, left);
    }

    #[test]
    fn zero_margin_stays_zero() {
        assert_eq!(margins(0.0), (0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn margins_scale_linearly() {
        let (single, ..) = margins(5.0);
        let (double, ..) = margins(10.0);
        assert!((double - 2.0 * single).abs() < 1e-3);
        assert!(single > 0.0);
    }

    #[test]
    fn page_source_rules_match_the_writer_for_every_setup() {
        let setups = [
            (Paper::A4, 20.0, true),
            (Paper::Letter, 12.5, false),
            (Paper::A4, 0.0, true),
            (Paper::Letter, 25.4, true),
            (Paper::A4, -3.0, true),
            (Paper::Letter, f32::INFINITY, false),
            (Paper::A4, f32::NAN, true),
        ];
        for (paper, margin_mm, numbered) in setups {
            let opts = options(paper, margin_mm, numbered);
            assert_eq!(
                page_source_rules(&opts),
                writer_page_rule(&opts),
                "page rule drifted for {paper:?}, {margin_mm}mm, numbered = {numbered}",
            );
        }
    }

    #[test]
    fn numbering_is_declared_exactly_when_pages_are_numbered() {
        let numbered = page_source_rules(&options(Paper::A4, 20.0, true));
        assert_eq!(
            numbered,
            "#set page(paper: \"a4\", margin: 20mm, numbering: \"1\")"
        );

        let plain = page_source_rules(&options(Paper::A4, 20.0, false));
        assert_eq!(plain, "#set page(paper: \"a4\", margin: 20mm)");
        assert!(!plain.contains("numbering"));
    }

    #[test]
    fn paper_and_margin_use_the_writers_spelling() {
        let letter = page_source_rules(&options(Paper::Letter, 25.4, true));
        assert!(letter.contains("paper: \"us-letter\""), "{letter}");
        assert!(letter.contains("margin: 25.4mm"), "{letter}");

        // Negative, non-finite and zero margins all render as a length.
        for margin_mm in [-5.0, f32::NEG_INFINITY, f32::NAN, -0.0] {
            let rule = page_source_rules(&options(Paper::A4, margin_mm, true));
            assert!(rule.contains("margin: 0mm"), "{margin_mm} became {rule}");
        }
    }

    #[test]
    fn page_source_rules_are_identical_across_calls() {
        let opts = options(Paper::Letter, 12.5, true);
        assert_eq!(page_source_rules(&opts), page_source_rules(&opts));
    }
}

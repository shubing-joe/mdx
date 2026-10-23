//! Page geometry: paper sizes and margins, expressed in points.
//!
//! Typst lays pages out in *points* (`typst::layout::Abs`, 1 pt = 1/72 in) while
//! [`super::PageSetup`] keeps the user-facing margin in millimetres, the unit
//! `mdx render --margin` speaks. These helpers do the conversion once so the
//! world and writer code never spell out the factor.
//!
//! Both functions are pure: no globals, no clock, no allocation, so identical
//! inputs always produce bit-identical output.

use super::Paper;

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

#[cfg(test)]
mod tests {
    use super::{PT_PER_MM, Paper, margins, page_size};

    /// Float comparison tolerance in points: 0.01 pt is well below one device
    /// pixel at any sane resolution, yet above `f32` rounding noise.
    const EPS: f32 = 0.01;

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
}

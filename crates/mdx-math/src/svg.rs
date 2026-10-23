//! RaTeX fallback: LaTeX math → standalone SVG.
//!
//! [`latex_to_svg`] is the second math backend of the crate. Where
//! [`crate::typst::latex_to_typst`] converts a formula into Typst math source
//! for the PDF/HTML pipeline, this module renders the *same* raw LaTeX straight
//! to a standalone SVG document, so a writer that cannot express a formula in
//! Typst still has a faithful picture to embed. The pipeline is:
//!
//! 1. `ratex_parser::parse` — LaTeX → parser AST (one node per top-level atom).
//! 2. `ratex_layout::layout` — AST → layout box tree (display style, black).
//! 3. `ratex_layout::validate_layout` — reject layouts whose drawing-item cost
//!    exceeds the engine budget before any geometry is materialised.
//! 4. `ratex_layout::to_display_list` + `ratex_svg::render_to_svg` — layout tree
//!    → SVG document string.
//!
//! Input is the raw LaTeX a caller got from `mdx-parse`; `$`/`$$`, `\(…\)` and
//! `\[…\]` fences are tolerated and stripped when they wrap the whole formula
//! (the parser normally removes them, but the MCP/CLI callers may pass a
//! fenced snippet verbatim).
//!
//! # Host CSS requirement
//!
//! This crate depends on `ratex-svg` with its default feature set, so glyphs are
//! emitted as `<text>` elements whose `font-family` are the KaTeX CSS font
//! names (`KaTeX_Main`, `KaTeX_Math`, `KaTeX_Size1`, …). The returned document is
//! therefore **not** self-contained: a host embedding it (HTML writer, GUI
//! preview) must load the KaTeX stylesheets and webfonts, exactly like KaTeX
//! itself. `ratex-svg`'s `standalone`/`embed-fonts` features would instead
//! outline or inline every glyph; enabling them is a deliberate workspace-level
//! decision (they pull the embedded KaTeX TTF set into the binary), so this
//! slice documents the requirement rather than forcing it on every build.
//!
//! Layout geometry (`viewBox`, `width`/`height` in pt) is computed at 40 user
//! units per em with 10 units of padding, matching `ratex-render` at DPR 1.
//!
//! # Failure modes
//!
//! The function returns `Err` — it never panics and never returns a partial
//! document — for
//!
//! * empty or whitespace-only input (including a bare `$`/`$$` fence pair),
//! * LaTeX the parser rejects (undefined control sequences, unbalanced groups,
//!   double superscripts, unmatched `\left`/`\right`, missing arguments, …),
//! * formulas whose layout exceeds the engine's drawing-item budget, and
//! * a panic inside the third-party RaTeX engine: the call is guarded with
//!   [`std::panic::catch_unwind`] because `ratex-parser` contains
//!   `unreachable!()` in its symbol-group dispatch and internal `unwrap()`s, and
//!   a malformed document must never take the whole render pipeline down. Such
//!   a caught panic is mapped to a plain `Err` (only the default panic hook's
//!   diagnostic still reaches stderr).
//!
//! Output is deterministic: no clock, no randomness, fixed options.

use ratex_layout::{LayoutOptions, layout, to_display_list, validate_layout};
use ratex_parser::parse;
use ratex_svg::{SvgOptions, render_to_svg};

/// User units per em for the emitted viewport (RaTeX default; matches
/// `ratex_render::RenderOptions::font_size` at DPR 1).
const FONT_SIZE: f64 = 40.0;

/// Padding on all four sides of the emitted viewport, in user units.
const PADDING: f64 = 10.0;

/// Stroke width for unfilled paths (fraction bars, radicals, delimiters), in
/// user units.
const STROKE_WIDTH: f64 = 1.5;

/// Render a LaTeX math expression to a standalone SVG document.
///
/// `src` may be a bare expression (`\frac{a}{b}`) or a fenced one
/// (`$$\frac{a}{b}$$`, `\[ … \]`, `\( … \)`); the fence is stripped when it wraps
/// the whole input. On success the returned string starts with `<svg` and ends
/// with `</svg>` — see the module docs for the KaTeX CSS requirement and the
/// full list of failure modes.
///
/// # Errors
///
/// Returns a short human-readable reason (never an empty string) when the input
/// is empty after fence stripping, when RaTeX rejects it, or when the layout
/// exceeds the engine's output budget.
pub fn latex_to_svg(src: &str) -> Result<String, String> {
    let body = strip_fences(src);
    if body.is_empty() {
        return Err("empty LaTeX input: nothing to render".to_string());
    }

    // Third-party code inside `render` contains `unreachable!()`/`unwrap()` on
    // paths this crate does not control; a malformed document must surface as an
    // `Err`, never as a process abort or a caught panic. `&str` is `UnwindSafe`,
    // so no `AssertUnwindSafe` is needed.
    let svg = std::panic::catch_unwind(|| render(body)).map_err(|_| {
        format!(
            "RaTeX panicked while rendering LaTeX input: {}",
            snippet(body)
        )
    })??;

    // Guard the documented post-condition even if the engine ever changes.
    if !svg.starts_with("<svg") {
        return Err(format!(
            "RaTeX produced a non-SVG document for LaTeX input: {}",
            snippet(body)
        ));
    }
    Ok(svg)
}

/// Strip one layer of surrounding math fences and surrounding whitespace.
///
/// Returns an empty string when the input carries no formula, so the caller can
/// reject it in one place.
fn strip_fences(src: &str) -> &str {
    let trimmed = src.trim();
    // Order matters: `$$` must be tried before `$`, otherwise `$$x$$` would
    // strip only one dollar on each side and leave `$x$` behind.
    const FENCES: [(&str, &str); 4] = [("$$", "$$"), ("\\[", "\\]"), ("\\(", "\\)"), ("$", "$")];
    for (open, close) in FENCES {
        // A bare delimiter pair carries no formula: report it as empty so the
        // caller rejects it instead of handing `$`/`\[` to the parser.
        if trimmed == open || trimmed == close {
            return "";
        }
        if let Some(rest) = trimmed.strip_prefix(open)
            && let Some(inner) = rest.strip_suffix(close)
        {
            return inner.trim();
        }
    }
    trimmed
}

/// Parse, lay out, validate and serialise one unfenced LaTeX expression.
fn render(body: &str) -> Result<String, String> {
    let ast = parse(body).map_err(|e| format!("LaTeX parse error: {e}"))?;
    if ast.is_empty() {
        return Err(format!(
            "LaTeX input produced no expressions: {}",
            snippet(body)
        ));
    }

    // API: https://docs.rs/ratex-layout/0.1.15/ratex_layout/struct.LayoutOptions.html
    // `LayoutOptions::default()` is display style, black — the documented,
    // deterministic baseline for the fallback path.
    let options = LayoutOptions::default();
    let tree = layout(&ast, &options);
    validate_layout(&tree)
        .map_err(|e| format!("LaTeX layout rejected for {}: {e}", snippet(body)))?;

    let display_list = to_display_list(&tree);
    // API: https://docs.rs/ratex-svg/0.1.15/ratex_svg/struct.SvgOptions.html
    let svg_options = SvgOptions {
        font_size: FONT_SIZE,
        padding: PADDING,
        stroke_width: STROKE_WIDTH,
        embed_glyphs: false,
        font_dir: String::new(),
    };
    Ok(render_to_svg(&display_list, &svg_options))
}

/// First 48 characters of `text`, for error messages. `text` is already trimmed.
fn snippet(text: &str) -> String {
    const LIMIT: usize = 48;
    let mut out = String::with_capacity(text.len().min(LIMIT) + 3);
    for (idx, ch) in text.char_indices() {
        if idx >= LIMIT {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn empty_and_whitespace_input_is_an_error() {
        for src in ["", "   ", "\n\t ", "$$ $$", "$$$$", "$"] {
            let err = latex_to_svg(src).expect_err("expected an error");
            assert!(!err.is_empty(), "empty error message for {src:?}");
        }
    }

    #[test]
    fn garbage_input_is_an_error_not_a_panic() {
        for src in [
            "\\nonexistentcommand{x}",
            "{x",
            "x^2^3",
            "\\frac{a}",
            "\\right)",
            "\\left( x",
            "\\begin{foobar} a \\end{foobar}",
        ] {
            match latex_to_svg(src) {
                Ok(svg) => panic!("expected an error for {src:?}, got {svg}"),
                Err(msg) => assert!(!msg.is_empty(), "empty error message for {src:?}"),
            }
        }
    }

    #[test]
    fn simple_formula_renders_a_standalone_svg() {
        let svg = latex_to_svg("x^2").expect("x^2 should render");
        assert!(svg.starts_with("<svg "), "{svg}");
        assert!(svg.ends_with("</svg>"), "{svg}");
    }

    #[test]
    fn fences_are_stripped() {
        let bare = latex_to_svg("\\frac{a}{b}").expect("bare formula should render");
        for fenced in [
            "$\\frac{a}{b}$",
            "$$\\frac{a}{b}$$",
            "\\(\\frac{a}{b}\\)",
            "\\[\\frac{a}{b}\\]",
            "  $$\\frac{a}{b}$$  ",
        ] {
            assert_eq!(
                latex_to_svg(fenced).expect("fenced formula should render"),
                bare,
                "{fenced}"
            );
        }
    }

    /// The corpus in `fixtures/math` is the integration surface: every
    /// `NNN-*.tex` fixture must render, and every `9xx-bad-*.tex` fixture must
    /// fail cleanly (an `Err`, never a panic).
    #[test]
    fn fixture_corpus_renders_and_rejects_as_documented() {
        let dir = corpus_dir();
        let mut checked = 0usize;
        let mut bad = 0usize;
        for path in tex_files(&dir) {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
                .unwrap_or_default();
            let src = fs::read_to_string(&path).expect("fixture must be readable UTF-8");
            match latex_to_svg(&src) {
                Ok(svg) => {
                    assert!(
                        !name.contains("-bad-"),
                        "bad fixture {name} unexpectedly rendered: {svg}"
                    );
                    assert!(svg.starts_with("<svg "), "{name}: {svg}");
                    checked += 1;
                }
                Err(msg) => {
                    assert!(name.contains("-bad-"), "good fixture {name} failed: {msg}");
                    assert!(!msg.is_empty(), "{name}: empty error message");
                    bad += 1;
                }
            }
        }
        assert_eq!(checked, 85, "expected 85 valid fixtures in {dir:?}");
        assert_eq!(bad, 10, "expected 10 malformed fixtures in {dir:?}");
    }

    fn corpus_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/math")
    }

    fn tex_files(dir: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .expect("fixtures/math must exist")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|ext| ext == std::ffi::OsStr::new("tex"))
            })
            .collect();
        paths.sort();
        paths
    }
}

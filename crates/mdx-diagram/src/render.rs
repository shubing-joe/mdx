//! Mermaid source to resvg-safe SVG.
//!
//! [`render_svg`] is this crate's rendering entry point. It drives merman's
//! typed SVG request through the resvg-safe pipeline, so the returned document
//! never contains `<foreignObject>` HTML labels and can be parsed by resvg/usvg
//! (both the self-contained HTML output and the Typst/PDF path depend on that),
//! and it pins every input that could otherwise leak in from the host
//! environment:
//!
//! * an operation control without a deadline, so no clock reaches the output;
//! * an explicit diagram id derived from the diagram source, so two diagrams in
//!   one document cannot share generated marker and edge ids;
//! * text metrics that never come from fonts installed on the machine.
//!
//! # Text metrics
//!
//! Mermaid sizes every label from measured text, so the metric source decides
//! the rendered geometry. Two sources are supported here:
//!
//! * *Deterministic* (the default): merman's built-in width model, which depends
//!   on the label text and font size only.
//! * *Font files*: when [`DiagramOptions::font_files`] is non-empty, widths are
//!   summed from the `hmtx` advances of those faces, scaled by `head.unitsPerEm`,
//!   and a label's CSS family stack selects the face it is measured with.
//!
//! merman 0.9.2 ships a shaping-based native measurement backend
//! (`SvgEnvironment::with_native_font_context`), but it is compiled only with
//! merman's `native-fonts` feature, which this workspace deliberately leaves off
//! because it would pull a rasterization/export dependency closure into every
//! build. The supported alternative is merman's host width callback, which is
//! what the font-file path uses, so no dependency outside this crate's manifest
//! is needed. Kerning, ligatures, and mark placement are consequently not
//! applied; the private `fonts` module documents the exact model.
//!
//! # Errors
//!
//! Every failure is a `String` with a stable prefix, so callers can map it onto
//! a diagnostic code without parsing prose. The prefixes are the diagram codes of
//! `mdx_core::DiagCode`:
//!
//! * `E_DIAGRAM_PARSE: …` — empty, malformed, or unsupported diagram source.
//! * `E_DIAGRAM_RENDER: …` — the diagram was understood, but rendering could not
//!   use the requested font files or produced output that violates the SVG
//!   contract above.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use merman::svg::{
    DeterministicTextMeasurer, MeasurementProfileId, SvgPipeline, SvgRenderOptions,
    TextMeasurementPolicy, TextMeasurementProfile, TextMeasurementProfileIdentity,
};
use merman::{OperationControl, RenderOutput, RenderRequest, Renderer, SvgEnvironment, SvgRequest};

use crate::cache;

/// Error prefix for diagram source failures.
const PARSE_CODE: &str = "E_DIAGRAM_PARSE";
/// Error prefix for failures that are not the source's fault: an unusable font
/// file, or output that violates the SVG contract.
const RENDER_CODE: &str = "E_DIAGRAM_RENDER";

/// Measurement profile name for the font-file metric source.
const FONT_PROFILE: &str = "mdx-diagram.font-file-advances";
/// Measurement profile version: the crate that produced the metric model.
const FONT_PROFILE_VERSION: &str = concat!("mdx-diagram@", env!("CARGO_PKG_VERSION"));

/// Options for one Mermaid render.
///
/// [`DiagramOptions::default`] renders with deterministic text metrics and no
/// explicit fonts, which is the configuration the HTML and PDF paths rely on:
/// identical input produces byte-identical SVG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagramOptions {
    /// Keep text metrics independent of the host environment (default `true`).
    ///
    /// Mermaid sizes labels from measured text widths, so metrics decide the
    /// rendered geometry. With `true`, measurement comes from merman's
    /// deterministic model and is identical on every machine.
    ///
    /// This build has no ambient font backend — merman's `native-fonts` and
    /// `system-fonts` features are not enabled — so `false` selects nothing on
    /// its own: explicit [`Self::font_files`] are measured from those files
    /// either way, and without them the deterministic model remains the only
    /// available source.
    pub deterministic_text: bool,
    /// Font files used to measure label text, in preference order.
    ///
    /// A non-empty list replaces the deterministic model with the advances found
    /// in these faces, and each label's CSS font stack is matched against the
    /// family names stored inside them. Every listed file must be a readable sfnt
    /// (TrueType/OpenType) font, otherwise rendering fails with
    /// `E_DIAGRAM_RENDER`.
    pub font_files: Vec<PathBuf>,
}

impl Default for DiagramOptions {
    fn default() -> Self {
        Self {
            deterministic_text: true,
            font_files: Vec::new(),
        }
    }
}

/// Renders one Mermaid diagram as a standalone, resvg-safe SVG document.
///
/// The source is trimmed before rendering, so surrounding blank lines are
/// irrelevant. The returned document starts with `<svg`, contains no
/// `<foreignObject>` HTML label, and is stable: the same source and options
/// always produce the same bytes, element ids included.
///
/// # Errors
///
/// * `E_DIAGRAM_PARSE: …` when the source is empty, is not a Mermaid diagram, or
///   uses a diagram type this build cannot render.
/// * `E_DIAGRAM_RENDER: …` when a file in [`DiagramOptions::font_files`] cannot
///   be read or is not an sfnt font, or when the renderer returned output that
///   violates the SVG contract above.
///
/// ```no_run
/// # fn main() -> Result<(), String> {
/// let options = mdx_diagram::DiagramOptions::default();
/// let svg = mdx_diagram::render_svg("flowchart TD\nA[Start] --> B[Done]", &options)?;
/// assert!(svg.starts_with("<svg"));
/// # Ok(())
/// # }
/// ```
pub fn render_svg(src: &str, opts: &DiagramOptions) -> Result<String, String> {
    let source = src.trim();
    if source.is_empty() {
        return Err(error(PARSE_CODE, "empty Mermaid source"));
    }

    let environment = build_environment(opts)?;
    let request = svg_request(environment, source);
    let output = Renderer::new()
        .render(RenderRequest::svg(source, OperationControl::new(), request))
        .map_err(|failure| error(PARSE_CODE, &failure.to_string()))?;

    let RenderOutput::Svg(rendered) = output else {
        return Err(error(
            PARSE_CODE,
            "renderer did not return SVG output for an SVG request",
        ));
    };
    let Some(rendered) = rendered else {
        return Err(error(PARSE_CODE, "no Mermaid diagram detected in source"));
    };

    let svg = rendered.into_parts().0;
    validate_output(&svg)?;
    Ok(svg)
}

/// Formats a failure with its stable code prefix.
fn error(code: &str, message: &str) -> String {
    format!("{code}: {message}")
}

/// Builds the SVG service set for one render.
///
/// Without font files this is merman's deterministic service set: a headless,
/// resource-bounded session whose text measurement depends on neither installed
/// fonts nor the clock.
fn build_environment(options: &DiagramOptions) -> Result<SvgEnvironment, String> {
    // API: merman 0.9.2 `SvgEnvironment::deterministic()`
    // (https://docs.rs/merman/0.9.2/merman/struct.SvgEnvironment.html#method.deterministic)
    // selects `TextMeasurementPolicy::deterministic()` plus the default render
    // resource policy. `DiagramOptions::deterministic_text` never *disables* it,
    // because no ambient metric source is compiled into this build; explicit font
    // files are the only way to select measured metrics.
    if options.font_files.is_empty() {
        return Ok(SvgEnvironment::deterministic());
    }

    let fonts = font_set(&options.font_files)?;

    // API: merman 0.9.2 host width callback
    // (https://docs.rs/merman/0.9.2/merman/svg/struct.DeterministicTextMeasurer.html#method.with_width_callback).
    // `with_width_callback` returns an `Arc<dyn TextMeasurer + Send + Sync>` that
    // keeps merman's wrapping, line height, and whitespace policy and takes only
    // whole-candidate widths from the host, which must be finite and non-negative.
    let measurer = DeterministicTextMeasurer::default()
        .with_width_callback(move |text, style| fonts.width_px(text, style));

    let profile = MeasurementProfileId::new(FONT_PROFILE)
        .map_err(|failure| error(RENDER_CODE, &failure.to_string()))?;
    let identity = TextMeasurementProfileIdentity::new(profile, FONT_PROFILE_VERSION)
        .and_then(|identity| identity.with_decorators(font_decorators(&options.font_files)))
        .map_err(|failure| error(RENDER_CODE, &failure.to_string()))?;

    // API: merman 0.9.2 `TextMeasurementPolicy::uniform`
    // (https://docs.rs/merman/0.9.2/merman/svg/struct.TextMeasurementPolicy.html#method.uniform)
    // routes every measurement phase through the same profile, so layout and
    // emitted label boxes agree.
    let policy = TextMeasurementPolicy::uniform(TextMeasurementProfile::new(identity, measurer));
    Ok(SvgEnvironment::deterministic().with_text_measurement_policy(policy))
}

/// Names the font files a measurement profile was built from.
///
/// The identity is reported in merman's measurement evidence and is not part of
/// the SVG, so it traces the metric source without affecting the output bytes.
fn font_decorators(font_files: &[PathBuf]) -> Vec<String> {
    font_files
        .iter()
        .map(|path| format!("font:{}", path.to_string_lossy()))
        .collect()
}

/// Memo of parsed font sets, keyed by the requested file list.
///
/// One process normally renders every diagram of a document against the same
/// font files, and a GUI preview re-renders on each keystroke, so the metric
/// tables of a file list are read once instead of once per diagram.
static FONT_MEMO: LazyLock<Mutex<HashMap<Vec<PathBuf>, Arc<fonts::FontSet>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns the parsed faces for `font_files`.
///
/// The memo never invents output: it only caches the metric tables those files
/// contain, and failures are reported on every call.
fn font_set(font_files: &[PathBuf]) -> Result<Arc<fonts::FontSet>, String> {
    let mut entries = FONT_MEMO
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(cached) = entries.get(font_files) {
        return Ok(Arc::clone(cached));
    }

    let loaded =
        Arc::new(fonts::FontSet::load(font_files).map_err(|reason| error(RENDER_CODE, &reason))?);
    entries.insert(font_files.to_vec(), Arc::clone(&loaded));
    Ok(loaded)
}

/// Builds the request for one trimmed source.
fn svg_request(environment: SvgEnvironment, source: &str) -> SvgRequest {
    let options = SvgRenderOptions {
        // Source-derived, document-unique id: merman scopes generated marker,
        // arrowhead, and label ids with this value, and several diagrams are
        // embedded into one HTML page or Typst document, where identical
        // fallback ids would resolve to the first diagram's definitions.
        diagram_id: Some(cache::key(source)),
        ..SvgRenderOptions::default()
    };

    // API: merman 0.9.2 `SvgPipeline::resvg_safe()`
    // (https://docs.rs/merman/0.9.2/merman/svg/struct.SvgPipeline.html#method.resvg_safe)
    // selects `SvgPipelinePreset::ResvgSafe`: the readable text-overlay path with
    // the native `<foreignObject>` labels stripped, resvg/usvg rasterization
    // hazards removed, and structural references restricted to same-document
    // fragments. The `svg` feature (enabled through the workspace's
    // `complete-svg`) compiles it; a `None` pipeline would keep parity output,
    // which retains `<foreignObject>` and is therefore never requested here.
    SvgRequest {
        environment,
        options,
        pipeline: Some(SvgPipeline::resvg_safe()),
        ..SvgRequest::default()
    }
}

/// Rejects output that is not a standalone, HTML-label-free SVG document.
fn validate_output(svg: &str) -> Result<(), String> {
    if !svg.trim_start().starts_with("<svg") {
        let prefix: String = svg.trim_start().chars().take(32).collect();
        return Err(error(
            RENDER_CODE,
            &format!("rendered output is not an SVG document (starts with {prefix:?})"),
        ));
    }
    if contains_foreign_object(svg) {
        return Err(error(
            RENDER_CODE,
            "rendered SVG still contains a foreignObject HTML label",
        ));
    }
    Ok(())
}

/// Reports whether `svg` carries an HTML-label `foreignObject` element.
///
/// The resvg-safe pipeline only emits lower-case `data-merman-foreignobject` and
/// `merman-foreignobject-fallback` identifiers, so the mixed-case element name is
/// unambiguous; the additional case-insensitive tag scan catches an element that
/// a future pipeline change might write with a different case.
fn contains_foreign_object(svg: &str) -> bool {
    svg.contains("foreignObject")
        || contains_ascii_case_insensitive(svg.as_bytes(), b"<foreignobject")
}

/// ASCII-case-insensitive substring search.
fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Advance-width measurement from caller-supplied font files.
///
/// The model is an unshaped text-advance model built from the sfnt metric tables
/// of the caller's fonts:
///
/// * `cmap` maps each character to a glyph id (formats 0, 4, 6, and 12 cover
///   every cmap a modern font ships);
/// * `hmtx` supplies each glyph's advance width, repeated for glyphs beyond
///   `hhea.numberOfHMetrics`;
/// * `head.unitsPerEm` scales advances into CSS pixels at the label's font size;
/// * `name` identifies the face's family, subfamily, full, and PostScript names
///   so a label's CSS family stack selects its face.
///
/// Kerning, ligatures, and mark positioning are not applied, so a measured width
/// is the sum of the glyph advances; characters without coverage contribute the
/// `.notdef` advance. Measurement stays deterministic: faces are scanned in the
/// caller's file order, and ties keep the first face.
mod fonts {
    use std::fs;
    use std::path::{Path, PathBuf};

    use merman::svg::TextStyle;

    /// Largest font file read from disk; a guard against a misconfigured path.
    const MAX_FONT_BYTES: u64 = 64 << 20;
    /// Faces read from one font collection before the rest are skipped.
    const MAX_FACES_PER_FILE: usize = 32;
    /// sfnt table directory entries read from one face.
    const MAX_TABLE_RECORDS: usize = 256;
    /// Character-to-glyph pairs retained per face.
    const MAX_COVERAGE_PAIRS: usize = 1 << 18;
    /// `name` records examined per face.
    const MAX_NAME_RECORDS: usize = 512;
    /// Unicode scalar ceiling, so a malformed `cmap` cannot run away.
    const MAX_CODEPOINT: u32 = 0x10_FFFF;
    /// Mermaid's default label font size, used when a style has no usable size.
    const DEFAULT_FONT_SIZE_PX: f64 = 16.0;
    /// Tag of a TrueType/OpenType collection header.
    const COLLECTION_TAG: [u8; 4] = *b"ttcf";
    /// Accepted sfnt versions: TrueType, Apple TrueType, CFF OpenType, Type 1.
    const SFNT_VERSIONS: [u32; 4] = [0x0001_0000, 0x7472_7565, 0x4f54_544f, 0x7479_7031];

    /// The faces loaded from [`super::DiagramOptions::font_files`].
    pub(super) struct FontSet {
        faces: Vec<Face>,
    }

    impl FontSet {
        /// Loads every face of every supplied file.
        ///
        /// # Errors
        ///
        /// Returns a reason naming the offending path when a file cannot be
        /// read, is not an sfnt font, or yields no face with usable horizontal
        /// metrics. Unusable faces inside a readable collection are skipped as
        /// long as they are not the only faces in the file.
        pub(super) fn load(paths: &[PathBuf]) -> Result<Self, String> {
            let mut faces = Vec::new();
            for path in paths {
                let data = read_font_file(path)?;
                let bases = face_offsets(&data).map_err(|reason| describe(path, &reason))?;
                let mut usable = 0usize;
                let mut first_reason = None;
                for base in bases.into_iter().take(MAX_FACES_PER_FILE) {
                    match Face::parse(&data, base) {
                        Ok(face) => {
                            faces.push(face);
                            usable += 1;
                        }
                        Err(reason) => {
                            first_reason.get_or_insert(reason);
                        }
                    }
                }
                if usable == 0 {
                    let reason = first_reason
                        .unwrap_or_else(|| "font file contains no usable faces".to_owned());
                    return Err(describe(path, &reason));
                }
            }

            if faces.is_empty() {
                return Err("no font files were supplied".to_owned());
            }
            Ok(Self { faces })
        }

        /// Total advance width of `text` in CSS pixels at the style's font size.
        ///
        /// Finite and non-negative for every input, as merman's width callback
        /// requires.
        pub(super) fn width_px(&self, text: &str, style: &TextStyle) -> f64 {
            let Some(face) = self.face_for(style) else {
                return 0.0;
            };
            let font_size = match style.font_size {
                size if size.is_finite() && size > 0.0 => size,
                _ => DEFAULT_FONT_SIZE_PX,
            };

            let mut units = 0.0_f64;
            for ch in text.chars() {
                if is_zero_advance(ch) {
                    continue;
                }
                units += f64::from(face.advance(face.glyph(ch)));
            }

            let width = units * font_size / f64::from(face.units_per_em);
            if width.is_finite() && width > 0.0 {
                width
            } else {
                0.0
            }
        }

        /// The face a label's CSS font stack, weight, and slope select.
        ///
        /// Faces whose family names match the stack's earliest listed family win;
        /// among equals, the closest weight and slope win; ties keep the earliest
        /// face in file order. Labels that name no loaded family are measured with
        /// the closest-weight face of the first file, so supplied fonts still take
        /// effect for Mermaid's default `trebuchet ms, verdana, arial` stack.
        fn face_for(&self, style: &TextStyle) -> Option<&Face> {
            let families = requested_families(style.font_family.as_deref());
            let want_bold = wants_bold(style.font_weight.as_deref());
            let want_italic = wants_italic(style.font_style.as_deref());

            let mut best: Option<(usize, u8, &Face)> = None;
            for face in &self.faces {
                let key = face.selection_key(&families, want_bold, want_italic);
                let better = match &best {
                    Some((rank, flags, _)) => key.0 < *rank || (key.0 == *rank && key.1 > *flags),
                    None => true,
                };
                if better {
                    best = Some((key.0, key.1, face));
                }
            }
            best.map(|(_, _, face)| face)
        }
    }

    /// One parsed font face.
    struct Face {
        /// Metric units per em, never zero.
        units_per_em: u16,
        /// Advance width per glyph id in font units.
        advances: Vec<u16>,
        /// Sorted, duplicate-free character-to-glyph pairs.
        coverage: Vec<(u32, u16)>,
        /// Lower-case family and alias names this face answers to.
        names: Vec<String>,
        /// Whether the subfamily describes a bold face.
        bold: bool,
        /// Whether the subfamily describes a slanted face.
        italic: bool,
    }

    impl Face {
        /// Parses the face that starts at `base` inside `data`.
        fn parse(data: &[u8], base: usize) -> Result<Self, String> {
            match sfnt_version(data, base) {
                Some(version) if SFNT_VERSIONS.contains(&version) => {}
                Some(_) => {
                    return Err(
                        "unsupported font container (expected TrueType or OpenType sfnt)"
                            .to_owned(),
                    );
                }
                None => return Err("truncated sfnt header".to_owned()),
            }

            let tables = table_directory(data, base)
                .ok_or_else(|| "truncated sfnt table directory".to_owned())?;

            let head =
                find_table(&tables, b"head").ok_or_else(|| "missing head table".to_owned())?;
            let units_per_em = u16_at(data, field_offset(head, 18, "head")?)
                .filter(|units| *units > 0)
                .ok_or_else(|| "missing or invalid head unitsPerEm".to_owned())?;

            let hhea =
                find_table(&tables, b"hhea").ok_or_else(|| "missing hhea table".to_owned())?;
            let metric_count = u16_at(data, field_offset(hhea, 34, "hhea")?)
                .filter(|count| *count > 0)
                .ok_or_else(|| "missing or invalid hhea numberOfHMetrics".to_owned())?;

            let glyph_count = find_table(&tables, b"maxp")
                .and_then(|maxp| field_offset(maxp, 4, "maxp").ok())
                .and_then(|offset| u16_at(data, offset))
                .filter(|count| *count > 0)
                .unwrap_or(metric_count);

            let hmtx =
                find_table(&tables, b"hmtx").ok_or_else(|| "missing hmtx table".to_owned())?;
            let advances = parse_advances(data, hmtx, metric_count, glyph_count)?;

            let cmap =
                find_table(&tables, b"cmap").ok_or_else(|| "missing cmap table".to_owned())?;
            let coverage =
                parse_coverage(data, cmap).ok_or_else(|| "no usable cmap subtable".to_owned())?;

            let names = find_table(&tables, b"name")
                .map(|table| parse_names(data, table))
                .unwrap_or_default();
            let style_words = names.style_words().unwrap_or_default();

            Ok(Self {
                units_per_em,
                advances,
                coverage,
                names: names.matchable(),
                bold: style_words.contains("bold"),
                italic: style_words.contains("italic") || style_words.contains("oblique"),
            })
        }

        /// Glyph id for `ch`, or glyph 0 when the face has no mapping.
        fn glyph(&self, ch: char) -> u16 {
            let codepoint = ch as u32;
            match self
                .coverage
                .binary_search_by_key(&codepoint, |&(cp, _)| cp)
            {
                Ok(index) => self.coverage[index].1,
                Err(_) => 0,
            }
        }

        /// Advance width of `glyph` in font units.
        fn advance(&self, glyph: u16) -> u16 {
            match self.advances.get(usize::from(glyph)) {
                Some(advance) => *advance,
                None => self.advances.last().copied().unwrap_or(0),
            }
        }

        /// Family rank and weight/slope score; lower rank and higher score win.
        fn selection_key(
            &self,
            families: &[String],
            want_bold: bool,
            want_italic: bool,
        ) -> (usize, u8) {
            let mut flags = 0u8;
            if self.bold == want_bold {
                flags += 2;
            }
            if self.italic == want_italic {
                flags += 1;
            }
            (self.family_rank(families), flags)
        }

        /// Position of the first requested family this face answers to.
        ///
        /// `usize::MAX` when no requested family matches, so an unmatched stack
        /// still selects by weight and slope instead of failing.
        fn family_rank(&self, families: &[String]) -> usize {
            families
                .iter()
                .position(|wanted| self.names.iter().any(|name| name == wanted))
                .unwrap_or(usize::MAX)
        }
    }

    /// One sfnt table directory entry. Table lengths are not retained: every
    /// reader below is bounds-checked against the file, which is what a table
    /// length would otherwise guard.
    struct Table {
        tag: [u8; 4],
        offset: usize,
    }

    /// The sfnt version at `base`.
    fn sfnt_version(data: &[u8], base: usize) -> Option<u32> {
        u32_at(data, base)
    }

    /// The table directory of the face at `base`, in file order.
    fn table_directory(data: &[u8], base: usize) -> Option<Vec<Table>> {
        let declared = usize::from(u16_at(data, base.checked_add(4)?)?);
        let count = declared.min(MAX_TABLE_RECORDS);
        let mut tables = Vec::with_capacity(count);
        for index in 0..count {
            let record = base.checked_add(12)?.checked_add(index.checked_mul(16)?)?;
            let tag: [u8; 4] = slice_at(data, record, 4)?.try_into().ok()?;
            tables.push(Table {
                tag,
                offset: u32_at(data, record.checked_add(8)?)? as usize,
            });
        }
        Some(tables)
    }

    /// The first table with `tag`.
    fn find_table<'a>(tables: &'a [Table], tag: &[u8; 4]) -> Option<&'a Table> {
        tables.iter().find(|table| &table.tag == tag)
    }

    /// `table.offset + delta`, reported as an error when the addition overflows.
    fn field_offset(table: &Table, delta: usize, table_name: &str) -> Result<usize, String> {
        table
            .offset
            .checked_add(delta)
            .ok_or_else(|| format!("{table_name} table offset overflows"))
    }

    /// Advance widths per glyph id, with the last horizontal metric repeated for
    /// the glyphs that `hhea.numberOfHMetrics` does not list.
    fn parse_advances(
        data: &[u8],
        hmtx: &Table,
        metric_count: u16,
        glyph_count: u16,
    ) -> Result<Vec<u16>, String> {
        let declared = usize::from(metric_count);
        let mut advances = Vec::with_capacity(usize::from(glyph_count).max(declared));
        for index in 0..declared {
            // Each entry is an advance width followed by a left side bearing,
            // which advance measurement does not use.
            let offset = field_offset(hmtx, index * 4, "hmtx")?;
            let advance = u16_at(data, offset).ok_or_else(|| "truncated hmtx table".to_owned())?;
            advances.push(advance);
        }
        let last = advances.last().copied().unwrap_or(0);
        advances.resize(usize::from(glyph_count).max(declared), last);
        Ok(advances)
    }

    /// Character coverage from the best usable `cmap` subtable.
    fn parse_coverage(data: &[u8], cmap: &Table) -> Option<Vec<(u32, u16)>> {
        let record_count = usize::from(u16_at(data, cmap.offset.checked_add(2)?)?);
        let mut candidates: Vec<(u8, usize)> = Vec::new();
        for index in 0..record_count {
            let record = cmap
                .offset
                .checked_add(4)?
                .checked_add(index.checked_mul(8)?)?;
            let platform = u16_at(data, record)?;
            let encoding = u16_at(data, record.checked_add(2)?)?;
            let subtable = cmap
                .offset
                .checked_add(u32_at(data, record.checked_add(4)?)? as usize)?;
            let format = u16_at(data, subtable)?;
            let score = subtable_score(platform, encoding, format);
            if score > 0 {
                candidates.push((score, subtable));
            }
        }

        // Stable sort: equal scores keep file order, so selection is fixed.
        candidates.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        for (_, subtable) in candidates {
            let Some(format) = u16_at(data, subtable) else {
                continue;
            };
            let parsed = match format {
                0 => parse_coverage_format_0(data, subtable),
                4 => parse_coverage_format_4(data, subtable),
                6 => parse_coverage_format_6(data, subtable),
                12 => parse_coverage_format_12(data, subtable),
                _ => None,
            };
            if let Some(coverage) = parsed {
                let coverage = normalize_coverage(coverage);
                if !coverage.is_empty() {
                    return Some(coverage);
                }
            }
        }
        None
    }

    /// Preference for a `cmap` subtable: wider formats first, then platform.
    fn subtable_score(platform: u16, encoding: u16, format: u16) -> u8 {
        let format_rank = match format {
            12 => 4,
            4 => 3,
            6 => 2,
            0 => 1,
            _ => return 0,
        };
        let platform_rank = match (platform, encoding) {
            (3, 10) => 5,
            (0, _) => 4,
            (3, 1) => 3,
            (3, 0) => 2,
            (1, _) => 1,
            _ => 0,
        };
        format_rank * 6 + platform_rank
    }

    /// Byte-indexed `cmap` subtable: 256 glyph ids from character code 0.
    fn parse_coverage_format_0(data: &[u8], offset: usize) -> Option<Vec<(u32, u16)>> {
        let glyphs = slice_at(data, offset.checked_add(6)?, 256)?;
        Some(
            glyphs
                .iter()
                .enumerate()
                .map(|(index, glyph)| (index as u32, u16::from(*glyph)))
                .collect(),
        )
    }

    /// Segment-mapped `cmap` subtable covering the basic multilingual plane.
    fn parse_coverage_format_4(data: &[u8], offset: usize) -> Option<Vec<(u32, u16)>> {
        let segment_count = usize::from(u16_at(data, offset.checked_add(6)?)?) / 2;
        let end_codes = offset.checked_add(14)?;
        let start_codes = end_codes
            .checked_add(segment_count.checked_mul(2)?)?
            .checked_add(2)?;
        let deltas = start_codes.checked_add(segment_count.checked_mul(2)?)?;
        let range_offsets = deltas.checked_add(segment_count.checked_mul(2)?)?;

        let mut coverage = Vec::new();
        for segment in 0..segment_count {
            let slot = segment.checked_mul(2)?;
            let end = u16_at(data, end_codes.checked_add(slot)?)?;
            let start = u16_at(data, start_codes.checked_add(slot)?)?;
            let delta = i16_at(data, deltas.checked_add(slot)?)?;
            let range_offset = u16_at(data, range_offsets.checked_add(slot)?)?;
            if start > end {
                continue;
            }

            for code in u32::from(start)..=u32::from(end) {
                if coverage.len() >= MAX_COVERAGE_PAIRS {
                    return Some(coverage);
                }
                let glyph = if range_offset == 0 {
                    // No indirection: the character code plus the segment delta.
                    (code as u16).wrapping_add(delta as u16)
                } else {
                    let address = range_offsets
                        .checked_add(slot)?
                        .checked_add(usize::from(range_offset))?
                        .checked_add((code as usize - usize::from(start)) * 2)?;
                    let glyph = u16_at(data, address)?;
                    if glyph == 0 {
                        continue;
                    }
                    glyph.wrapping_add(delta as u16)
                };
                if glyph != 0 {
                    coverage.push((code, glyph));
                }
            }
        }
        Some(coverage)
    }

    /// Trimmed-array `cmap` subtable.
    fn parse_coverage_format_6(data: &[u8], offset: usize) -> Option<Vec<(u32, u16)>> {
        let first = u32::from(u16_at(data, offset.checked_add(6)?)?);
        let count = usize::from(u16_at(data, offset.checked_add(8)?)?);
        let mut coverage = Vec::new();
        for index in 0..count.min(MAX_COVERAGE_PAIRS) {
            let address = offset.checked_add(10)?.checked_add(index.checked_mul(2)?)?;
            let glyph = u16_at(data, address)?;
            if glyph != 0 {
                coverage.push((first.saturating_add(index as u32), glyph));
            }
        }
        Some(coverage)
    }

    /// Segmented-coverage `cmap` subtable, the full Unicode range.
    fn parse_coverage_format_12(data: &[u8], offset: usize) -> Option<Vec<(u32, u16)>> {
        let group_count = u32_at(data, offset.checked_add(12)?)? as usize;
        let mut coverage = Vec::new();
        for index in 0..group_count {
            let group = offset
                .checked_add(16)?
                .checked_add(index.checked_mul(12)?)?;
            let first = u32_at(data, group)?;
            let last = u32_at(data, group.checked_add(4)?)?;
            let first_glyph = u32_at(data, group.checked_add(8)?)?;
            if last < first || first > MAX_CODEPOINT {
                continue;
            }

            for code in first..=last.min(MAX_CODEPOINT) {
                if coverage.len() >= MAX_COVERAGE_PAIRS {
                    return Some(coverage);
                }
                let glyph = first_glyph.saturating_add(code - first);
                if glyph > u32::from(u16::MAX) {
                    continue;
                }
                coverage.push((code, glyph as u16));
            }
        }
        Some(coverage)
    }

    /// Sorts coverage by character code and drops duplicate mappings, keeping the
    /// first one encountered so the binary search in [`Face::glyph`] is exact.
    fn normalize_coverage(mut coverage: Vec<(u32, u16)>) -> Vec<(u32, u16)> {
        coverage.sort_by_key(|&(code, _)| code);
        coverage.dedup_by_key(|entry| entry.0);
        coverage
    }

    /// Names read from a face's `name` table.
    #[derive(Default)]
    struct FaceNames {
        /// `(name id, platform rank, value)` rows in file order.
        records: Vec<(u16, u8, String)>,
    }

    impl FaceNames {
        /// Best-ranked, lower-case value for one name id.
        fn best(&self, name_id: u16) -> Option<String> {
            let mut best: Option<(u8, &str)> = None;
            for entry in self.records.iter().filter(|entry| entry.0 == name_id) {
                if best.is_none_or(|(rank, _)| entry.1 > rank) {
                    best = Some((entry.1, entry.2.as_str()));
                }
            }
            best.map(|(_, value)| value.to_ascii_lowercase())
        }

        /// Words describing the face's weight and slope: subfamily, else full name.
        fn style_words(&self) -> Option<String> {
            self.best(2).or_else(|| self.best(4))
        }

        /// Family names plus full and PostScript aliases used for family matching.
        fn matchable(&self) -> Vec<String> {
            let mut names: Vec<String> = Vec::new();
            for name_id in [1u16, 16, 4, 6] {
                if let Some(value) = self.best(name_id)
                    && !names.contains(&value)
                {
                    names.push(value);
                }
            }
            names
        }
    }

    /// Reads a face's family, subfamily, full, and PostScript names.
    fn parse_names(data: &[u8], table: &Table) -> FaceNames {
        let mut names = FaceNames::default();
        let Some((record_count, storage)) = name_table_header(data, table) else {
            return names;
        };

        for index in 0..record_count {
            let Some(record) = read_name_record(data, table, storage, index) else {
                continue;
            };
            if !matches!(record.name_id, 1 | 2 | 4 | 6 | 16) {
                continue;
            }
            let value = record
                .value
                .trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{0}')
                .to_owned();
            if value.is_empty() {
                continue;
            }
            names.records.push((
                record.name_id,
                platform_rank(record.platform, record.encoding),
                value,
            ));
        }
        names
    }

    /// A `name` table's record count and the absolute offset of its string data.
    fn name_table_header(data: &[u8], table: &Table) -> Option<(usize, usize)> {
        let count = usize::from(u16_at(data, table.offset.checked_add(2)?)?).min(MAX_NAME_RECORDS);
        let storage = table
            .offset
            .checked_add(usize::from(u16_at(data, table.offset.checked_add(4)?)?))?;
        Some((count, storage))
    }

    /// One decoded `name` record.
    struct NameRecord {
        platform: u16,
        encoding: u16,
        name_id: u16,
        value: String,
    }

    /// Reads and decodes one `name` record.
    fn read_name_record(
        data: &[u8],
        table: &Table,
        storage: usize,
        index: usize,
    ) -> Option<NameRecord> {
        let record = field_offset(table, index.checked_mul(12)?.checked_add(6)?, "name").ok()?;
        let platform = u16_at(data, record)?;
        let encoding = u16_at(data, record.checked_add(2)?)?;
        let name_id = u16_at(data, record.checked_add(6)?)?;
        let length = usize::from(u16_at(data, record.checked_add(8)?)?);
        let offset = usize::from(u16_at(data, record.checked_add(10)?)?);
        let bytes = slice_at(data, storage.checked_add(offset)?, length)?;
        Some(NameRecord {
            platform,
            encoding,
            name_id,
            value: decode_name_string(platform, bytes),
        })
    }

    /// Decodes a `name` string: UTF-16BE for Unicode platforms, Latin-1 otherwise.
    fn decode_name_string(platform: u16, bytes: &[u8]) -> String {
        if matches!(platform, 0 | 3) {
            let mut units = Vec::with_capacity(bytes.len() / 2);
            for pair in bytes.as_chunks::<2>().0 {
                units.push(u16::from_be_bytes(*pair));
            }
            return String::from_utf16_lossy(&units);
        }
        bytes.iter().map(|byte| char::from(*byte)).collect()
    }

    /// Preference for a `name` record: Unicode entries first, then Windows, then
    /// Macintosh.
    fn platform_rank(platform: u16, encoding: u16) -> u8 {
        match (platform, encoding) {
            (3, 10) => 5,
            (3, 1) => 4,
            (0, _) => 3,
            (3, _) => 2,
            (1, _) => 1,
            _ => 0,
        }
    }

    /// Face offsets inside one font file: a plain sfnt has one face at offset 0,
    /// a `ttcf` collection lists its faces in its header.
    fn face_offsets(data: &[u8]) -> Result<Vec<usize>, String> {
        if slice_at(data, 0, 4) == Some(COLLECTION_TAG.as_slice()) {
            let declared =
                u32_at(data, 8).ok_or_else(|| "truncated TrueType collection header".to_owned())?;
            let count = (declared as usize).min(MAX_FACES_PER_FILE);
            let mut offsets = Vec::with_capacity(count);
            for index in 0..count {
                let offset = u32_at(data, 12 + index * 4)
                    .ok_or_else(|| "truncated TrueType collection face list".to_owned())?;
                offsets.push(offset as usize);
            }
            if offsets.is_empty() {
                return Err("font collection contains no faces".to_owned());
            }
            return Ok(offsets);
        }
        Ok(vec![0])
    }

    /// Reads one font file, rejecting paths that are not regular files or are
    /// implausibly large.
    fn read_font_file(path: &Path) -> Result<Vec<u8>, String> {
        let metadata =
            fs::metadata(path).map_err(|failure| describe(path, &failure.to_string()))?;
        if !metadata.is_file() {
            return Err(describe(path, "not a regular file"));
        }
        if metadata.len() > MAX_FONT_BYTES {
            return Err(describe(
                path,
                &format!("font file exceeds {MAX_FONT_BYTES} bytes"),
            ));
        }

        let data = fs::read(path).map_err(|failure| describe(path, &failure.to_string()))?;
        if data.len() as u64 > MAX_FONT_BYTES {
            return Err(describe(
                path,
                &format!("font file exceeds {MAX_FONT_BYTES} bytes"),
            ));
        }
        Ok(data)
    }

    /// Formats a font problem with its path, without the error code prefix.
    fn describe(path: &Path, reason: &str) -> String {
        format!("{}: {reason}", path.display())
    }

    /// CSS font stacks are comma separated; each family is unquoted, trimmed, and
    /// lower-cased so it can be compared with the names stored in a face.
    fn requested_families(font_family: Option<&str>) -> Vec<String> {
        let Some(stack) = font_family else {
            return Vec::new();
        };
        stack
            .split(',')
            .map(|family| {
                family
                    .trim()
                    .trim_matches(|ch: char| matches!(ch, '"' | '\''))
                    .trim()
                    .to_ascii_lowercase()
            })
            .filter(|family| !family.is_empty())
            .collect()
    }

    /// CSS weight keywords and numeric weights at or above 600 select a bold face.
    fn wants_bold(weight: Option<&str>) -> bool {
        let Some(weight) = weight else {
            return false;
        };
        let weight = weight.trim();
        if weight.eq_ignore_ascii_case("bold") || weight.eq_ignore_ascii_case("bolder") {
            return true;
        }
        weight.parse::<u32>().is_ok_and(|numeric| numeric >= 600)
    }

    /// `italic` and `oblique` select a slanted face.
    fn wants_italic(style: Option<&str>) -> bool {
        let Some(style) = style else {
            return false;
        };
        let style = style.trim().to_ascii_lowercase();
        style.contains("italic") || style.contains("oblique")
    }

    /// Characters that add no advance width: controls, the soft hyphen, and
    /// zero-width format characters. Measured text therefore ignores line breaks
    /// and joiners instead of charging them a `.notdef` advance.
    fn is_zero_advance(ch: char) -> bool {
        ch.is_control()
            || matches!(
                ch,
                '\u{ad}'
                    | '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{2064}'
                    | '\u{feff}'
            )
    }

    /// Big-endian `u16` at `offset`.
    fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
        let end = offset.checked_add(2)?;
        let bytes: [u8; 2] = data.get(offset..end)?.try_into().ok()?;
        Some(u16::from_be_bytes(bytes))
    }

    /// Big-endian `i16` at `offset`.
    fn i16_at(data: &[u8], offset: usize) -> Option<i16> {
        u16_at(data, offset).map(|value| value as i16)
    }

    /// Big-endian `u32` at `offset`.
    fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
        let end = offset.checked_add(4)?;
        let bytes: [u8; 4] = data.get(offset..end)?.try_into().ok()?;
        Some(u32::from_be_bytes(bytes))
    }

    /// `len` bytes at `offset`.
    fn slice_at(data: &[u8], offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        data.get(offset..end)
    }

    #[cfg(test)]
    mod tests {
        use super::{Face, face_offsets};

        #[test]
        fn truncated_font_data_is_rejected_without_panicking() {
            assert!(face_offsets(b"ttcf\x00\x01\x00\x00").is_err());
            assert!(Face::parse(b"", 0).is_err());
            assert!(Face::parse(b"\x00\x01\x00\x00", 0).is_err());
            assert!(Face::parse(b"not a font at all", 0).is_err());
        }

        #[test]
        fn plain_sfnt_files_expose_one_face_at_offset_zero() {
            assert_eq!(face_offsets(b"\x00\x01\x00\x00").ok(), Some(vec![0]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DiagramOptions, render_svg, validate_output};

    /// A source that renders as the smallest useful Mermaid diagram.
    const SOURCE: &str = "flowchart TD\nA --> B";

    #[test]
    fn empty_source_is_rejected_before_rendering() {
        let rejected = render_svg(" \n\t ", &DiagramOptions::default())
            .is_err_and(|failure| failure.starts_with("E_DIAGRAM_PARSE:"));
        assert!(rejected, "an empty source must fail with E_DIAGRAM_PARSE");
    }

    #[test]
    fn output_contract_rejects_missing_svg_and_html_labels() {
        assert!(validate_output("<svg id=\"a\"><text>ok</text></svg>").is_ok());
        assert!(validate_output("  <svg/>").is_ok());
        // Fallback identifiers keep the lower-case spelling, which is allowed.
        assert!(validate_output("<svg><g class=\"merman-foreignobject-fallback\"/></svg>").is_ok());

        let rejected = validate_output("<p>not an svg</p>")
            .is_err_and(|failure| failure.starts_with("E_DIAGRAM_RENDER:"));
        assert!(rejected, "non-SVG output must fail with E_DIAGRAM_RENDER");
        assert!(validate_output("").is_err());
        assert!(validate_output("<svg><foreignObject width=\"8\" height=\"8\"/></svg>").is_err());
        assert!(validate_output("<svg></foreignObject></svg>").is_err());
        assert!(validate_output("<svg><FOREIGNOBJECT/></svg>").is_err());
    }

    #[test]
    fn missing_font_files_fail_with_the_render_code() {
        let missing = std::env::temp_dir().join("mdx-diagram-missing-font-file.ttf");
        let _ = std::fs::remove_file(&missing);
        let options = DiagramOptions {
            deterministic_text: true,
            font_files: vec![missing],
        };

        let rejected = render_svg(SOURCE, &options)
            .is_err_and(|failure| failure.starts_with("E_DIAGRAM_RENDER:"));
        assert!(
            rejected,
            "a missing font file must fail with E_DIAGRAM_RENDER"
        );
    }

    #[test]
    fn directories_are_not_font_files() {
        let options = DiagramOptions {
            deterministic_text: true,
            font_files: vec![std::env::temp_dir()],
        };

        let rejected = render_svg(SOURCE, &options)
            .is_err_and(|failure| failure.starts_with("E_DIAGRAM_RENDER:"));
        assert!(rejected, "a directory must fail with E_DIAGRAM_RENDER");
    }

    #[test]
    fn non_sfnt_files_fail_with_the_render_code() {
        let path = std::env::temp_dir().join("mdx-diagram-garbage-font-file.ttf");
        // A read-only or missing scratch directory skips the file-level check;
        // the parser rejection itself is covered by the `fonts` unit tests.
        if std::fs::write(&path, b"definitely not an sfnt font").is_err() {
            return;
        }
        let options = DiagramOptions {
            deterministic_text: true,
            font_files: vec![path.clone()],
        };

        let rejected = render_svg(SOURCE, &options)
            .is_err_and(|failure| failure.starts_with("E_DIAGRAM_RENDER:"));
        let _ = std::fs::remove_file(&path);
        assert!(rejected, "a non-sfnt file must fail with E_DIAGRAM_RENDER");
    }
}

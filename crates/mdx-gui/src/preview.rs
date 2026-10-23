//! SVG page preview pane.
//!
//! The SVG exporter in `mdx-typst` emits one standalone SVG document per page.
//! This module draws such a page list as a scrollable column of images inside an
//! [`egui::Ui`], with page numbers and a choice between "fit width" and an
//! explicit zoom factor.
//!
//! Every page is handed to egui as a `data:image/svg+xml;base64,…` data URI.
//! [`SvgDataUriLoader`] — registered lazily, on the first call to [`show`] — is
//! the [`egui::load::ImageLoader`] that understands those URIs: it only decodes
//! the base64 payload already in memory and rasterizes it through `egui_extras`'
//! SVG support. Nothing here touches the filesystem, spawns threads, or reads a
//! clock.
//!
//! All state lives either in the caller's values (`pages`, `zoom`) or in egui's
//! temporary memory (the fit-width toggle and the per-page cached data URIs).

use std::sync::Arc;

use egui::load::{ImageLoadResult, ImageLoader, LoadError, SizeHint};
use egui::{Color32, CornerRadius, Id, Margin, RichText, Ui, Vec2, vec2};

/// Smallest zoom factor the pane accepts.
pub const MIN_ZOOM: f32 = 0.25;

/// Largest zoom factor the pane accepts.
pub const MAX_ZOOM: f32 = 4.0;

/// Zoom factor restored by the pane's "100 %" button.
pub const DEFAULT_ZOOM: f32 = 1.0;

/// Media-type prefix of the data URI used for every page.
const DATA_URI_PREFIX: &str = "data:image/svg+xml;base64,";

/// Id of the data-URI image loader, used to detect that it is already installed.
const LOADER_ID: &str = concat!(module_path!(), "::SvgDataUriLoader");

/// Vertical gap between two consecutive pages, in points.
const PAGE_GAP: f32 = 16.0;

/// A page is only rasterized again once the requested width moved by this
/// factor, so dragging the zoom control does not re-rasterize on every step.
const RASTER_HYSTERESIS: f32 = 1.25;

/// Draw the preview pane.
///
/// `pages` holds one standalone SVG document per page, in document order (as
/// produced by `mdx-typst`'s SVG exporter). `zoom` is the pane's persistent zoom
/// factor: it is clamped into [`MIN_ZOOM`]`..=`[`MAX_ZOOM`] and written back when
/// the user drags the zoom control. Whether pages are scaled to the pane width
/// instead is kept in egui's temporary memory and defaults to "fit width".
///
/// An empty `pages` slice draws a placeholder instead of the page list.
pub fn show(ui: &mut Ui, pages: &[String], zoom: &mut f32) {
    *zoom = normalize_zoom(*zoom);

    if pages.is_empty() {
        placeholder(ui);
        return;
    }

    ensure_loader(ui.ctx());

    let mut fit_width = fit_width_enabled(ui.ctx());
    let pane_width = ui.available_width();

    ui.horizontal(|ui| {
        ui.checkbox(&mut fit_width, "Fit width");
        ui.add_enabled_ui(!fit_width, |ui| {
            ui.add(
                egui::DragValue::new(zoom)
                    .speed(0.01)
                    .range(MIN_ZOOM..=MAX_ZOOM)
                    .fixed_decimals(2)
                    .suffix("x"),
            );
        });
        if ui.button("100 %").clicked() {
            *zoom = DEFAULT_ZOOM;
        }
        if let Some(scale) = pages
            .first()
            .and_then(|svg| display_scale(svg_intrinsic_size(svg), pane_width, fit_width, *zoom))
        {
            ui.label(
                RichText::new(format!("{:.0} %", scale * 100.0))
                    .weak()
                    .monospace(),
            );
        }
        ui.separator();
        ui.label(RichText::new(page_count_label(pages.len())).weak());
    });

    set_fit_width(ui.ctx(), fit_width);
    ui.separator();

    let effective_zoom = normalize_zoom(*zoom);
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, svg) in pages.iter().enumerate() {
                draw_page(ui, index, pages.len(), svg, effective_zoom, fit_width);
                if index + 1 < pages.len() {
                    ui.add_space(PAGE_GAP);
                }
            }
        });
}

/// Draw one page: its header line and the rasterized page itself.
fn draw_page(ui: &mut Ui, index: usize, total: usize, svg: &str, zoom: f32, fit_width: bool) {
    let intrinsic = svg_intrinsic_size(svg);
    let scale = display_scale(intrinsic, ui.available_width(), fit_width, zoom);
    let display = intrinsic
        .zip(scale)
        .map(|(size, scale)| clamp_to_texture(ui.ctx(), size * scale));

    ui.horizontal(|ui| {
        ui.label(RichText::new(page_label(index, total)).strong());
        if let Some(size) = intrinsic {
            ui.label(
                RichText::new(format!("{:.0} x {:.0} pt", size.x, size.y))
                    .weak()
                    .monospace(),
            );
        }
        if let Some(scale) = scale {
            ui.label(
                RichText::new(format!("{:.0} %", scale * 100.0))
                    .weak()
                    .monospace(),
            );
        }
    });

    let uri = page_data_uri(ui.ctx(), index, svg);
    if let Some(display) = display {
        refresh_raster(
            ui.ctx(),
            index,
            &uri,
            display.x * ui.ctx().pixels_per_point(),
        );
    }

    let image = match display {
        Some(display) => egui::Image::from_uri(&*uri).fit_to_exact_size(display),
        // Unknown page size: let egui shrink the page into the available space.
        None => egui::Image::from_uri(&*uri),
    }
    .alt_text(page_label(index, total));

    // A white sheet behind the page, so transparent SVGs still read as paper.
    egui::Frame::new()
        .fill(Color32::WHITE)
        .corner_radius(CornerRadius::same(2))
        .inner_margin(Margin::same(1))
        .show(ui, |ui| {
            ui.add(image);
        });
}

/// Caps a page's on-screen size so its rasterized texture stays inside the
/// backend's texture limit. A very wide pane in "Fit width" mode would
/// otherwise ask for a texture larger than the GPU (or a headless `Context`)
/// accepts.
fn clamp_to_texture(ctx: &egui::Context, size: Vec2) -> Vec2 {
    let max_side = ctx.input(|i| i.max_texture_side) as f32;
    let pixels_per_point = ctx.pixels_per_point().max(0.1);
    let limit = (max_side / pixels_per_point).max(64.0);
    if size.x.is_finite() && size.y.is_finite() && size.x > 0.0 && size.y > 0.0 {
        let scale = (limit / size.x).min(limit / size.y).min(1.0);
        size * scale
    } else {
        size
    }
}

/// Empty-state body, shown when there is nothing to render into the pane.
///
/// This covers both "not rendered yet" and "rendering failed": the caller
/// reports failures through the diagnostics pane instead.
fn placeholder(ui: &mut Ui) {
    ui.add_space(PAGE_GAP);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("No pages to preview").size(16.0).strong());
        ui.label(
            RichText::new("Render the document, or check the diagnostics for the reason.").weak(),
        );
    });
}

/// `Page 3 / 7`.
fn page_label(index: usize, total: usize) -> String {
    format!("Page {} / {total}", index + 1)
}

/// `1 page` / `7 pages`.
fn page_count_label(total: usize) -> String {
    if total == 1 {
        "1 page".to_owned()
    } else {
        format!("{total} pages")
    }
}

/// Clamp `zoom` into the supported range, replacing non-finite values.
fn normalize_zoom(zoom: f32) -> f32 {
    if zoom.is_finite() {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        DEFAULT_ZOOM
    }
}

/// Scale factor that makes a page `page_width` points wide fill `available_width`.
fn fit_scale(page_width: f32, available_width: f32) -> f32 {
    if page_width.is_finite()
        && page_width > 0.0
        && available_width.is_finite()
        && available_width > 0.0
    {
        available_width / page_width
    } else {
        DEFAULT_ZOOM
    }
}

/// Scale factor the page is drawn with, or `None` when its size is unknown.
fn display_scale(
    intrinsic: Option<Vec2>,
    available_width: f32,
    fit_width: bool,
    zoom: f32,
) -> Option<f32> {
    let size = intrinsic?;
    if !size.x.is_finite() || !size.y.is_finite() {
        return None;
    }
    if size.x <= 0.0 || size.y <= 0.0 {
        return None;
    }
    Some(if fit_width {
        fit_scale(size.x, available_width)
    } else {
        normalize_zoom(zoom)
    })
}

/// Whether the pane currently scales pages to its width instead of using `zoom`.
fn fit_width_enabled(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(fit_width_id()).unwrap_or(true))
}

/// Remember the fit-width toggle for the next frame.
fn set_fit_width(ctx: &egui::Context, fit_width: bool) {
    ctx.data_mut(|data| data.insert_temp(fit_width_id(), fit_width));
}

/// Id of the temporary fit-width flag.
fn fit_width_id() -> Id {
    Id::unique("mdx-gui::preview::fit-width")
}

/// Data URI for one page, reused while the SVG it was built from is unchanged.
///
/// The cache entry is keyed by page index and validated with a content
/// fingerprint. When a page's content changes, every cache derived from its
/// previous data URI is dropped, so an editing session does not accumulate
/// stale textures, rasterized pages or published bytes.
fn page_data_uri(ctx: &egui::Context, page: usize, svg: &str) -> Arc<str> {
    let id = Id::unique(("mdx-gui::preview::data-uri", page));
    let fingerprint = fingerprint(svg.as_bytes());

    if let Some((cached, uri)) = ctx.data(|data| data.get_temp::<(u64, Arc<str>)>(id)) {
        if cached == fingerprint {
            return uri;
        }
        forget_page(ctx, &uri);
    }

    let uri: Arc<str> = Arc::from(svg_data_uri(svg));
    let stored = Arc::clone(&uri);
    ctx.data_mut(|data| data.insert_temp(id, (fingerprint, stored)));
    uri
}

/// Drop everything cached for one page's data URI.
fn forget_page(ctx: &egui::Context, uri: &str) {
    ctx.forget_image(uri);
    if let Some(key) = svg_bytes_key(uri) {
        ctx.forget_image(&key);
    }
}

/// Rasterize the page again when its raster width moved by more than
/// [`RASTER_HYSTERESIS`], so zooming produces a crisp SVG at the new size.
fn refresh_raster(ctx: &egui::Context, page: usize, uri: &str, pixel_width: f32) {
    let id = Id::unique(("mdx-gui::preview::raster-width", page));
    let previous = ctx.data(|data| data.get_temp::<f32>(id));

    if raster_refresh_needed(previous, pixel_width) {
        ctx.forget_image(uri);
    }
    ctx.data_mut(|data| data.insert_temp(id, pixel_width));
}

/// Whether a page cached at `previous` pixels wide must be rasterized again.
///
/// No refresh is needed without a previous width: nothing has been rasterized
/// yet, so the first load already uses the requested size.
fn raster_refresh_needed(previous: Option<f32>, pixel_width: f32) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if !previous.is_finite() || !pixel_width.is_finite() {
        return false;
    }
    if previous <= 0.0 || pixel_width <= 0.0 {
        return false;
    }
    pixel_width > previous * RASTER_HYSTERESIS || pixel_width * RASTER_HYSTERESIS < previous
}

/// `data:image/svg+xml;base64,…` for `svg`.
pub fn svg_data_uri(svg: &str) -> String {
    let mut uri = String::with_capacity(DATA_URI_PREFIX.len() + base64_len(svg.len()));
    uri.push_str(DATA_URI_PREFIX);
    uri.push_str(&base64_encode(svg.as_bytes()));
    uri
}

/// Intrinsic page size in the SVG's own units (Typst emits points).
///
/// `width`/`height` win when both are absolute lengths; otherwise the root
/// element's `viewBox` decides. `None` means the page carries no usable size.
fn svg_intrinsic_size(svg: &str) -> Option<Vec2> {
    let tag = root_tag(svg);
    let width = attr_value(tag, "width").and_then(parse_length);
    let height = attr_value(tag, "height").and_then(parse_length);
    if let (Some(width), Some(height)) = (width, height) {
        return Some(vec2(width, height));
    }
    view_box_size(tag)
}

/// The root element's opening tag, i.e. everything up to its first `>`.
fn root_tag(svg: &str) -> &str {
    match svg.find('>') {
        Some(end) => &svg[..end],
        None => svg,
    }
}

/// Value of attribute `name` in `tag`, quotes stripped.
///
/// Only attributes whose name is delimited on the left are matched, so a search
/// for `width` does not hit `stroke-width`.
fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let bytes = tag.as_bytes();
    let mut search_from = 0;

    while let Some(offset) = tag[search_from..].find(name) {
        let start = search_from + offset;
        let end = start + name.len();
        search_from = end;

        if start > 0 && is_name_byte(bytes[start - 1]) {
            // Part of a longer attribute name, such as `width` in `stroke-width`.
            continue;
        }
        if let Some(value) = quoted_value(tag[end..].trim_start()) {
            return Some(value);
        }
    }
    None
}

/// Value of a `="…"` (or `='…'`) tail, quotes stripped.
fn quoted_value(rest: &str) -> Option<&str> {
    let rest = rest.strip_prefix('=')?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let quoted = &rest[quote.len_utf8()..];
    let close = quoted.find(quote)?;
    Some(&quoted[..close])
}

/// Whether `byte` may be part of an attribute name.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b':'
}

/// Whether `character` may appear inside the numeric part of a length.
fn is_number_byte(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, '.' | '-' | '+' | 'e' | 'E')
}

/// Parse an absolute SVG length into SVG user units (`pt`).
///
/// Relative lengths (`%`, `em`, …) yield `None`, because they cannot be resolved
/// without a layout context.
fn parse_length(value: &str) -> Option<f32> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let split = value
        .find(|c: char| !is_number_byte(c))
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number: f32 = number.parse().ok()?;
    let factor = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "px" | "pt" => 1.0,
        "in" => 72.0,
        "pc" => 12.0,
        "mm" => 72.0 / 25.4,
        "cm" => 72.0 / 2.54,
        "q" => 72.0 / 101.6,
        _ => return None,
    };
    let scaled = number * factor;
    if scaled.is_finite() && scaled > 0.0 {
        Some(scaled)
    } else {
        None
    }
}

/// Size encoded by a `viewBox="min-x min-y width height"` attribute.
fn view_box_size(tag: &str) -> Option<Vec2> {
    let numbers: Vec<f32> = attr_value(tag, "viewBox")?
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f32>().ok())
        .collect();

    let [_, _, width, height] = numbers.as_slice() else {
        return None;
    };
    if width.is_finite() && height.is_finite() && *width > 0.0 && *height > 0.0 {
        Some(vec2(*width, *height))
    } else {
        None
    }
}

/// Base64 length of an `input_len` byte payload, including padding.
fn base64_len(input_len: usize) -> usize {
    input_len.div_ceil(3) * 4
}

/// Encode `bytes` as standard base64 (RFC 4648, with padding).
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(base64_len(bytes.len()));
    base64_encode_into(bytes, &mut out);
    out
}

/// Append the standard base64 encoding of `bytes` to `out`.
fn base64_encode_into(bytes: &[u8], out: &mut String) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    for chunk in bytes.chunks(3) {
        let mut triple = [0u8; 3];
        let len = chunk.len();
        triple[..len].copy_from_slice(chunk);
        let group =
            (u32::from(triple[0]) << 16) | (u32::from(triple[1]) << 8) | u32::from(triple[2]);

        out.push(ALPHABET[((group >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((group >> 12) & 0x3f) as usize] as char);
        if len > 1 {
            out.push(ALPHABET[((group >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if len > 2 {
            out.push(ALPHABET[(group & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
}

/// Decode standard base64, tolerating embedded line breaks but no other junk.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut accumulator: u32 = 0;
    let mut bit_count: u32 = 0;
    let mut padding = 0usize;

    for byte in text.bytes() {
        match byte {
            b'=' => padding += 1,
            b'\n' | b'\r' | b' ' | b'\t' => {}
            _ => {
                if padding > 0 {
                    return None;
                }
                let value = base64_value(byte)?;
                accumulator = (accumulator << 6) | u32::from(value);
                bit_count += 6;
                if bit_count >= 8 {
                    bit_count -= 8;
                    out.push((accumulator >> bit_count) as u8);
                }
            }
        }
    }

    if padding > 2 {
        return None;
    }
    Some(out)
}

/// Six-bit value of one base64 symbol.
fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// FNV-1a fingerprint, extended with the length, used as a cheap cache key.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^ (bytes.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// Make sure the data-URI image loader is registered on this context.
fn ensure_loader(ctx: &egui::Context) {
    if !ctx.is_loader_installed(LOADER_ID) {
        ctx.add_image_loader(Arc::new(SvgDataUriLoader::default()));
    }
}

/// [`ImageLoader`] for `data:image/svg+xml;base64,…` URIs.
///
/// The payload is decoded in memory and published to the context as
/// `bytes://`-addressed bytes, which are then rasterized by `egui_extras`' SVG
/// loader (which only accepts URIs ending in `.svg`). The derived byte URI is a
/// pure function of the payload, so repeated frames and repeated loads of the
/// same page share one cache entry.
#[derive(Default)]
struct SvgDataUriLoader {
    svg: egui_extras::loaders::svg_loader::SvgLoader,
}

/// Byte URI a decoded SVG page is published under.
///
/// The key is a pure function of the payload, so identical pages share one
/// `bytes://` entry and repeated loads always publish to the same place.
fn svg_bytes_key_for(bytes: &[u8]) -> String {
    format!("bytes://mdx-gui/svg/{:016x}.svg", fingerprint(bytes))
}

/// Byte URI that a data URI publishes its decoded SVG under.
fn svg_bytes_key(uri: &str) -> Option<String> {
    let payload = uri.strip_prefix(DATA_URI_PREFIX)?;
    let bytes = base64_decode(payload)?;
    Some(svg_bytes_key_for(&bytes))
}

impl ImageLoader for SvgDataUriLoader {
    fn id(&self) -> &str {
        LOADER_ID
    }

    fn load(&self, ctx: &egui::Context, uri: &str, size_hint: SizeHint) -> ImageLoadResult {
        let payload = match uri.strip_prefix(DATA_URI_PREFIX) {
            Some(payload) => payload,
            None => return Err(LoadError::NotSupported),
        };
        let bytes = match base64_decode(payload) {
            Some(bytes) => bytes,
            None => {
                return Err(LoadError::Loading(
                    "invalid base64 payload in SVG data URI".to_owned(),
                ));
            }
        };

        let key = svg_bytes_key_for(&bytes);
        ctx.include_bytes(key.clone(), bytes);
        self.svg.load(ctx, &key, size_hint)
    }

    fn forget(&self, uri: &str) {
        if let Some(key) = svg_bytes_key(uri) {
            self.svg.forget(&key);
        }
    }

    fn forget_all(&self) {
        self.svg.forget_all();
    }

    fn byte_size(&self) -> usize {
        self.svg.byte_size()
    }

    fn end_pass(&self, pass_index: u64) {
        self.svg.end_pass(pass_index);
    }
}

#[cfg(test)]
mod tests {
    use egui::load::ImagePoll;

    use super::*;

    /// A page shaped like the ones `typst-svg` writes.
    const PAGE_SVG: &str = concat!(
        r#"<svg viewBox="0 0 595.276 841.89" width="595.276pt" height="841.89pt" "#,
        r#"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">"#,
        r#"<path d="M 10 10 L 20 20"/></svg>"#,
    );

    fn approx(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    #[test]
    fn base64_matches_the_rfc4648_vectors() {
        let vectors: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];

        for (bytes, encoded) in vectors {
            assert_eq!(base64_encode(bytes), encoded);
            assert_eq!(base64_decode(encoded).as_deref(), Some(bytes));
        }
    }

    #[test]
    fn base64_rejects_junk_but_accepts_line_breaks() {
        assert!(base64_decode("Zm9v*").is_none());
        assert!(base64_decode("Zm=9v").is_none());
        assert!(base64_decode("====").is_none());
        assert_eq!(
            base64_decode("Zm9v\nYmFy").unwrap_or_default(),
            b"foobar".to_vec()
        );
    }

    #[test]
    fn data_uri_round_trips_through_base64() {
        let uri = svg_data_uri(PAGE_SVG);
        let payload = uri.strip_prefix(DATA_URI_PREFIX).unwrap_or("");
        assert!(uri.starts_with("data:image/svg+xml;base64,"));
        assert_eq!(
            base64_decode(payload).unwrap_or_default(),
            PAGE_SVG.as_bytes()
        );
    }

    #[test]
    fn intrinsic_size_reads_absolute_attributes() {
        let size = svg_intrinsic_size(PAGE_SVG);
        assert!(matches!(size, Some(size) if approx(size.x, 595.276) && approx(size.y, 841.89)));
    }

    #[test]
    fn intrinsic_size_falls_back_to_the_view_box() {
        let percent = r#"<svg width="100%" height="100%" viewBox="0 0 200 100"><rect/></svg>"#;
        assert!(
            matches!(svg_intrinsic_size(percent), Some(size) if approx(size.x, 200.0) && approx(size.y, 100.0))
        );

        let similar_attribute = r#"<svg stroke-width="8" viewBox="0,0,300,150"><g/></svg>"#;
        assert!(
            matches!(svg_intrinsic_size(similar_attribute), Some(size) if approx(size.x, 300.0) && approx(size.y, 150.0))
        );

        let single_quoted = r#"<svg width='100' height='50'><g/></svg>"#;
        assert!(
            matches!(svg_intrinsic_size(single_quoted), Some(size) if approx(size.x, 100.0) && approx(size.y, 50.0))
        );

        let unterminated = r#"<svg width="100 height="50"><g/></svg>"#;
        assert!(svg_intrinsic_size(unterminated).is_none());

        assert!(svg_intrinsic_size("<svg xmlns=\"http://www.w3.org/2000/svg\">x</svg>").is_none());
    }

    #[test]
    fn lengths_are_converted_to_points() {
        assert!(matches!(parse_length("595.276pt"), Some(value) if approx(value, 595.276)));
        assert!(matches!(parse_length("210mm"), Some(value) if approx(value, 595.2756)));
        assert!(matches!(parse_length("8.5in"), Some(value) if approx(value, 612.0)));
        assert!(matches!(parse_length("1pc"), Some(value) if approx(value, 12.0)));
        assert!(parse_length("100%").is_none());
        assert!(parse_length("2em").is_none());
        assert!(parse_length("").is_none());
        assert!(parse_length("0").is_none());
    }

    #[test]
    fn zoom_is_clamped_into_the_supported_range() {
        assert!(approx(normalize_zoom(DEFAULT_ZOOM), DEFAULT_ZOOM));
        assert!(approx(normalize_zoom(10.0), MAX_ZOOM));
        assert!(approx(normalize_zoom(0.001), MIN_ZOOM));
        assert!(approx(normalize_zoom(f32::NAN), DEFAULT_ZOOM));
        assert!(approx(normalize_zoom(f32::INFINITY), DEFAULT_ZOOM));
    }

    #[test]
    fn fit_width_scales_the_page_to_the_pane() {
        assert!(approx(fit_scale(595.276, 1190.552), 2.0));
        assert!(approx(fit_scale(0.0, 500.0), DEFAULT_ZOOM));
        assert!(approx(fit_scale(500.0, 0.0), DEFAULT_ZOOM));
    }

    #[test]
    fn display_scale_prefers_fit_width_over_zoom() {
        let page = Some(vec2(600.0, 800.0));
        assert!(
            matches!(display_scale(page, 300.0, false, 1.5), Some(scale) if approx(scale, 1.5))
        );
        assert!(matches!(display_scale(page, 300.0, true, 1.5), Some(scale) if approx(scale, 0.5)));
        assert!(
            matches!(display_scale(page, 300.0, false, 99.0), Some(scale) if approx(scale, MAX_ZOOM))
        );
        assert!(display_scale(Some(vec2(0.0, 12.0)), 300.0, true, 1.0).is_none());
        assert!(display_scale(None, 300.0, true, 1.0).is_none());
    }

    #[test]
    fn page_labels_are_one_based() {
        assert_eq!(page_label(0, 7), "Page 1 / 7");
        assert_eq!(page_label(6, 7), "Page 7 / 7");
        assert_eq!(page_count_label(1), "1 page");
        assert_eq!(page_count_label(3), "3 pages");
    }

    #[test]
    fn pane_renders_pages_and_placeholders_without_a_backend() {
        let pages = [PAGE_SVG.to_owned()];
        let mut zoom: f32 = DEFAULT_ZOOM;

        egui::__run_test_ui(|ui| {
            show(ui, &[], &mut zoom);
            show(ui, &pages, &mut zoom);
            // Same page again: the cached data URI must not disturb rendering.
            show(ui, &pages, &mut zoom);
        });

        let mut zoomed: f32 = 4.0;
        egui::__run_test_ui(|ui| {
            show(ui, &pages, &mut zoomed);
        });
        assert!(approx(zoomed, MAX_ZOOM));
    }

    #[test]
    fn loader_is_registered_once_and_rasterizes_data_uris() {
        let ctx = egui::Context::default();
        ensure_loader(&ctx);
        ensure_loader(&ctx);
        assert!(ctx.is_loader_installed(LOADER_ID));

        let loader = SvgDataUriLoader::default();
        assert!(matches!(
            loader.load(&ctx, "file:///page.svg", SizeHint::default()),
            Err(LoadError::NotSupported)
        ));

        let svg = concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10pt" height="20pt" "#,
            r##"viewBox="0 0 10 20"><rect x="0" y="0" width="10" height="20" fill="#000"/></svg>"##,
        );
        let hint = SizeHint::Size {
            width: 10,
            height: 20,
            maintain_aspect_ratio: true,
        };
        let rasterized = match loader.load(&ctx, &svg_data_uri(svg), hint) {
            Ok(ImagePoll::Ready { image }) => Some([image.width(), image.height()]),
            _ => None,
        };
        assert_eq!(rasterized, Some([10, 20]));
    }

    #[test]
    fn data_uri_cache_reuses_unchanged_pages() {
        let ctx = egui::Context::default();
        let first = page_data_uri(&ctx, 0, PAGE_SVG);
        let second = page_data_uri(&ctx, 0, PAGE_SVG);
        assert!(Arc::ptr_eq(&first, &second));

        let edited = page_data_uri(&ctx, 0, "<svg width=\"10\" height=\"10\"/>");
        assert!(!Arc::ptr_eq(&first, &edited));
        assert!(edited.starts_with(DATA_URI_PREFIX));

        // A neighbouring page keeps its own cache entry.
        let other = page_data_uri(&ctx, 1, PAGE_SVG);
        assert_eq!(&*other, &*first);
    }

    #[test]
    fn byte_keys_are_derived_from_the_payload() {
        let uri = svg_data_uri(PAGE_SVG);
        let key = svg_bytes_key(&uri);
        assert!(
            matches!(&key, Some(key) if key.starts_with("bytes://mdx-gui/svg/") && key.ends_with(".svg"))
        );
        assert_eq!(key, svg_bytes_key(&svg_data_uri(PAGE_SVG)));
        assert_ne!(
            key,
            svg_bytes_key(&svg_data_uri("<svg width=\"9\" height=\"9\"/>"))
        );
        assert!(svg_bytes_key("file:///page.svg").is_none());
        assert!(svg_bytes_key("data:image/svg+xml;base64,****").is_none());
    }

    #[test]
    fn raster_refresh_only_follows_a_real_size_change() {
        assert!(!raster_refresh_needed(None, 800.0));
        assert!(!raster_refresh_needed(Some(800.0), 800.0));
        assert!(!raster_refresh_needed(Some(800.0), 900.0));
        assert!(raster_refresh_needed(Some(800.0), 1100.0));
        assert!(raster_refresh_needed(Some(1100.0), 800.0));
        assert!(!raster_refresh_needed(Some(0.0), 0.0));
        assert!(!raster_refresh_needed(Some(f32::NAN), 800.0));
    }

    #[test]
    fn forgetting_a_page_leaves_the_context_usable() {
        let ctx = egui::Context::default();
        ensure_loader(&ctx);
        let uri = page_data_uri(&ctx, 0, PAGE_SVG);
        refresh_raster(&ctx, 0, &uri, 1200.0);
        refresh_raster(&ctx, 0, &uri, 1204.0);
        forget_page(&ctx, &uri);
        assert!(page_data_uri(&ctx, 0, PAGE_SVG).starts_with(DATA_URI_PREFIX));
    }

    #[test]
    fn fit_width_toggle_round_trips_through_context_memory() {
        let ctx = egui::Context::default();
        assert!(fit_width_enabled(&ctx));
        set_fit_width(&ctx, false);
        assert!(!fit_width_enabled(&ctx));
    }
}

//! Source pane: a monospace `TextEdit` with a line-number gutter and a band
//! behind the caret's line.
//!
//! Alignment: the pane lays the buffer out itself ([`layout_source`]) and hands
//! the very same shape to the `TextEdit` through its `layouter` hook. The gutter
//! and the band are painted from that galley *before* the widget draws its text,
//! so both stay aligned with the rows egui renders — including the extra rows a
//! wrapped line produces, which get exactly one number.
//!
//! The pane does no I/O, keeps no globals and reads no clock: it is a function
//! of the buffer it is handed and [`EditorState`].

use std::sync::Arc;

use egui::text::{CCursor, LayoutJob};
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Galley, Layout, Painter, Rect, ScrollArea,
    Sense, Stroke, TextBuffer, TextEdit, Ui, UiBuilder, Vec2, pos2,
};

/// Smallest width the text column is allowed to shrink to, in points.
const MIN_TEXT_WIDTH: f32 = 32.0;

/// Horizontal padding reserved inside the line-number gutter, in points.
const GUTTER_PADDING: f32 = 12.0;

/// Point size of the source column.
const FONT_SIZE: f32 = 13.0;

/// Editor preferences plus the caret bookkeeping the status bar reads.
#[derive(Debug, Clone)]
pub struct EditorState {
    /// Draw the line-number gutter.
    pub show_line_numbers: bool,
    /// Draw a band behind the caret's line.
    pub highlight_current_line: bool,

    /// 1-based caret line, as measured on the last frame.
    caret_line: usize,
    /// 1-based caret column, as measured on the last frame.
    caret_column: usize,
    /// Vertical distance between the rectangle reserved for the text column and
    /// the top of the galley the widget actually painted. Zero in practice; the
    /// correction keeps the gutter honest if a theme changes the widget's inner
    /// margin.
    origin_fix: f32,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            show_line_numbers: true,
            highlight_current_line: true,
            caret_line: 1,
            caret_column: 1,
            origin_fix: 0.0,
        }
    }
}

impl EditorState {
    /// 1-based line of the caret, as of the last drawn frame.
    #[must_use]
    pub fn caret_line(&self) -> usize {
        self.caret_line
    }

    /// 1-based column of the caret, as of the last drawn frame.
    #[must_use]
    pub fn caret_column(&self) -> usize {
        self.caret_column
    }

    /// Draw the source pane for `text`.
    ///
    /// Returns `true` when the user changed the buffer during this frame.
    pub fn show(&mut self, ui: &mut Ui, text: &mut String) -> bool {
        let font_id = FontId::monospace(FONT_SIZE);
        // Same metric the widget uses for its rows, so the gutter lines up.
        // API: https://docs.rs/egui/0.37.0/egui/struct.Ui.html#method.fonts_mut
        let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font_id));
        let line_height = row_height + ui.spacing().extra_text_line_spacing;
        let digit_width = ui.fonts_mut(|fonts| fonts.glyph_width(&font_id, '0'));

        let text_color = ui.visuals().text_color();
        let backdrop = ui.visuals().text_edit_bg_color();
        let band = ui.visuals().widgets.hovered.weak_bg_fill;
        let number_color = ui.visuals().weak_text_color();
        let separator = ui.visuals().widgets.noninteractive.bg_stroke;

        let show_numbers = self.show_line_numbers;
        let highlight_line = self.highlight_current_line;
        let caret_line = self.caret_line.max(1);
        let digits = decimal_digits(count_lines(text));
        let spacing = ui.spacing().item_spacing.x;
        let origin_fix = self.origin_fix;

        let mut changed = false;
        let mut caret = None;

        ScrollArea::vertical()
            .id_salt("mdx_source_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    let row_start = ui.cursor().min;
                    let available = ui.available_width();
                    let gutter_width = if show_numbers {
                        GUTTER_PADDING + digits as f32 * digit_width
                    } else {
                        0.0
                    };
                    let text_width = (available - gutter_width - spacing).max(MIN_TEXT_WIDTH);

                    // The galley the widget will paint: same text, font, colour,
                    // line height and wrap width, so it comes back from egui's
                    // galley cache instead of being laid out twice.
                    let galley = layout_source(
                        ui,
                        text.as_str(),
                        &font_id,
                        text_color,
                        line_height,
                        text_width,
                    );
                    let content = Rect::from_min_size(
                        row_start,
                        Vec2::new(available, galley.size().y.max(line_height)),
                    );
                    let text_top = row_start.y + origin_fix;

                    let painter = ui.painter();
                    painter.rect_filled(content, CornerRadius::ZERO, backdrop);
                    if highlight_line {
                        paint_caret_band(painter, &galley, content, text_top, caret_line, band);
                    }
                    if show_numbers {
                        paint_gutter(
                            painter,
                            &galley,
                            text_top,
                            row_start.x + gutter_width,
                            &font_id,
                            number_color,
                            separator,
                        );
                    }

                    if show_numbers {
                        let _ = ui.allocate_exact_size(
                            Vec2::new(gutter_width, content.height()),
                            Sense::hover(),
                        );
                    }

                    let text_rect = Rect::from_min_size(
                        ui.cursor().min,
                        Vec2::new(text_width, content.height()),
                    );
                    // The wrap width is pinned to the width reserved above
                    // rather than taken from the argument: the widget then
                    // paints exactly the galley this frame measured, whatever
                    // width the widget's own sizing works out to. That is what
                    // keeps the gutter honest at every pane width.
                    let mut layouter = |ui: &Ui, buffer: &dyn TextBuffer, _wrap_width: f32| {
                        layout_source(
                            ui,
                            buffer.as_str(),
                            &font_id,
                            text_color,
                            line_height,
                            text_width,
                        )
                    };

                    // `scope_builder` gives the widget exactly `text_rect` and
                    // then advances the parent cursor by that rectangle, which
                    // is what sizes the scroll area's content.
                    // API: https://docs.rs/egui/0.37.0/egui/struct.Ui.html#method.scope_builder
                    let output = ui
                        .scope_builder(
                            UiBuilder::new()
                                .max_rect(text_rect)
                                .layout(Layout::top_down(Align::Min))
                                .id_salt("mdx_source_text"),
                            |ui| {
                                TextEdit::multiline(text)
                                    .id_salt("mdx_source_edit")
                                    .font(font_id.clone())
                                    // No frame and no margin: the pane paints its
                                    // own background, and a zero inner margin is
                                    // what makes `galley_pos` the rectangle's
                                    // top-left corner.
                                    // API: https://docs.rs/egui/0.37.0/egui/struct.Frame.html#associatedconstant.NONE
                                    .frame(Frame::NONE)
                                    .desired_width(text_width)
                                    // One row is enough: the galley sets the
                                    // real height.
                                    .desired_rows(1)
                                    .align(Align2::LEFT_TOP)
                                    // Tab indents instead of moving focus — the
                                    // buffer is Markdown source.
                                    .lock_focus(true)
                                    .layouter(&mut layouter)
                                    .show(ui)
                            },
                        )
                        .inner;

                    self.origin_fix = output.galley_pos.y - text_rect.min.y;
                    // `WidgetAtomResponse` derefs to the widget's `Response`; go
                    // through the field so the change flag is unambiguous.
                    changed |= output.response.response.changed();
                    if let Some(range) = output.cursor_range {
                        caret = Some(caret_position(
                            &output.galley,
                            CCursor::new(range.primary.index),
                        ));
                    }
                });
            });

        if let Some((line, column)) = caret {
            self.caret_line = line;
            self.caret_column = column;
        }

        changed
    }
}

/// Lay `text` out the way the source pane paints it.
///
/// Used both for the pre-paint measurements and, through the `TextEdit`
/// layouter hook, for the text itself: identical inputs mean identical galleys,
/// which is the whole basis of the gutter's alignment.
fn layout_source(
    ui: &Ui,
    text: &str,
    font_id: &FontId,
    text_color: Color32,
    line_height: f32,
    wrap_width: f32,
) -> Arc<Galley> {
    // `LayoutJob::simple` breaks on `\n` and at `wrap_width`; `keep_trailing_
    // whitespace` and the explicit line height mirror egui's own default
    // layouter for `TextEdit`.
    let mut job = LayoutJob::simple(text.to_owned(), font_id.clone(), text_color, wrap_width);
    job.keep_trailing_whitespace = true;
    for section in &mut job.sections {
        section.format.line_height = Some(line_height);
    }
    ui.fonts_mut(|fonts| fonts.layout_job(job))
}

/// Paint the numbers of `galley`'s rows inside the gutter ending at
/// `gutter_right`.
///
/// A row gets a number only when it starts a logical line, so a wrapped line is
/// numbered once.
fn paint_gutter(
    painter: &Painter,
    galley: &Galley,
    text_top: f32,
    gutter_right: f32,
    font_id: &FontId,
    number_color: Color32,
    separator: Stroke,
) {
    let numbers_right = gutter_right - GUTTER_PADDING * 0.5;
    let top = painter.clip_rect().top();
    let bottom = painter.clip_rect().bottom();

    let mut line = 1usize;
    let mut starts_line = true;
    for placed in &galley.rows {
        if starts_line {
            let row_top = text_top + placed.pos.y;
            // Culling keeps a 100k-line file from issuing 100k text shapes.
            if row_top + placed.row.size.y >= top && row_top <= bottom {
                painter.text(
                    pos2(numbers_right, row_top),
                    Align2::RIGHT_TOP,
                    line.to_string(),
                    font_id.clone(),
                    number_color,
                );
            }
        }
        starts_line = placed.ends_with_newline;
        if starts_line {
            line += 1;
        }
    }

    painter.vline(gutter_right, top..=bottom, separator);
}

/// Paint a band across `content` behind every row of `caret_line`.
fn paint_caret_band(
    painter: &Painter,
    galley: &Galley,
    content: Rect,
    text_top: f32,
    caret_line: usize,
    band: Color32,
) {
    let mut line = 1usize;
    for placed in &galley.rows {
        if line == caret_line {
            let row_top = text_top + placed.pos.y;
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(content.left(), row_top),
                    pos2(content.right(), row_top + placed.row.size.y),
                ),
                CornerRadius::ZERO,
                band,
            );
        }
        if placed.ends_with_newline {
            line += 1;
        }
    }
}

/// Line and column (both 1-based) of `cursor`, in logical lines: rows produced
/// by wrapping count as part of the line they wrap.
fn caret_position(galley: &Galley, cursor: CCursor) -> (usize, usize) {
    // API: https://docs.rs/epaint/0.37.0/epaint/text/struct.Galley.html#method.layout_from_cursor
    let layout = galley.layout_from_cursor(cursor);
    let row = layout.row.min(galley.rows.len().saturating_sub(1));
    let line = 1 + galley.rows[..row]
        .iter()
        .filter(|placed| placed.ends_with_newline)
        .count();
    (line, layout.column.0 + 1)
}

/// Number of lines in `text`, counting the empty line a trailing newline opens.
fn count_lines(text: &str) -> usize {
    text.split('\n').count().max(1)
}

/// Decimal digits of `value`, at least one.
fn decimal_digits(value: usize) -> usize {
    let mut digits = 1;
    let mut rest = value;
    while rest >= 10 {
        rest /= 10;
        digits += 1;
    }
    digits
}

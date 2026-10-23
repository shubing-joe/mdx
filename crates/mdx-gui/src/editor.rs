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
//! of the buffer it is handed and [`EditorState`]. The one way in from the rest
//! of the shell is [`EditorState::reveal_line`], which the outline and the
//! diagnostics pane use to show a line: the request moves the caret through the
//! widget's own state and scrolls the pane to the line.

use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::widgets::text_edit::TextEditState;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Galley, Id, Layout, Painter, Pos2, Rect,
    ScrollArea, Sense, Stroke, TextBuffer, TextEdit, Ui, UiBuilder, Vec2, pos2,
};

/// Salt of the pane's one `TextEdit`: the widget keeps its caret state under it.
const TEXT_ID_SALT: &str = "mdx_source_edit";

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

    /// Line a reveal asked for, applied by the next [`EditorState::show`].
    pending_reveal: Option<usize>,
    /// Text queued for insertion at the caret (image paste), applied next frame.
    pending_insert: Option<String>,
    /// Line the last reveal landed on, banded until the buffer changes under it.
    revealed_line: Option<usize>,
    /// Id of the pane's `TextEdit`, learned from the first frame it was drawn:
    /// a reveal moves the caret through the widget's state, which is keyed by
    /// that id.
    text_id: Option<Id>,

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
            pending_reveal: None,
            pending_insert: None,
            revealed_line: None,
            text_id: None,
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

    /// Ask the pane to show `line` (1-based): the next [`EditorState::show`]
    /// scrolls to it, moves the caret to its first character and bands it.
    ///
    /// A line past the end of the buffer clamps to the last line, so a stale
    /// line number from an outline or a diagnostic can never scroll the pane
    /// past the text.
    pub fn reveal_line(&mut self, line: usize) {
        self.pending_reveal = Some(line.max(1));
    }

    /// Line the last reveal landed on, if the buffer has not changed since.
    #[must_use]
    pub fn revealed_line(&self) -> Option<usize> {
        self.revealed_line
    }

    /// Insert `snippet` at the caret on the next frame, replacing any selection.
    ///
    /// Used by the image-paste path: the caller has already written the file and
    /// only needs the Markdown reference to land where the caret is.
    pub fn insert_at_caret(&mut self, snippet: String) {
        self.pending_insert = Some(snippet);
    }

    /// Replace the caret's selection (or insert at the caret) with `snippet`.
    ///
    /// Returns `false` and keeps the request queued when the pane has not been
    /// drawn yet and therefore has no widget id to move the caret through.
    fn insert_now(&mut self, ui: &Ui, text: &mut String, snippet: &str) -> bool {
        let Some(editor_id) = self.text_id else {
            self.pending_insert = Some(snippet.to_owned());
            return false;
        };
        let mut state = TextEditState::load(ui.ctx(), editor_id).unwrap_or_default();
        let range = state
            .cursor
            .char_range()
            .unwrap_or_else(|| CCursorRange::one(CCursor::new(text.chars().count())));
        let [start, end] = range.sorted_cursors();
        let start_byte = byte_index(text, start.index.0);
        let end_byte = byte_index(text, end.index.0);
        text.replace_range(start_byte..end_byte, snippet);
        let caret = CCursor::new(start.index.0 + snippet.chars().count());
        state.cursor.set_char_range(Some(CCursorRange::one(caret)));
        state.store(ui.ctx(), editor_id);
        ui.memory_mut(|memory| memory.request_focus(editor_id));
        self.revealed_line = None;
        true
    }

    /// Draw the source pane for `text`.
    ///
    /// Returns `true` when the user changed the buffer during this frame.
    pub fn show(&mut self, ui: &mut Ui, text: &mut String) -> bool {
        // A paste of an image becomes a Markdown reference at the caret; apply
        // it before the buffer is measured so this frame lays out the result.
        let inserted = match self.pending_insert.take() {
            Some(snippet) => self.insert_now(ui, text, &snippet),
            None => false,
        };
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
        let text_lines = count_lines(text);
        let digits = decimal_digits(text_lines);
        let spacing = ui.spacing().item_spacing.x;
        let origin_fix = self.origin_fix;

        // A reveal asked for by the shell: the bands are painted from the line
        // it names, and the caret and the scroll are applied below, once the
        // widget has laid the buffer out. The line stays pending until it has
        // been applied, which needs the widget's id from the previous frame.
        let reveal = self.pending_reveal.map(|line| line.min(text_lines));
        let band_line = reveal.or(self.revealed_line);
        let reveal_band = ui.visuals().selection.bg_fill;

        let mut changed = false;
        let mut caret = None;
        let mut revealed = None;

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
                    if let Some(line) = band_line {
                        paint_line_band(painter, &galley, content, text_top, line, reveal_band);
                    }
                    if highlight_line {
                        paint_line_band(painter, &galley, content, text_top, caret_line, band);
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
                                // A reveal moves the caret through the widget's
                                // own state, keyed by the id the widget was
                                // drawn under last frame.
                                if let (Some(line), Some(editor_id)) = (reveal, self.text_id) {
                                    let index = CCursor::new(line_start_char_index(text, line));
                                    let mut state = TextEditState::load(ui.ctx(), editor_id)
                                        .unwrap_or_default();
                                    state.cursor.set_char_range(Some(CCursorRange::one(index)));
                                    state.store(ui.ctx(), editor_id);
                                    ui.memory_mut(|memory| memory.request_focus(editor_id));
                                    revealed = Some(line);
                                }

                                TextEdit::multiline(text)
                                    .id_salt(TEXT_ID_SALT)
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
                    // The id the next reveal has to move the caret through.
                    self.text_id = Some(output.response.response.id);
                    // `WidgetAtomResponse` derefs to the widget's `Response`; go
                    // through the field so the change flag is unambiguous.
                    changed |= output.response.response.changed();
                    if let Some(range) = output.cursor_range {
                        caret = Some(caret_position(
                            &output.galley,
                            CCursor::new(range.primary.index),
                        ));
                    }

                    if let Some(line) = revealed {
                        // The widget does not follow a caret it did not move
                        // itself, so the pane scrolls the revealed line in: the
                        // enclosing scroll area takes this pass's target.
                        self.pending_reveal = None;
                        self.revealed_line = Some(line);
                        if let Some(rect) =
                            line_rect(&output.galley, output.galley_pos, text_width, line)
                        {
                            ui.scroll_to_rect(rect, Some(Align::Center));
                        }
                    } else if changed {
                        self.revealed_line = None;
                    }
                });
            });

        if let Some((line, column)) = caret {
            self.caret_line = line;
            self.caret_column = column;
        }

        changed || inserted
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

/// Paint a band across `content` behind every row of `line`.
fn paint_line_band(
    painter: &Painter,
    galley: &Galley,
    content: Rect,
    text_top: f32,
    line: usize,
    band: Color32,
) {
    let mut current = 1usize;
    for placed in &galley.rows {
        if current == line {
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
            current += 1;
        }
    }
}

/// Rectangle covered by the rows of `line` in a galley laid out at `origin`,
/// or `None` when the galley has no such line.
///
/// Wrapping means a line can own several rows; the rectangle spans them all, so
/// a revealed line is scrolled to as a whole.
fn line_rect(galley: &Galley, origin: Pos2, width: f32, line: usize) -> Option<Rect> {
    let mut current = 1usize;
    let mut span = None;

    for placed in &galley.rows {
        if current == line {
            let row_top = origin.y + placed.pos.y;
            let row_bottom = row_top + placed.row.size.y;
            span = Some(match span {
                Some((top, _)) => (top, row_bottom),
                None => (row_top, row_bottom),
            });
        }
        if placed.ends_with_newline {
            current += 1;
        }
    }

    let (top, bottom) = span?;
    Some(Rect::from_min_size(
        pos2(origin.x, top),
        Vec2::new(width, bottom - top),
    ))
}

/// Index of the first `char` of `line` (1-based), clamped to the buffer.
///
/// This is what a `CCursor` counts, and what makes a revealed line's caret land
/// on its first character instead of at the end of the buffer.
fn line_start_char_index(text: &str, line: usize) -> usize {
    let preceding_newlines = line.saturating_sub(1);
    let mut seen = 0usize;
    let mut index = 0usize;
    for character in text.chars() {
        if seen >= preceding_newlines {
            break;
        }
        index += 1;
        if character == '\n' {
            seen += 1;
        }
    }
    index
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

/// Byte offset of `char_index` in `text`, clamped to the end of the buffer.
fn byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_index_clamps_and_counts_chars() {
        assert_eq!(byte_index("abc", 0), 0);
        assert_eq!(byte_index("abc", 2), 2);
        assert_eq!(byte_index("你a", 1), 3);
        assert_eq!(byte_index("abc", 99), 3);
    }

    #[test]
    fn insert_at_caret_lands_in_the_buffer() {
        let mut state = EditorState::default();
        let mut text = String::from("hello");
        egui::__run_test_ui(|ui| {
            state.show(ui, &mut text);
        });
        state.insert_at_caret("![](assets/x.png)".to_owned());
        egui::__run_test_ui(|ui| {
            state.show(ui, &mut text);
        });
        assert!(text.ends_with("![](assets/x.png)"), "{text}");
        assert!(text.starts_with("hello"), "{text}");
    }
}

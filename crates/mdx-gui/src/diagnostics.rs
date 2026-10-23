//! Diagnostics list pane.
//!
//! [`show`] renders a report's [`Diagnostic`]s as clickable rows: a severity dot
//! in the severity's colour, the stable `CODE:line:column` position, the message
//! and — when the diagnostic was raised for an asset — a chip naming that asset.
//! Clicking anywhere on a row copies the diagnostic's one-line rendering to the
//! system clipboard.
//!
//! A toolbar above the rows narrows them: a filter box matching the code, the
//! severity or the message text, one toggleable chip per severity showing how
//! many diagnostics carry it, a `Copy all` button that puts every listed row on
//! the clipboard and the number of rows currently shown.
//!
//! The pane is pure UI over the slice it is given: it never reads files, never
//! consults a clock, and the only state it keeps is the filter, in egui's
//! temporary memory. An empty slice draws an empty state instead of a list.

use egui::{Color32, CornerRadius, Id, Margin, Response, RichText, Sense, Ui, Vec2};
use mdx_core::{Diagnostic, Severity};

/// Diameter of the severity dot drawn in front of every row, in points.
const DOT_SIZE: f32 = 8.0;

/// Width of the filter box, in points.
const FILTER_WIDTH: f32 = 240.0;

/// Which severities the pane lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeverityFilter {
    error: bool,
    warning: bool,
    info: bool,
}

impl SeverityFilter {
    /// Every severity enabled: the pane's default.
    const fn all() -> Self {
        Self {
            error: true,
            warning: true,
            info: true,
        }
    }

    /// Whether diagnostics of `severity` are listed.
    const fn allows(self, severity: Severity) -> bool {
        match severity {
            Severity::Error => self.error,
            Severity::Warning => self.warning,
            Severity::Info => self.info,
        }
    }

    /// The flag of one severity, so a chip can toggle it in place.
    fn slot(&mut self, severity: Severity) -> &mut bool {
        match severity {
            Severity::Error => &mut self.error,
            Severity::Warning => &mut self.warning,
            Severity::Info => &mut self.info,
        }
    }
}

impl Default for SeverityFilter {
    /// A fresh filter lists every severity; hiding one is the user's choice.
    fn default() -> Self {
        Self::all()
    }
}

/// The pane's active filter: the query text plus the enabled severities.
#[derive(Debug, Clone, Default)]
struct Filter {
    query: String,
    severities: SeverityFilter,
}

impl Filter {
    /// Whether `diagnostic` passes the filter.
    ///
    /// The query is trimmed first, so a whitespace-only query filters nothing,
    /// and is then matched case-insensitively against the code, the severity
    /// name, the message and the asset name.
    fn matches(&self, diagnostic: &Diagnostic) -> bool {
        if !self.severities.allows(diagnostic.severity) {
            return false;
        }
        let query = self.query.trim();
        if query.is_empty() {
            return true;
        }
        [
            diagnostic.code.as_str(),
            severity_label(diagnostic.severity),
            diagnostic.message.as_str(),
            diagnostic.asset.as_deref().unwrap_or(""),
        ]
        .iter()
        .any(|field| contains_ignore_case(field, query))
    }
}

/// Draw the diagnostics list.
///
/// The rows appear in the order they were given (which is the order the
/// pipeline produced them) and are narrowed by the pane's filter. Nothing is
/// drawn but the empty state when `diags` is empty.
pub fn show(ui: &mut Ui, diags: &[Diagnostic]) {
    if diags.is_empty() {
        empty_state(ui, false);
        return;
    }

    let mut filter = filter_state(ui.ctx());

    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut filter.query)
                .hint_text("filter: code, severity or text")
                .desired_width(FILTER_WIDTH),
        );
        severity_chips(ui, &mut filter.severities, diags);
    });

    let visible: Vec<&Diagnostic> = diags.iter().filter(|d| filter.matches(d)).collect();
    set_filter_state(ui.ctx(), &filter);

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(row_count_label(visible.len(), diags.len()))
                .weak()
                .monospace(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            copy_all_button(ui, &visible);
        });
    });
    ui.separator();

    if visible.is_empty() {
        empty_state(ui, true);
        return;
    }

    for (index, diagnostic) in visible.iter().enumerate() {
        row(ui, diagnostic);
        if index + 1 < visible.len() {
            ui.separator();
        }
    }
}

/// The filter left over from the previous frame, or the pass-through default.
fn filter_state(ctx: &egui::Context) -> Filter {
    ctx.data(|data| data.get_temp::<Filter>(filter_id()).unwrap_or_default())
}

/// Remember the filter for the next frame.
fn set_filter_state(ctx: &egui::Context, filter: &Filter) {
    ctx.data_mut(|data| data.insert_temp(filter_id(), filter.clone()));
}

/// Id of the temporary filter state.
fn filter_id() -> Id {
    Id::unique("mdx-gui::diagnostics::filter")
}

/// Colour that marks a diagnostic of the given severity.
#[must_use]
pub fn severity_color(severity: Severity) -> Color32 {
    match severity {
        Severity::Error => Color32::from_rgb(208, 72, 72),
        Severity::Warning => Color32::from_rgb(226, 160, 40),
        Severity::Info => Color32::from_rgb(86, 148, 214),
    }
}

/// Lower-case name of a severity, used in tooltips and summaries.
#[must_use]
pub fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// Exact text a click on a diagnostic's row puts on the clipboard.
#[must_use]
pub fn clipboard_line(diagnostic: &Diagnostic) -> String {
    diagnostic.to_string()
}

/// One diagnostic row; returns the row's clickable response.
fn row(ui: &mut Ui, diagnostic: &Diagnostic) -> Response {
    let color = severity_color(diagnostic.severity);
    let line = clipboard_line(diagnostic);

    let response = egui::Frame::new()
        .inner_margin(Margin::symmetric(4, 3))
        .corner_radius(CornerRadius::same(3))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                severity_dot(ui, diagnostic.severity);
                ui.label(
                    RichText::new(code_text(diagnostic))
                        .color(color)
                        .monospace()
                        .small(),
                );
                ui.label(RichText::new(diagnostic.message.as_str()));
                if let Some(asset) = diagnostic.asset.as_deref() {
                    asset_chip(ui, asset);
                }
            });
        })
        .response;

    let response = response
        .interact(Sense::click())
        .on_hover_text(format!("Click to copy: {line}"));
    if response.clicked() {
        ui.ctx().copy_text(line);
    }
    response
}

/// A filled circle in the severity's colour, with the severity as its tooltip.
fn severity_dot(ui: &mut Ui, severity: Severity) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(DOT_SIZE), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), DOT_SIZE * 0.5, severity_color(severity));
    response.on_hover_text(severity_label(severity));
}

/// Chip naming the asset a diagnostic was raised for.
fn asset_chip(ui: &mut Ui, asset: &str) {
    let visuals = ui.visuals().clone();
    egui::Frame::new()
        .fill(visuals.widgets.noninteractive.bg_fill)
        .stroke(visuals.widgets.noninteractive.bg_stroke)
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(4, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(asset).small().monospace());
        })
        .response
        .on_hover_text("Asset this diagnostic was raised for");
}

/// Toggleable chip per severity: its colour, its count in the whole list, and
/// whether that severity is currently listed.
fn severity_chips(ui: &mut Ui, filter: &mut SeverityFilter, diags: &[Diagnostic]) {
    let (errors, warnings, infos) = counts(diags);

    for (count, severity) in [
        (errors, Severity::Error),
        (warnings, Severity::Warning),
        (infos, Severity::Info),
    ] {
        let label = count_label(count, severity);
        let text = if filter.allows(severity) {
            RichText::new(label).color(severity_color(severity))
        } else {
            RichText::new(label).weak()
        };
        let hint = match severity {
            Severity::Error => "Show or hide errors",
            Severity::Warning => "Show or hide warnings",
            Severity::Info => "Show or hide info",
        };
        ui.toggle_value(filter.slot(severity), text)
            .on_hover_text(hint);
    }
}

/// `Copy all` — puts every listed row on the clipboard, one per line.
fn copy_all_button(ui: &mut Ui, visible: &[&Diagnostic]) {
    let response = ui
        .add_enabled(!visible.is_empty(), egui::Button::new("Copy all"))
        .on_hover_text("Copy every listed diagnostic, one per line");
    if response.clicked() {
        ui.ctx().copy_text(clipboard_all(visible));
    }
}

/// Everything listed, rendered as one line per diagnostic.
fn clipboard_all(visible: &[&Diagnostic]) -> String {
    let mut text = String::new();
    for (index, diagnostic) in visible.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        text.push_str(&clipboard_line(diagnostic));
    }
    text
}

/// Whether `haystack` contains `needle`, folding ASCII letters' case.
///
/// Non-ASCII text is compared byte-exactly: a filter box over codes, paths and
/// messages is expected to fold ASCII case, and folding the rest would need a
/// per-frame allocation (and a Unicode case table) for no real gain.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

/// `7 rows`, or `3 of 12 shown` while the filter hides rows.
fn row_count_label(shown: usize, total: usize) -> String {
    if shown == total {
        rows_label(total)
    } else {
        format!("{shown} of {total} shown")
    }
}

/// `0 rows` / `1 row` / `7 rows`.
fn rows_label(total: usize) -> String {
    if total == 1 {
        "1 row".to_owned()
    } else {
        format!("{total} rows")
    }
}

/// Body drawn when there is nothing to report, either because nothing was
/// reported or because the filter hides all of it.
fn empty_state(ui: &mut Ui, filtered: bool) {
    let hint = if filtered {
        "No row matches the current filter."
    } else {
        "Nothing to report from the last run."
    };

    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("No diagnostics").strong());
        ui.label(RichText::new(hint).weak());
    });
}

/// `E_PARSE:3:5`, `E_PARSE:3` or bare `E_PARSE` when the position is unknown.
fn code_text(diagnostic: &Diagnostic) -> String {
    let code = diagnostic.code.as_str();
    match (diagnostic.line, diagnostic.column) {
        (Some(line), Some(column)) => format!("{code}:{line}:{column}"),
        (Some(line), None) => format!("{code}:{line}"),
        _ => code.to_owned(),
    }
}

/// `2 errors` / `1 warning` / `3 info`.
fn count_label(count: usize, severity: Severity) -> String {
    let plural = match severity {
        Severity::Error => "errors",
        Severity::Warning => "warnings",
        Severity::Info => "info",
    };
    if count == 1 {
        format!("1 {}", severity_label(severity))
    } else {
        format!("{count} {plural}")
    }
}

/// Diagnostics per severity as `(errors, warnings, infos)`.
fn counts(diags: &[Diagnostic]) -> (usize, usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    let mut infos = 0;

    for diagnostic in diags {
        match diagnostic.severity {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
            Severity::Info => infos += 1,
        }
    }

    (errors, warnings, infos)
}

#[cfg(test)]
mod tests {
    use mdx_core::DiagCode;

    use super::*;

    fn info(message: &str) -> Diagnostic {
        Diagnostic {
            code: DiagCode::Config,
            severity: Severity::Info,
            message: message.to_owned(),
            line: None,
            column: None,
            asset: None,
        }
    }

    fn sample() -> Vec<Diagnostic> {
        vec![
            Diagnostic::error(DiagCode::Parse, "unexpected closing fence").at(3, 5),
            Diagnostic::warning(DiagCode::AssetMissing, "image not found")
                .for_asset("img-0a1b2c3d4e5f"),
            info("using the default paper size"),
        ]
    }

    /// A filter with only `query` set, everything else at its default.
    fn filtering(query: &str) -> Filter {
        Filter {
            query: query.to_owned(),
            ..Filter::default()
        }
    }

    #[test]
    fn severities_get_distinct_colours() {
        let error = severity_color(Severity::Error);
        let warning = severity_color(Severity::Warning);
        let info = severity_color(Severity::Info);
        assert_ne!(error, warning);
        assert_ne!(warning, info);
        assert_ne!(error, info);
    }

    #[test]
    fn code_text_covers_every_position_shape() {
        let positioned = Diagnostic::error(DiagCode::Parse, "boom").at(3, 5);
        assert_eq!(code_text(&positioned), "E_PARSE:3:5");

        let mut line_only = Diagnostic::error(DiagCode::DiagramRender, "boom");
        line_only.line = Some(12);
        assert_eq!(code_text(&line_only), "E_DIAGRAM_RENDER:12");

        let mut bare = Diagnostic::warning(DiagCode::FrontMatter, "boom");
        bare.line = Some(1);
        bare.column = None;
        assert_eq!(code_text(&bare), "W_FRONTMATTER:1");
        assert_eq!(
            code_text(&Diagnostic::warning(DiagCode::FrontMatter, "boom")),
            "W_FRONTMATTER"
        );
        assert_eq!(code_text(&info("note")), "E_CONFIG");
    }

    #[test]
    fn clipboard_line_matches_the_diagnostic_display() {
        let diagnostic = Diagnostic::error(DiagCode::Parse, "boom").at(3, 5);
        assert_eq!(clipboard_line(&diagnostic), "E_PARSE:3:5 boom");
        assert_eq!(clipboard_line(&diagnostic), diagnostic.to_string());
        assert_eq!(clipboard_line(&info("note")), "E_CONFIG note");
    }

    #[test]
    fn counts_and_labels_follow_plural_rules() {
        let diags = sample();
        assert_eq!(counts(&diags), (1, 1, 1));
        assert_eq!(counts(&[]), (0, 0, 0));

        assert_eq!(count_label(1, Severity::Error), "1 error");
        assert_eq!(count_label(2, Severity::Error), "2 errors");
        assert_eq!(count_label(1, Severity::Warning), "1 warning");
        assert_eq!(count_label(4, Severity::Warning), "4 warnings");
        assert_eq!(count_label(0, Severity::Info), "0 info");
        assert_eq!(count_label(3, Severity::Info), "3 info");
    }

    #[test]
    fn severity_labels_are_stable() {
        assert_eq!(severity_label(Severity::Error), "error");
        assert_eq!(severity_label(Severity::Warning), "warning");
        assert_eq!(severity_label(Severity::Info), "info");
    }

    #[test]
    fn substring_matching_folds_ascii_case_only() {
        assert!(contains_ignore_case("E_PARSE:3:5", "e_parse"));
        assert!(contains_ignore_case("image not found", "NOT FOUND"));
        assert!(contains_ignore_case("anything", ""));
        assert!(!contains_ignore_case("image", "images"));
        assert!(!contains_ignore_case("", "x"));
        // Non-ASCII text is compared byte-exactly, so only ASCII case folds.
        assert!(contains_ignore_case("Grüße", "Grüße"));
        assert!(!contains_ignore_case("Grüße", "grÜße"));
    }

    #[test]
    fn a_default_filter_lists_everything() {
        let filter = Filter::default();
        assert_eq!(filter.query, "");
        assert!(filter.severities.allows(Severity::Error));
        for diagnostic in sample() {
            assert!(filter.matches(&diagnostic));
        }
        assert!(filter.matches(&Diagnostic::error(DiagCode::Io, "anything at all")));
    }

    #[test]
    fn the_query_matches_code_severity_message_and_asset() {
        let diags = sample();
        let error = &diags[0];
        let warning = &diags[1];

        assert!(
            filtering("   ").matches(error),
            "a blank query filters nothing"
        );

        assert!(filtering("e_parse").matches(error));
        assert!(filtering("closing").matches(error));
        assert!(!filtering("closing").matches(warning));

        assert!(filtering("WARNING").matches(warning));
        assert!(!filtering("WARNING").matches(error));

        assert!(filtering("img-0A1B").matches(warning));
        assert!(!filtering("img-0A1B").matches(error));

        assert!(!filtering("no such diagnostic").matches(error));
    }

    #[test]
    fn disabled_severities_hide_their_rows() {
        let diags = sample();
        let filter = Filter {
            severities: SeverityFilter {
                warning: false,
                ..SeverityFilter::all()
            },
            ..Filter::default()
        };

        assert!(filter.matches(&diags[0]));
        assert!(!filter.matches(&diags[1]));
        assert!(filter.matches(&diags[2]));

        let off = SeverityFilter {
            error: false,
            warning: false,
            info: false,
        };
        for diagnostic in &diags {
            assert!(!off.allows(diagnostic.severity));
        }

        let mut slots = SeverityFilter::all();
        *slots.slot(Severity::Info) = false;
        assert!(!slots.allows(Severity::Info));
        assert!(slots.allows(Severity::Error));
        assert!(slots.allows(Severity::Warning));
    }

    #[test]
    fn row_counts_follow_plural_rules() {
        assert_eq!(row_count_label(0, 0), "0 rows");
        assert_eq!(row_count_label(1, 1), "1 row");
        assert_eq!(row_count_label(4, 4), "4 rows");
        assert_eq!(row_count_label(0, 3), "0 of 3 shown");
        assert_eq!(row_count_label(2, 5), "2 of 5 shown");
    }

    #[test]
    fn copying_all_joins_the_listed_rows_with_newlines() {
        let diags = sample();
        let all: Vec<&Diagnostic> = diags.iter().collect();

        assert_eq!(clipboard_all(&[]), "");
        assert_eq!(
            clipboard_all(&[all[0]]),
            "E_PARSE:3:5 unexpected closing fence"
        );
        assert_eq!(
            clipboard_all(&all),
            concat!(
                "E_PARSE:3:5 unexpected closing fence\n",
                "E_ASSET_MISSING image not found\n",
                "E_CONFIG using the default paper size",
            )
        );
    }

    #[test]
    fn the_filter_round_trips_through_context_memory() {
        let ctx = egui::Context::default();
        assert_eq!(filter_state(&ctx).query, "");
        assert!(filter_state(&ctx).severities.allows(Severity::Warning));

        let mut filter = filter_state(&ctx);
        filter.query = "boom".to_owned();
        filter.severities.info = false;
        set_filter_state(&ctx, &filter);

        let restored = filter_state(&ctx);
        assert_eq!(restored.query, "boom");
        assert!(!restored.severities.info);
        assert!(restored.severities.allows(Severity::Error));
    }

    #[test]
    fn empty_and_populated_lists_both_render() {
        egui::__run_test_ui(|ui| {
            show(ui, &[]);
            show(ui, &sample());
            show(
                ui,
                &[
                    Diagnostic::error(DiagCode::AssetOffline, "remote asset blocked")
                        .for_asset("img-0123456789ab")
                        .at(7, 1),
                ],
            );
        });
    }

    #[test]
    fn a_filter_that_hides_every_row_still_renders() {
        egui::__run_test_ui(|ui| {
            show(ui, &sample());

            let mut filter = filter_state(ui.ctx());
            filter.query = "nothing matches this".to_owned();
            set_filter_state(ui.ctx(), &filter);

            // The empty state replaces the rows, and the filter survives.
            show(ui, &sample());
            assert_eq!(filter_state(ui.ctx()).query, "nothing matches this");

            // Clearing the query brings the rows back on the next frame.
            let mut filter = filter_state(ui.ctx());
            filter.query.clear();
            set_filter_state(ui.ctx(), &filter);
            show(ui, &sample());
        });
    }
}

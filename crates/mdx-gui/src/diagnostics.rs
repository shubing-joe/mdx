//! Diagnostics list pane.
//!
//! [`show`] renders a report's [`Diagnostic`]s as clickable rows: a severity dot
//! in the severity's colour, the stable `CODE:line:column` position, the message
//! and — when the diagnostic was raised for an asset — a chip naming that asset.
//! Clicking anywhere on a row copies the diagnostic's one-line rendering to the
//! system clipboard.
//!
//! The pane is pure UI over the slice it is given: it never reads files, never
//! consults a clock and holds no state of its own. An empty slice draws an
//! empty state instead of a list.

use egui::{Color32, CornerRadius, Margin, Response, RichText, Sense, Ui, Vec2};
use mdx_core::{Diagnostic, Severity};

/// Diameter of the severity dot drawn in front of every row, in points.
const DOT_SIZE: f32 = 8.0;

/// Draw the diagnostics list.
///
/// The rows appear in the order they were given (which is the order the
/// pipeline produced them). Nothing is drawn but the empty state when `diags`
/// is empty.
pub fn show(ui: &mut Ui, diags: &[Diagnostic]) {
    if diags.is_empty() {
        empty_state(ui);
        return;
    }

    summary(ui, diags);
    ui.separator();

    for (index, diagnostic) in diags.iter().enumerate() {
        row(ui, diagnostic);
        if index + 1 < diags.len() {
            ui.separator();
        }
    }
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

/// Coloured counts of the diagnostics about to be listed.
fn summary(ui: &mut Ui, diags: &[Diagnostic]) {
    let (errors, warnings, infos) = counts(diags);

    ui.horizontal_wrapped(|ui| {
        for (count, severity) in [
            (errors, Severity::Error),
            (warnings, Severity::Warning),
            (infos, Severity::Info),
        ] {
            ui.label(
                RichText::new(count_label(count, severity))
                    .color(severity_color(severity))
                    .strong(),
            );
        }
    });
}

/// Body drawn when there is nothing to report.
fn empty_state(ui: &mut Ui) {
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("No diagnostics").strong());
        ui.label(RichText::new("Nothing to report from the last run.").weak());
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
}

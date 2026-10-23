//! Integration tests for diagnostic rendering, merging, and JSON encoding.
//!
//! The `E_*`/`W_*` strings are part of the CLI/MCP contract, so they are
//! asserted here verbatim.

use mdx_core::diag::{DiagCode, Diagnostic, Report, Severity};

fn info(code: DiagCode, message: &str) -> Diagnostic {
    Diagnostic {
        code,
        severity: Severity::Info,
        message: message.to_string(),
        line: None,
        column: None,
        asset: None,
    }
}

#[test]
fn display_uses_code_line_column_message() {
    let d = Diagnostic::error(DiagCode::Parse, "unexpected token").at(12, 3);
    assert_eq!(d.to_string(), "E_PARSE:12:3 unexpected token");
}

#[test]
fn display_omits_position_when_absent() {
    let d = Diagnostic::warning(DiagCode::FrontMatter, "no frontmatter");
    assert_eq!(d.to_string(), "W_FRONTMATTER no frontmatter");
}

#[test]
fn display_with_line_only_omits_column() {
    let mut d = Diagnostic::error(DiagCode::Io, "read failed");
    d.line = Some(7);
    assert_eq!(d.to_string(), "E_IO:7 read failed");
}

#[test]
fn diagnostic_is_an_error() {
    fn assert_error<E: std::error::Error>(_: &E) {}
    let d = Diagnostic::error(DiagCode::Typst, "compile failed");
    assert_error(&d);
    assert_eq!(d.to_string(), "E_TYPST compile failed");
}

#[test]
fn is_fatal_only_for_e_codes_with_error_severity() {
    assert!(Diagnostic::error(DiagCode::AssetMissing, "missing").is_fatal());
    assert!(!Diagnostic::warning(DiagCode::AssetRemoteCached, "cached").is_fatal());
    // A warning code whose severity was set to Error is still not fatal.
    assert!(!info(DiagCode::FrontMatter, "note").is_fatal());
    let mut contradictory = Diagnostic::warning(DiagCode::Parse, "odd");
    contradictory.severity = Severity::Error;
    assert!(contradictory.is_fatal());
    // Warning severity never blocks, even with an `E_*` code.
    let mut downgraded = Diagnostic::error(DiagCode::Parse, "downgraded");
    downgraded.severity = Severity::Warning;
    assert!(!downgraded.is_fatal());
}

#[test]
fn summary_counts_by_severity() {
    let mut report = Report::default();
    assert_eq!(report.summary(), (0, 0, 0));

    report.push(Diagnostic::error(DiagCode::Parse, "bad"));
    report.push(Diagnostic::warning(DiagCode::FrontMatter, "meh"));
    report.push(Diagnostic::warning(DiagCode::UnsupportedHtml, "dropped"));
    report.push(info(DiagCode::Io, "note"));

    assert_eq!(report.summary(), (1, 2, 1));
}

#[test]
fn merge_from_keeps_insertion_order() {
    let mut left = Report::default();
    left.push(Diagnostic::error(DiagCode::Parse, "first"));
    left.push(Diagnostic::warning(DiagCode::FrontMatter, "second"));

    let mut right = Report::default();
    right.push(Diagnostic::error(DiagCode::Io, "third"));
    right.push(Diagnostic::warning(DiagCode::UnsupportedHtml, "fourth"));

    left.merge_from(right);

    let rendered: Vec<String> = left.diagnostics.iter().map(ToString::to_string).collect();
    assert_eq!(
        rendered,
        vec![
            "E_PARSE first",
            "W_FRONTMATTER second",
            "E_IO third",
            "W_UNSUPPORTED_HTML fourth",
        ]
    );
    assert_eq!(left.summary(), (2, 2, 0));
    assert!(left.has_errors());

    // Merging an empty report is a no-op.
    let before = left.clone();
    left.merge_from(Report::default());
    assert_eq!(left, before);
}

#[test]
fn json_uses_stable_e_and_w_code_strings() {
    let d = Diagnostic::error(DiagCode::AssetOffline, "remote blocked")
        .at(4, 9)
        .for_asset("https://example.com/a.png");

    let json = serde_json::to_value(&d).expect("diagnostic serializes");
    assert_eq!(json["code"], "E_ASSET_OFFLINE");
    assert_eq!(json["severity"], "error");
    assert_eq!(json["message"], "remote blocked");
    assert_eq!(json["line"], 4);
    assert_eq!(json["column"], 9);
    assert_eq!(json["asset"], "https://example.com/a.png");

    let w = Diagnostic::warning(DiagCode::FrontMatter, "no frontmatter");
    let json = serde_json::to_value(&w).expect("diagnostic serializes");
    assert_eq!(json["code"], "W_FRONTMATTER");
    assert_eq!(json["severity"], "warning");
    assert!(json.get("line").is_none());
    assert!(json.get("column").is_none());
    assert!(json.get("asset").is_none());

    let report_json = serde_json::to_value(&Report {
        diagnostics: vec![d.clone(), w.clone()],
    })
    .expect("report serializes");
    assert_eq!(report_json["diagnostics"][0]["code"], "E_ASSET_OFFLINE");
    assert_eq!(report_json["diagnostics"][1]["code"], "W_FRONTMATTER");
}

#[test]
fn json_round_trips_through_deserialization() {
    let report = Report {
        diagnostics: vec![
            Diagnostic::error(DiagCode::DiagramRender, "render failed").at(2, 1),
            Diagnostic::warning(DiagCode::AssetRemoteCached, "cached").for_asset("a.png"),
        ],
    };
    let encoded = serde_json::to_string(&report).expect("report serializes");
    let decoded: Report = serde_json::from_str(&encoded).expect("report deserializes");
    assert_eq!(decoded, report);
}

#[test]
fn diag_codes_render_exactly_as_declared() {
    let expected = [
        (DiagCode::Parse, "E_PARSE"),
        (DiagCode::AssetMissing, "E_ASSET_MISSING"),
        (DiagCode::AssetUnsupported, "E_ASSET_UNSUPPORTED"),
        (DiagCode::AssetOffline, "E_ASSET_OFFLINE"),
        (DiagCode::MathConvert, "E_MATH_CONVERT"),
        (DiagCode::DiagramParse, "E_DIAGRAM_PARSE"),
        (DiagCode::DiagramRender, "E_DIAGRAM_RENDER"),
        (DiagCode::Typst, "E_TYPST"),
        (DiagCode::Io, "E_IO"),
        (DiagCode::Config, "E_CONFIG"),
        (DiagCode::FrontMatter, "W_FRONTMATTER"),
        (DiagCode::UnsupportedHtml, "W_UNSUPPORTED_HTML"),
        (DiagCode::AssetRemoteCached, "W_ASSET_REMOTE_CACHED"),
    ];
    for (code, text) in expected {
        assert_eq!(code.as_str(), text);
        let json = serde_json::to_value(code).expect("code serializes");
        assert_eq!(json, serde_json::Value::String(text.to_string()));
    }
}

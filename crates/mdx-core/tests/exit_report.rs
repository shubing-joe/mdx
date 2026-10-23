//! Exit-code mapping and `Report` / `Diagnostic` semantics.
//!
//! Owned by slice 1 (`crates/mdx-core/tests/**`) — see `docs/CONTRACT.md`.
//! (Slice 2 owns the UX-helper file `tests/diag.rs`; this file only covers the
//! frozen behaviour of `exit.rs` and `diag.rs` as shipped.)

use mdx_core::{DiagCode, Diagnostic, Report, Severity, exit};

fn report(diags: impl IntoIterator<Item = Diagnostic>) -> Report {
    Report {
        diagnostics: diags.into_iter().collect(),
    }
}

fn error(code: DiagCode) -> Diagnostic {
    Diagnostic::error(code, format!("{} happened", code.as_str()))
}

fn warning(code: DiagCode) -> Diagnostic {
    Diagnostic::warning(code, format!("{} happened", code.as_str()))
}

#[test]
fn exit_code_constants_and_success_cases_are_zero() {
    assert_eq!(exit::OK, 0);
    assert_eq!(exit::FAILED, 1);
    assert_eq!(exit::USAGE, 2);
    assert_eq!(exit::ASSETS, 3);

    assert_eq!(
        exit::exit_code(&Report::default()),
        0,
        "no diagnostics is success"
    );
    assert!(!Report::default().has_errors());

    // Warnings and info never influence the exit code.
    let warnings = report([
        warning(DiagCode::FrontMatter),
        warning(DiagCode::UnsupportedHtml),
    ]);
    assert_eq!(exit::exit_code(&warnings), 0);
    assert!(!warnings.has_errors());

    let infos = report([
        Diagnostic {
            code: DiagCode::AssetRemoteCached,
            severity: Severity::Info,
            message: "cached".into(),
            line: None,
            column: None,
            asset: None,
        },
        warning(DiagCode::AssetRemoteCached),
    ]);
    assert!(!infos.has_errors());
    assert_eq!(
        exit::exit_code(&infos),
        0,
        "asset warnings alone are not asset errors"
    );
    // `Warning`-severity asset diagnostic for a *caching* notice exits 0.
    assert_eq!(
        exit::exit_code(&report([warning(DiagCode::AssetRemoteCached)])),
        0
    );
}

#[test]
fn asset_error_codes_map_to_exit_three() {
    for code in [
        DiagCode::AssetMissing,
        DiagCode::AssetUnsupported,
        DiagCode::AssetOffline,
    ] {
        let r = report([error(code)]);
        assert!(r.has_errors(), "{code:?} error must count as an error");
        assert_eq!(exit::exit_code(&r), 3, "{code:?} is an asset problem");
        assert_eq!(r.errors().count(), 1);
    }

    // All asset problems together still yield 3, and asset codes win over
    // other concurrent errors (CLI contract: 3 = unresolved assets).
    let mixed = report([
        error(DiagCode::AssetOffline),
        error(DiagCode::AssetMissing),
        error(DiagCode::AssetUnsupported),
    ]);
    assert_eq!(mixed.errors().count(), 3);
    assert_eq!(exit::exit_code(&mixed), 3);
    assert_eq!(
        exit::exit_code(&report([
            error(DiagCode::Parse),
            error(DiagCode::AssetMissing)
        ])),
        3,
        "an asset error alongside a parse error still reports unresolved assets"
    );
}

#[test]
fn non_asset_errors_map_to_exit_one() {
    for code in [
        DiagCode::Parse,
        DiagCode::MathConvert,
        DiagCode::DiagramParse,
        DiagCode::DiagramRender,
        DiagCode::Typst,
        DiagCode::Io,
        DiagCode::Config,
        DiagCode::AssetRemoteCached,
    ] {
        assert_eq!(
            exit::exit_code(&report([error(code)])),
            1,
            "{code:?} is a generic failure"
        );
    }

    // A parse failure plus asset warnings is still a plain failure.
    let r = report([
        error(DiagCode::Parse),
        warning(DiagCode::AssetRemoteCached),
        warning(DiagCode::FrontMatter),
    ]);
    assert_eq!(r.errors().count(), 1);
    assert_eq!(exit::exit_code(&r), 1);

    // The early return means a *warning*-severity asset diagnostic alone is not
    // an asset failure: the report succeeds.
    assert_eq!(
        exit::exit_code(&report([warning(DiagCode::AssetMissing)])),
        0
    );
}

#[test]
fn report_predicates_and_error_iterator() {
    let empty = Report::default();
    assert!(!empty.has_errors());
    assert!(!empty.has_code(DiagCode::Parse));
    assert_eq!(empty.errors().count(), 0);
    assert!(empty.diagnostics.is_empty());

    let mut r = Report::default();
    r.push(warning(DiagCode::FrontMatter));
    assert!(!r.has_errors(), "a warning is not an error");
    assert!(r.has_code(DiagCode::FrontMatter));
    assert!(!r.has_code(DiagCode::Io));
    assert_eq!(r.errors().count(), 0);

    r.push(error(DiagCode::Io));
    assert!(r.has_errors());
    assert!(r.has_code(DiagCode::Io));
    assert_eq!(r.errors().count(), 1);
    assert_eq!(
        r.errors().next().map(|d| d.message.as_str()),
        Some("E_IO happened")
    );
    assert_eq!(r.diagnostics.len(), 2, "push appends, order preserved");
    assert_eq!(r.diagnostics[0].severity, Severity::Warning);

    // `extend` moves all diagnostics across, preserving order; the source is consumed.
    let mut base = report([error(DiagCode::Parse)]);
    base.extend(report([
        warning(DiagCode::UnsupportedHtml),
        error(DiagCode::Typst),
    ]));
    assert_eq!(base.diagnostics.len(), 3);
    assert!(base.has_errors() && base.has_code(DiagCode::UnsupportedHtml));
    assert_eq!(
        base.errors().map(|d| d.code).collect::<Vec<_>>(),
        vec![DiagCode::Parse, DiagCode::Typst]
    );
    assert_eq!(exit::exit_code(&base), 1);

    // `has_code` is exact-code, not severity- or family-based.
    let assets = report([
        error(DiagCode::AssetMissing),
        warning(DiagCode::AssetOffline),
    ]);
    assert!(assets.has_code(DiagCode::AssetMissing));
    assert!(assets.has_code(DiagCode::AssetOffline));
    assert!(!assets.has_code(DiagCode::AssetUnsupported));
    assert_eq!(assets.errors().count(), 1);
    assert_eq!(exit::exit_code(&assets), 3);
}

#[test]
fn diagnostic_builder_sets_fields_and_serializes_them() {
    let d = Diagnostic::error(DiagCode::AssetMissing, "image not found")
        .at(12, 7)
        .for_asset("../img/logo.png");

    assert_eq!(d.code, DiagCode::AssetMissing);
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.message, "image not found");
    assert_eq!(d.line, Some(12));
    assert_eq!(d.column, Some(7));
    assert_eq!(d.asset.as_deref(), Some("../img/logo.png"));
    assert_eq!(d.code.as_str(), "E_ASSET_MISSING");

    assert_eq!(
        serde_json::to_value(&d).expect("diagnostic serializes"),
        serde_json::json!({
            "code": "E_ASSET_MISSING",
            "severity": "error",
            "message": "image not found",
            "line": 12,
            "column": 7,
            "asset": "../img/logo.png",
        })
    );

    // Warning severity + builders in the other order; unset optional fields are omitted.
    let w = Diagnostic::warning(DiagCode::FrontMatter, "no front matter").for_asset("logo.png");
    assert_eq!(w.severity, Severity::Warning);
    assert_eq!(w.line, None);
    assert_eq!(w.column, None);
    let wv = serde_json::to_value(&w).expect("warning serializes");
    assert_eq!(wv.get("line"), None, "None line is skipped, not null");
    assert_eq!(wv.get("column"), None);
    assert_eq!(wv["asset"], serde_json::json!("logo.png"));
    assert_eq!(wv.as_object().expect("object").len(), 4);
    assert_eq!(wv["severity"], serde_json::json!("warning"));

    let bare = Diagnostic::error(DiagCode::Parse, "bad markdown");
    assert_eq!(bare.line, None);
    assert_eq!(bare.asset, None);
    assert_eq!(
        serde_json::to_value(&bare).expect("error serializes"),
        serde_json::json!({"code": "E_PARSE", "severity": "error", "message": "bad markdown"})
    );

    // Severity serializes lowercase, matching the JSON above.
    assert_eq!(
        serde_json::to_value(Severity::Error).expect("severity"),
        serde_json::json!("error")
    );
    assert_eq!(
        serde_json::to_value(Severity::Warning).expect("severity"),
        serde_json::json!("warning")
    );
    assert_eq!(
        serde_json::to_value(Severity::Info).expect("severity"),
        serde_json::json!("info")
    );

    // `Report` is a single `diagnostics` array; the round trip preserves everything.
    let r = report([d.clone(), w.clone()]);
    assert_eq!(
        serde_json::to_value(&r).expect("report serializes"),
        serde_json::json!({ "diagnostics": [
            {"code": "E_ASSET_MISSING", "severity": "error", "message": "image not found",
             "line": 12, "column": 7, "asset": "../img/logo.png"},
            {"code": "W_FRONTMATTER", "severity": "warning", "message": "no front matter",
             "asset": "logo.png"}
        ] })
    );
    let back: Report = serde_json::from_str(&serde_json::to_string(&r).expect("report json"))
        .expect("report back");
    assert_eq!(back, r);
    assert_eq!(back.diagnostics[0].line, Some(12));
}

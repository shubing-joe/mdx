//! End-to-end smoke test for the `mdx` binary.
//!
//! The tests drive the executable cargo builds for this package
//! (`env!("CARGO_BIN_EXE_mdx")`) over the checked-in fixtures in
//! `fixtures/notes/`, which is the same path a user takes:
//! read → parse → resolve assets → convert math/diagrams → render.
//!
//! Assertions stay on the coarse surface the contract freezes: process exit
//! status, diagnostic codes and the PDF magic bytes. The wording of a message
//! is free to change; its code and the exit status are not.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Repository root: the first ancestor of this crate that holds `fixtures/notes`.
///
/// The test target lives in a crate below the root, so the manifest directory
/// is not the root itself.
fn repo_root() -> Result<PathBuf, String> {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("fixtures").join("notes").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            let manifest = env!("CARGO_MANIFEST_DIR");
            return Err(format!("no fixtures/notes directory above {manifest}"));
        }
    }
}

/// Absolute path of a checked-in fixture.
fn fixture(name: &str) -> Result<PathBuf, String> {
    let path = repo_root()?.join("fixtures").join("notes").join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("fixture {} does not exist", path.display()))
    }
}

/// Scratch directory cargo creates for integration tests.
fn scratch() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("smoke")
}

/// One finished `mdx` invocation.
struct Run {
    /// Exit status, or `None` when the process died from a signal.
    code: Option<i32>,
    /// Everything the process wrote to stdout.
    stdout: String,
    /// Everything the process wrote to stderr.
    stderr: String,
}

impl Run {
    /// Stdout and stderr together.
    ///
    /// Which stream carries a diagnostic is not part of the contract, only the
    /// code it carries.
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// Run the `mdx` binary and capture everything it printed.
fn mdx(args: &[&str]) -> Result<Run, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_mdx"))
        .args(args)
        .output()
        .map_err(|e| format!("could not run mdx with {args:?}: {e}"))?;
    Ok(Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Parse the single JSON object `--json` promises on stdout.
fn envelope(run: &Run, what: &str) -> Result<Value, String> {
    let parsed: Result<Value, serde_json::Error> = serde_json::from_str(run.stdout.trim());
    match parsed {
        Ok(value) => Ok(value),
        Err(error) => Err(format!("{what}: stdout is not one JSON object ({error})")),
    }
}

/// The document model inside whatever `mdx ast` printed.
///
/// `--json` wraps the output in the CLI envelope, a bare run may print the
/// document directly, and the document itself is the object carrying `blocks`.
fn document(value: &Value) -> Option<&Value> {
    if value.get("blocks").is_some() {
        return Some(value);
    }
    for key in ["doc", "ast", "document"] {
        if let Some(inner) = value.get(key)
            && inner.get("blocks").is_some()
        {
            return Some(inner);
        }
    }
    None
}

#[test]
fn check_reports_a_clean_document() -> Result<(), Box<dyn std::error::Error>> {
    let arg = fixture("basic.md")?.to_string_lossy().into_owned();

    let plain = mdx(&["check", &arg])?;
    assert_eq!(
        plain.code,
        Some(0),
        "mdx check must succeed on a valid document, got {:?}: {}",
        plain.code,
        plain.combined()
    );

    let run = mdx(&["check", &arg, "--json"])?;
    let json = envelope(&run, "mdx check basic.md --json")?;
    assert_eq!(
        json.get("ok").and_then(Value::as_bool),
        Some(true),
        "a clean document must be reported as ok: {}",
        run.stdout
    );
    assert_eq!(
        json.get("exit").and_then(Value::as_i64),
        Some(0),
        "a clean document must report exit 0: {}",
        run.stdout
    );

    let diagnostics = json
        .get("diagnostics")
        .and_then(Value::as_array)
        .ok_or("the envelope must carry a diagnostics array")?;
    let fatal: Vec<&Value> = diagnostics
        .iter()
        .filter(|d| d.get("severity").and_then(Value::as_str) == Some("error"))
        .collect();
    assert!(
        fatal.is_empty(),
        "a clean document has no errors: {fatal:?}"
    );
    Ok(())
}

#[test]
fn check_reports_a_missing_asset() -> Result<(), Box<dyn std::error::Error>> {
    let arg = fixture("broken-asset.md")?.to_string_lossy().into_owned();

    let plain = mdx(&["check", &arg])?;
    assert_eq!(
        plain.code,
        Some(3),
        "a missing asset is an asset problem (exit 3), got {:?}: {}",
        plain.code,
        plain.combined()
    );
    assert!(
        plain.combined().contains("E_ASSET_MISSING"),
        "the diagnostic must carry its code: {}",
        plain.combined()
    );

    let run = mdx(&["check", &arg, "--json"])?;
    let json = envelope(&run, "mdx check broken-asset.md --json")?;
    assert_eq!(json.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(json.get("exit").and_then(Value::as_i64), Some(3));

    let mut codes: Vec<&str> = Vec::new();
    if let Some(diagnostics) = json.get("diagnostics").and_then(Value::as_array) {
        for diagnostic in diagnostics {
            if let Some(code) = diagnostic.get("code").and_then(Value::as_str) {
                codes.push(code);
            }
        }
    }
    assert!(
        codes.contains(&"E_ASSET_MISSING"),
        "expected E_ASSET_MISSING among the diagnostics, got {codes:?}"
    );
    Ok(())
}

#[test]
fn ast_prints_the_document_as_json() -> Result<(), Box<dyn std::error::Error>> {
    let arg = fixture("basic.md")?.to_string_lossy().into_owned();

    let bare = mdx(&["ast", &arg])?;
    assert_eq!(
        bare.code,
        Some(0),
        "mdx ast must succeed on a valid document, got {:?}: {}",
        bare.code,
        bare.combined()
    );
    let parsed: Result<Value, serde_json::Error> = serde_json::from_str(bare.stdout.trim());
    let bare_json = match parsed {
        Ok(value) => value,
        Err(error) => {
            let message = format!("mdx ast must print valid JSON ({error})");
            return Err(message.into());
        }
    };
    assert!(
        document(&bare_json).is_some(),
        "mdx ast must include the parsed document model: {}",
        bare.stdout
    );

    let run = mdx(&["ast", &arg, "--json"])?;
    let json = envelope(&run, "mdx ast basic.md --json")?;
    assert_eq!(json.get("exit").and_then(Value::as_i64), Some(0));
    let found = document(&json);
    let missing = "mdx ast --json must include the document model";
    let doc = found.ok_or(missing)?;
    let blocks = doc
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or("the document model must carry a blocks array")?;
    assert!(
        !blocks.is_empty(),
        "basic.md parses into more than zero blocks"
    );
    Ok(())
}

#[test]
fn render_writes_a_pdf() -> Result<(), Box<dyn std::error::Error>> {
    let input = fixture("basic.md")?;
    let dir = scratch();
    fs::create_dir_all(&dir)?;
    let out = dir.join("basic.pdf");
    let _ = fs::remove_file(&out);
    let input_arg = input.to_string_lossy().into_owned();
    let out_arg = out.to_string_lossy().into_owned();

    let run = mdx(&["render", &input_arg, "-o", &out_arg])?;
    assert_eq!(
        run.code,
        Some(0),
        "mdx render must succeed on a valid document, got {:?}: {}",
        run.code,
        run.combined()
    );

    let bytes = match fs::read(&out) {
        Ok(bytes) => bytes,
        Err(e) => return Err(format!("no PDF at {}: {e}", out.display()).into()),
    };
    assert!(
        bytes.starts_with(b"%PDF-"),
        "rendered output must be a PDF, which starts with %PDF-: {:?}",
        String::from_utf8_lossy(&bytes[..bytes.len().min(8)])
    );
    assert!(
        bytes.len() > 1024,
        "the rendered PDF is only {} bytes",
        bytes.len()
    );
    Ok(())
}

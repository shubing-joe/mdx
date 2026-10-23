//! The exit-code contract of the `mdx` binary.
//!
//! `docs/CONTRACT.md` freezes four codes: `0` success, `1` parse or render
//! failure, `2` usage or configuration error, `3` unresolved assets. Every test
//! here pins one of them against a run a user can reproduce, and checks that the
//! `--json` envelope reports the same code the process exited with.
//!
//! The tests are independent: each builds its own scratch directory below
//! `CARGO_TARGET_TMPDIR`, reads the checked-in fixtures through
//! `CARGO_MANIFEST_DIR`, and touches neither the network nor a machine font.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Success (`mdx_core::exit::OK`).
const OK: i32 = 0;

/// Parse or render failure (`mdx_core::exit::FAILED`).
const FAILED: i32 = 1;

/// Usage or configuration error (`mdx_core::exit::USAGE`).
const USAGE: i32 = 2;

/// Unresolved assets (`mdx_core::exit::ASSETS`).
const ASSETS: i32 = 3;

/// A document whose front matter is not valid YAML.
///
/// The fences are well formed, so the block is recognised as front matter; the
/// unterminated quote and the unclosed flow sequence are what the YAML parser
/// rejects. The contract then says the body still renders and the run still
/// succeeds, with a `W_FRONTMATTER` warning in the report.
const BROKEN_FRONT_MATTER: &str = "---\ntitle: \"unterminated\ntags: [a, b\n---\n\n# Body\n\nThe body after broken front matter still renders.\n";

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

/// Scratch directory for one test, below `CARGO_TARGET_TMPDIR`.
///
/// Every test names its own directory, so two tests never share a file even
/// when cargo runs them in parallel.
fn scratch(case: &str) -> Result<PathBuf, String> {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let dir = root.join("exit-codes").join(case);
    if let Err(error) = fs::create_dir_all(&dir) {
        return Err(format!("cannot create {}: {error}", dir.display()));
    }
    Ok(dir)
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
    /// Stdout and stderr together, for an assertion's failure message.
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

/// A path as a command line argument.
fn argument(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Parse the single JSON object `--json` promises, and check the part of its
/// shape every run shares: `ok`, `exit`, `diagnostics` and `output` are always
/// present, `ok` mirrors `exit`, and `exit` is the code the process returned.
fn envelope(run: &Run, code: i32, what: &str) -> Result<Value, String> {
    let parsed: Result<Value, serde_json::Error> = serde_json::from_str(run.stdout.trim());
    let json = match parsed {
        Ok(value) => value,
        Err(error) => return Err(format!("{what}: stdout is not one JSON object ({error})")),
    };
    let Some(object) = json.as_object() else {
        return Err(format!(
            "{what}: the envelope is no object: {}",
            run.combined()
        ));
    };
    for key in ["ok", "exit", "diagnostics", "output"] {
        if !object.contains_key(key) {
            return Err(format!(
                "{what}: the envelope has no `{key}`: {}",
                run.stdout
            ));
        }
    }
    let ok = json.get("ok").and_then(Value::as_bool);
    if ok != Some(code == OK) {
        return Err(format!(
            "{what}: `ok` must be false for {code}: {}",
            run.stdout
        ));
    }
    if json.get("exit").and_then(Value::as_i64) != Some(i64::from(code)) {
        return Err(format!("{what}: `exit` must be {code}: {}", run.stdout));
    }
    if run.code != Some(code) {
        return Err(format!(
            "{what}: the process exited {code}: {}",
            run.combined()
        ));
    }
    Ok(json)
}

/// Whether the envelope's diagnostics carry `code`.
fn has_code(json: &Value, code: &str) -> bool {
    let Some(diagnostics) = json.get("diagnostics").and_then(Value::as_array) else {
        return false;
    };
    for diagnostic in diagnostics {
        if diagnostic.get("code").and_then(Value::as_str) == Some(code) {
            return true;
        }
    }
    false
}

/// The severity the envelope records for `code`, when it reports that code.
fn severity_of<'a>(json: &'a Value, code: &str) -> Option<&'a str> {
    let diagnostics = json.get("diagnostics")?.as_array()?;
    for diagnostic in diagnostics {
        if diagnostic.get("code").and_then(Value::as_str) == Some(code) {
            return diagnostic.get("severity").and_then(Value::as_str);
        }
    }
    None
}

/// Run `mdx args` and assert the usage failure every subcommand shares: exit 2
/// with the envelope carrying the reason, and no output.
fn usage_failure(args: &[&str], what: &str) -> Result<(), String> {
    let run = mdx(args)?;
    let json = envelope(&run, USAGE, what)?;
    let output = json.get("output").and_then(Value::as_str);
    assert!(
        output.is_none(),
        "{what}: nothing was written: {}",
        run.stdout
    );
    assert!(has_code(&json, "E_IO"), "{what}: {}", run.combined());
    Ok(())
}

/// Render `input` to `out` and assert the write-failure contract: exit 1, an
/// `E_IO` diagnostic, and no output file.
fn output_failure(input: &Path, out: &Path, what: &str) -> Result<(), String> {
    let input_arg = argument(input);
    let out_arg = argument(out);
    let run = mdx(&["render", &input_arg, "-o", &out_arg, "--json"])?;
    let json = envelope(&run, FAILED, what)?;
    let output = json.get("output").and_then(Value::as_str);
    assert!(
        output.is_none(),
        "{what}: nothing was written: {}",
        run.stdout
    );
    assert!(!out.exists(), "{what}: no output file may exist");
    assert!(has_code(&json, "E_IO"), "{what}: {}", run.combined());
    Ok(())
}

#[test]
fn a_missing_input_is_a_usage_error() -> Result<(), Box<dyn std::error::Error>> {
    let dir = scratch("missing-input")?;
    let missing = dir.join("does-not-exist.md");
    let _ = fs::remove_file(&missing);
    let input = argument(&missing);
    let out = argument(&dir.join("out.pdf"));

    let check: [&str; 3] = ["check", &input, "--json"];
    usage_failure(&check, "mdx check on a missing input")?;
    let ast: [&str; 3] = ["ast", &input, "--json"];
    usage_failure(&ast, "mdx ast on a missing input")?;
    let assets: [&str; 3] = ["assets", &input, "--json"];
    usage_failure(&assets, "mdx assets on a missing input")?;
    let render: [&str; 5] = ["render", &input, "-o", &out, "--json"];
    usage_failure(&render, "mdx render on a missing input")?;
    Ok(())
}

#[test]
fn an_unresolved_asset_is_an_asset_error() -> Result<(), Box<dyn std::error::Error>> {
    let input = fixture("broken-asset.md")?;
    let input_arg = argument(&input);

    let what = "mdx check broken-asset.md --json";
    let run = mdx(&["check", &input_arg, "--json"])?;
    let json = envelope(&run, ASSETS, what)?;
    assert!(
        has_code(&json, "E_ASSET_MISSING"),
        "{what}: {}",
        run.combined()
    );
    assert_eq!(severity_of(&json, "E_ASSET_MISSING"), Some("error"));

    // A failed build writes nothing: the exit code already says it failed, and
    // a half-written file would look finished.
    let dir = scratch("broken-asset")?;
    let out = dir.join("out.pdf");
    let _ = fs::remove_file(&out);
    let out_arg = argument(&out);
    let what = "mdx render broken-asset.md --json";
    let run = mdx(&["render", &input_arg, "-o", &out_arg, "--json"])?;
    let json = envelope(&run, ASSETS, what)?;
    assert!(
        has_code(&json, "E_ASSET_MISSING"),
        "{what}: {}",
        run.combined()
    );
    assert!(!out.exists(), "{what}: a failed render writes no file");
    Ok(())
}

#[test]
fn malformed_front_matter_warns_but_still_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let dir = scratch("front-matter")?;
    let input = dir.join("broken-front-matter.md");
    fs::write(&input, BROKEN_FRONT_MATTER)?;
    let input_arg = argument(&input);

    let what = "mdx check broken-front-matter.md --json";
    let run = mdx(&["check", &input_arg, "--json"])?;
    let json = envelope(&run, OK, what)?;
    assert!(
        has_code(&json, "W_FRONTMATTER"),
        "{what}: {}",
        run.combined()
    );
    assert_eq!(severity_of(&json, "W_FRONTMATTER"), Some("warning"));

    // The warning is about the header, not the document: the body still renders.
    let out = dir.join("out.pdf");
    let _ = fs::remove_file(&out);
    let out_arg = argument(&out);
    let what = "mdx render broken-front-matter.md --json";
    let run = mdx(&["render", &input_arg, "-o", &out_arg, "--json"])?;
    let json = envelope(&run, OK, what)?;
    let written = json.get("output").and_then(Value::as_str);
    assert_eq!(written, Some(out_arg.as_str()), "{what}: {}", run.stdout);
    let bytes = match fs::read(&out) {
        Ok(bytes) => bytes,
        Err(error) => return Err(format!("no PDF at {}: {error}", out.display()).into()),
    };
    assert!(
        bytes.starts_with(b"%PDF-"),
        "{what}: the body must still render"
    );
    Ok(())
}

#[test]
fn an_unwritable_output_path_fails_the_render() -> Result<(), Box<dyn std::error::Error>> {
    // The parent of the output is a regular file, so opening the output fails
    // for every user on every platform: the run reports `E_IO` and exits 1
    // instead of pretending a document was written.
    let dir = scratch("unwritable-path")?;
    let blocker = dir.join("not-a-directory");
    fs::write(&blocker, b"a regular file, not a directory")?;
    let out = blocker.join("out.pdf");
    output_failure(&fixture("basic.md")?, &out, "output inside a file")?;
    Ok(())
}

/// An output directory the process may not write into.
///
/// A test runner with root privileges ignores the write bit, and then no
/// portable trick makes the directory deny the write; the not-a-directory case
/// above covers such a runner, and this test asserts what it can observe.
#[cfg(unix)]
#[test]
fn an_unwritable_output_directory_fails_the_render() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("unwritable-directory")?;
    let blocked = dir.join("blocked");
    let _ = fs::remove_dir_all(&blocked);
    fs::create_dir_all(&blocked)?;
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500))?;

    let probe = blocked.join("probe");
    let denies_writes = fs::write(&probe, b"probe").is_err();
    if !denies_writes {
        let _ = fs::remove_file(&probe);
    }

    let out = blocked.join("out.pdf");
    let rendered = if denies_writes {
        output_failure(
            &fixture("basic.md")?,
            &out,
            "output inside a read-only directory",
        )
    } else {
        Ok(())
    };

    // Restore the write bit so the scratch directory can be cleaned up.
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700))?;
    rendered?;
    Ok(())
}

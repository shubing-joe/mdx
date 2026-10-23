//! End-to-end tests for the `mdx` binary over the adversarial fixture set.
//!
//! `smoke.rs` walks the pipeline once with `basic.md`; this file drives the
//! documents that stress its edges — raw HTML, math inside table cells and
//! footnotes, source lines two kilobytes long, a two-hundred-column table,
//! scripts without a Latin base, an empty document and a document that is
//! nothing but math — and pins the surface the contract freezes: the process
//! exit status, the `--json` envelope, the bytes of the rendered PDF, and the
//! byte-identity of two `--deterministic` renders.
//!
//! Every test owns a scratch directory below `CARGO_TARGET_TMPDIR` and reaches
//! its fixture through `CARGO_MANIFEST_DIR`, so the tests stay independent of
//! each other, write nowhere else, and never touch the network. Glyph coverage
//! is deliberately not asserted: which fonts a machine ships is none of the
//! contract's business, and a document needing a glyph no font has still
//! renders — only the exit status and the shape of the output are checked.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Exit code of a successful run (`mdx_core::exit::OK`).
const OK: i32 = 0;

/// Smallest PDF that is still plausibly a rendered page.
const PDF_FLOOR: usize = 4096;

/// Smallest PDF for `empty.md`, whose one blank page carries almost nothing.
const BLANK_PDF_FLOOR: usize = 1024;

/// SHA-256 of the empty message, from the FIPS 180-4 examples.
const EMPTY_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// SHA-256 of `b"abc"`, the one-block example from the FIPS 180-4 examples.
const ABC_DIGEST: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

/// The 56-byte message whose padding exactly fills its first block.
const PADDED_MESSAGE: &[u8] = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";

/// SHA-256 of [`PADDED_MESSAGE`].
const PADDED_DIGEST: &str = "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1";

/// A message that is not ASCII and not a multiple of the block size.
const PDF_MESSAGE: &[u8] = b"%PDF-1.7 a rendered document is not text";

/// SHA-256 of [`PDF_MESSAGE`].
const PDF_DIGEST: &str = "9e4bfb9b49e7bd938312b14c8602fb61ed8c29b9ed73a5da35a6f476a0a1a4b1";

/// Fractional parts of the square roots of the first eight primes
/// (FIPS 180-4, §5.3.3), eight hex digits per constant.
const INITIAL_STATE: &str =
    "6a09e667 bb67ae85 3c6ef372 a54ff53a 510e527f 9b05688c 1f83d9ab 5be0cd19";

/// The first 32 bits of the fractional parts of the cube roots of the first 64
/// primes (FIPS 180-4, §4.2.2), eight hex digits per constant.
///
/// Both tables are hex text rather than integer arrays: an array of numbers
/// this long is laid out one number per line, while a string literal is the one
/// token no formatter may split, so the table keeps the grid the standard
/// prints.
const ROUND_CONSTANTS: &str = "428a2f98 71374491 b5c0fbcf e9b5dba5 3956c25b 59f111f1 923f82a4 ab1c5ed5 d807aa98 12835b01 243185be 550c7dc3 72be5d74 80deb1fe 9bdc06a7 c19bf174 e49b69c1 efbe4786 0fc19dc6 240ca1cc 2de92c6f 4a7484aa 5cb0a9dc 76f988da 983e5152 a831c66d b00327c8 bf597fc7 c6e00bf3 d5a79147 06ca6351 14292967 27b70a85 2e1b2138 4d2c6dfc 53380d13 650a7354 766a0abb 81c2c92e 92722c85 a2bfe8a1 a81a664b c24b8b70 c76c51a3 d192e819 d6990624 f40e3585 106aa070 19a4c116 1e376c08 2748774c 34b0bcb5 391c0cb3 4ed8aa4a 5b9cca4f 682e6ff3 748f82ee 78a5636f 84c87814 8cc70208 90befffa a4506ceb bef9a3f7 c67178f2";

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
    let dir = root.join("e2e").join(case);
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

/// A path as a command line argument.
fn argument(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Parse the single JSON object `--json` promises on stdout.
fn envelope(run: &Run, what: &str) -> Result<Value, String> {
    let parsed: Result<Value, serde_json::Error> = serde_json::from_str(run.stdout.trim());
    match parsed {
        Ok(value) => Ok(value),
        Err(error) => Err(format!("{what}: stdout is not one JSON object ({error})")),
    }
}

/// Check the shape every `--json` run promises, and that it agrees with the
/// process status.
///
/// The envelope is `{"ok","exit","diagnostics","output","manifest"}`; `ok`
/// mirrors `exit`, every diagnostic carries `code`, `severity` and `message`,
/// and `output` is either `null` or the path that was written.
fn assert_envelope(run: &Run, json: &Value, code: i32, what: &str) -> Result<(), String> {
    let Some(object) = json.as_object() else {
        return Err(format!(
            "{what}: the envelope is no object: {}",
            run.combined()
        ));
    };
    for key in ["ok", "exit", "diagnostics", "output", "manifest"] {
        if !object.contains_key(key) {
            return Err(format!(
                "{what}: the envelope has no `{key}`: {}",
                run.stdout
            ));
        }
    }
    if json.get("ok").and_then(Value::as_bool) != Some(code == OK) {
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
    let Some(diagnostics) = json.get("diagnostics").and_then(Value::as_array) else {
        return Err(format!(
            "{what}: `diagnostics` must be an array: {}",
            run.stdout
        ));
    };
    for diagnostic in diagnostics {
        if !complete_diagnostic(diagnostic) {
            return Err(format!("{what}: incomplete diagnostic: {diagnostic}"));
        }
    }
    let output = json.get("output");
    if !matches!(output, Some(Value::Null) | Some(Value::String(_))) {
        return Err(format!("{what}: `output` is no path: {}", run.stdout));
    }
    Ok(())
}

/// Whether a diagnostic object carries the three keys the contract promises.
fn complete_diagnostic(diagnostic: &Value) -> bool {
    let code = diagnostic.get("code").and_then(Value::as_str);
    let severity = diagnostic.get("severity").and_then(Value::as_str);
    let message = diagnostic.get("message").and_then(Value::as_str);
    code.is_some() && severity.is_some() && message.is_some()
}

/// Whether the envelope reports no error diagnostic.
///
/// Error codes start with `E` and warnings with `W` (`mdx_core::DiagCode`), so
/// the envelope needs no extra field to tell the two apart.
fn is_clean(json: &Value) -> bool {
    let Some(diagnostics) = json.get("diagnostics").and_then(Value::as_array) else {
        return false;
    };
    diagnostics.iter().all(|diagnostic| {
        let code = diagnostic.get("code").and_then(Value::as_str);
        code.is_none_or(|code| !code.starts_with('E'))
    })
}

/// Assert the bytes of a rendered file are a PDF of a plausible size.
fn assert_pdf(bytes: &[u8], floor: usize, what: &str) -> Result<(), String> {
    if !bytes.starts_with(b"%PDF-") {
        let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(5)]).into_owned();
        return Err(format!(
            "{what}: output must start with %PDF-, got {prefix:?}"
        ));
    }
    let trailing = bytes
        .iter()
        .rev()
        .take_while(|byte| byte.is_ascii_whitespace());
    let end = bytes.len() - trailing.count();
    if !bytes[..end].ends_with(b"%%EOF") {
        return Err(format!("{what}: the PDF must end with %%EOF"));
    }
    if bytes.len() < floor {
        return Err(format!("{what}: the PDF is only {} bytes", bytes.len()));
    }
    Ok(())
}

/// Check, render and verify one fixture: exit 0, envelope intact, PDF written.
fn exercise(name: &str, floor: usize) -> Result<(), Box<dyn std::error::Error>> {
    let input = fixture(name)?;
    let input_arg = argument(&input);

    // `check` parses and resolves assets without writing anything: a fixture
    // that cannot be checked cannot be rendered either.
    let what = format!("mdx check {name} --json");
    let run = mdx(&["check", &input_arg, "--json"])?;
    let json = envelope(&run, &what)?;
    assert_envelope(&run, &json, OK, &what)?;
    assert!(
        is_clean(&json),
        "{what}: {name} reports an error diagnostic: {}",
        run.combined()
    );
    assert!(
        json.get("output").and_then(Value::as_str).is_none(),
        "{what}: check writes no output: {}",
        run.stdout
    );

    // `render` writes the PDF the same fixture has to produce in a real build.
    let dir = scratch(name.trim_end_matches(".md"))?;
    let out = dir.join("out.pdf");
    let _ = fs::remove_file(&out);
    let out_arg = argument(&out);
    let what = format!("mdx render {name} -o out.pdf --json");
    let run = mdx(&["render", &input_arg, "-o", &out_arg, "--json"])?;
    let json = envelope(&run, &what)?;
    assert_envelope(&run, &json, OK, &what)?;
    assert_eq!(
        json.get("output").and_then(Value::as_str),
        Some(out_arg.as_str()),
        "{what}: the envelope must name the written file: {}",
        run.stdout
    );
    let bytes = match fs::read(&out) {
        Ok(bytes) => bytes,
        Err(error) => return Err(format!("no PDF at {}: {error}", out.display()).into()),
    };
    assert_pdf(&bytes, floor, &what)?;
    Ok(())
}

#[test]
fn adversarial_html_and_nesting_renders() -> Result<(), Box<dyn std::error::Error>> {
    exercise("adversarial.md", PDF_FLOOR)
}

#[test]
fn math_in_tables_and_footnotes_renders() -> Result<(), Box<dyn std::error::Error>> {
    exercise("math-table.md", PDF_FLOOR)
}

#[test]
fn two_kilobyte_lines_and_a_wide_table_render() -> Result<(), Box<dyn std::error::Error>> {
    exercise("long-lines.md", PDF_FLOOR)
}

#[test]
fn cjk_arabic_and_combining_marks_render() -> Result<(), Box<dyn std::error::Error>> {
    exercise("unicode.md", PDF_FLOOR)
}

#[test]
fn an_empty_document_renders() -> Result<(), Box<dyn std::error::Error>> {
    exercise("empty.md", BLANK_PDF_FLOOR)
}

#[test]
fn a_document_of_math_alone_renders() -> Result<(), Box<dyn std::error::Error>> {
    exercise("only-math.md", PDF_FLOOR)
}

/// Render `input` to `out` with `--deterministic` and return the PDF bytes.
fn render_deterministic(input: &str, out: &Path, what: &str) -> Result<Vec<u8>, String> {
    let out_arg = argument(out);
    let _ = fs::remove_file(out);
    let run = mdx(&["render", input, "-o", &out_arg, "--deterministic"])?;
    if run.code != Some(OK) {
        let message = format!(
            "{what}: must succeed, got {:?}: {}",
            run.code,
            run.combined()
        );
        return Err(message);
    }
    let bytes = match fs::read(out) {
        Ok(bytes) => bytes,
        Err(error) => return Err(format!("no PDF at {}: {error}", out.display())),
    };
    assert_pdf(&bytes, PDF_FLOOR, what)?;
    Ok(bytes)
}

/// Two `--deterministic` renders of one document are byte-identical.
///
/// `--deterministic` pins the build date and the diagram text; the PDF writer
/// adds no wall-clock timestamp of its own and derives the document id from the
/// document, so a second build of the same input has to reproduce the first
/// byte for byte. Anything else would turn every rebuild into a spurious diff.
#[test]
fn deterministic_renders_are_byte_identical() -> Result<(), Box<dyn std::error::Error>> {
    for name in ["math-table.md", "long-lines.md"] {
        let input_arg = argument(&fixture(name)?);
        let dir = scratch(&format!("deterministic-{}", name.trim_end_matches(".md")))?;
        let first_path = dir.join("first.pdf");
        let second_path = dir.join("second.pdf");
        let first_what = format!("mdx render {name} --deterministic (first pass)");
        let second_what = format!("mdx render {name} --deterministic (second pass)");

        let first = render_deterministic(&input_arg, &first_path, &first_what)?;
        let second = render_deterministic(&input_arg, &second_path, &second_what)?;
        assert_eq!(
            first.len(),
            second.len(),
            "two --deterministic renders of {name} differ in size: {} and {}",
            first_path.display(),
            second_path.display()
        );
        assert_eq!(
            sha256_hex(&first),
            sha256_hex(&second),
            "two --deterministic renders of {name} differ: {} and {}",
            first_path.display(),
            second_path.display()
        );
    }
    Ok(())
}

/// Read `COUNT` space-separated eight-digit hex constants from `text`.
///
/// A field that does not parse stays zero; no published vector accepts such a
/// table, so `sha256_matches_the_published_vectors` cannot pass with one.
fn hex_words<const COUNT: usize>(text: &str) -> [u32; COUNT] {
    let mut words = [0u32; COUNT];
    for (index, slot) in words.iter_mut().enumerate() {
        if let Some(field) = text.get(index * 9..index * 9 + 8) {
            *slot = u32::from_str_radix(field, 16).unwrap_or_default();
        }
    }
    words
}

/// SHA-256 of `bytes`, as lowercase hex.
///
/// A local implementation keeps this test target dependent on nothing beyond
/// `std` and `serde_json`; `sha256_matches_the_published_vectors` pins it to
/// published vectors, so a broken hasher cannot make two different renders look
/// equal.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut state: [u32; 8] = hex_words(INITIAL_STATE);
    let constants: [u32; 64] = hex_words(ROUND_CONSTANTS);

    // Message padding: a `1` bit, zeros, then the length in bits as a 64-bit
    // big-endian integer, so the padded length is a multiple of 64 bytes.
    let mut message = Vec::with_capacity(bytes.len() + 72);
    message.extend_from_slice(bytes);
    let bits = (bytes.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());

    for chunk in message.as_chunks::<64>().0 {
        let mut schedule = [0u32; 64];
        for (index, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            schedule[index] = u32::from_be_bytes(*word);
        }
        // Words 16..64 come from the four earlier words the recurrence names,
        // so the index stays explicit.
        let mut index = 16;
        while index < 64 {
            let previous = schedule[index - 15];
            let recent = schedule[index - 2];
            let s0 = previous.rotate_right(7) ^ previous.rotate_right(18) ^ (previous >> 3);
            let s1 = recent.rotate_right(17) ^ recent.rotate_right(19) ^ (recent >> 10);
            let low = schedule[index - 16].wrapping_add(s0);
            let value = low.wrapping_add(schedule[index - 7]).wrapping_add(s1);
            schedule[index] = value;
            index += 1;
        }

        let mut working = state;
        for (&constant, &word) in constants.iter().zip(schedule.iter()) {
            let [a, b, c, d, e, f, g, h] = working;
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h.wrapping_add(s1).wrapping_add(choice);
            let temp1 = temp1.wrapping_add(constant).wrapping_add(word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            working = [
                temp1.wrapping_add(temp2),
                a,
                b,
                c,
                d.wrapping_add(temp1),
                e,
                f,
                g,
            ];
        }
        for (slot, value) in state.iter_mut().zip(working) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut hex = String::with_capacity(64);
    for word in &state {
        hex.push_str(&format!("{word:08x}"));
    }
    hex
}

/// The hasher above against published SHA-256 vectors.
///
/// The lengths cover the padding boundaries: the empty message, the one-block
/// case, a message whose padding fills its first block exactly, and text that
/// is neither ASCII nor block-aligned.
#[test]
fn sha256_matches_the_published_vectors() {
    assert_eq!(sha256_hex(b""), EMPTY_DIGEST);
    assert_eq!(sha256_hex(b"abc"), ABC_DIGEST);
    assert_eq!(sha256_hex(PADDED_MESSAGE), PADDED_DIGEST);
    assert_eq!(sha256_hex(PDF_MESSAGE), PDF_DIGEST);
}

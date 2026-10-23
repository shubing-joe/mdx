//! The Typst backend's contract with the math corpus.
//!
//! `fixtures/math` is the shared corpus of the two math backends. `svg.rs`
//! asserts the RaTeX side of it (`latex_to_svg` renders every valid fixture and
//! rejects every malformed one); this test asserts the same split for the Typst
//! side, `mdx_math::latex_to_typst`, which is the pipeline's primary path.
//!
//! It is deliberately count-based: the corpus is the coverage report, so a
//! formula that stops converting, a new fixture nobody wired in, or a valid
//! fixture that silently disappears all fail here instead of shipping. The
//! summary goes to stderr because the counts are the point of the test — a
//! `cargo test` run shows the shape of the corpus, not just a green tick.
//!
//! `fixtures/math/COVERAGE.md` records which constructs are covered, which fall
//! back to RaTeX, and which are known gaps.

use std::fs;
use std::path::{Path, PathBuf};

/// Valid fixtures (`NNN-name.tex`, `NNN` = `001`…`085`) that must convert.
const EXPECTED_VALID: usize = 85;

/// Malformed fixtures (`9xx-bad-*.tex`) that must fail cleanly.
const EXPECTED_MALFORMED: usize = 10;

/// Marker in a malformed fixture's file name.
const MALFORMED_MARKER: &str = "-bad-";

#[test]
fn corpus_converts_or_rejects_as_documented() {
    let dir = corpus_dir();
    let paths = tex_files(&dir);
    assert_eq!(
        paths.len(),
        EXPECTED_VALID + EXPECTED_MALFORMED,
        "expected {} fixtures in {dir:?}, found {}",
        EXPECTED_VALID + EXPECTED_MALFORMED,
        paths.len()
    );

    let mut converted = Vec::new();
    let mut rejected = Vec::new();
    let mut problems = Vec::new();

    for path in &paths {
        let name = file_name(path);
        let src = match fs::read_to_string(path) {
            Ok(src) => src,
            Err(error) => {
                problems.push(format!("{name}: not readable UTF-8: {error}"));
                continue;
            }
        };
        let malformed = name.contains(MALFORMED_MARKER);

        match mdx_math::latex_to_typst(&src) {
            Ok(body) => {
                if malformed {
                    problems.push(format!(
                        "{name}: malformed fixture converted to `{}`",
                        excerpt(&body)
                    ));
                } else if let Some(problem) = unusable_body(&body) {
                    problems.push(format!("{name}: {problem}"));
                } else {
                    converted.push(body);
                }
            }
            Err(reason) => {
                if reason.trim().is_empty() {
                    problems.push(format!("{name}: rejected without a reason"));
                } else if malformed {
                    rejected.push(format!("{name}: {reason}"));
                } else {
                    problems.push(format!("{name}: valid fixture rejected: {reason}"));
                }
            }
        }
    }

    eprintln!(
        "math corpus in {}: {} fixtures, {} expected valid / {} expected malformed",
        dir.display(),
        paths.len(),
        EXPECTED_VALID,
        EXPECTED_MALFORMED
    );
    eprintln!("  converted: {}", converted.len());
    eprintln!("  rejected:  {}", rejected.len());
    for reason in &rejected {
        eprintln!("    {reason}");
    }

    assert_eq!(
        converted.len(),
        EXPECTED_VALID,
        "converted {} valid fixtures, expected {EXPECTED_VALID}: {problems:#?}",
        converted.len()
    );
    assert_eq!(
        rejected.len(),
        EXPECTED_MALFORMED,
        "rejected {} malformed fixtures, expected {EXPECTED_MALFORMED}: {problems:#?}",
        rejected.len()
    );
    assert!(problems.is_empty(), "corpus problems: {problems:#?}");
}

/// Every fixture is either `NNN-name.tex` (valid) or `9NN-bad-name.tex`
/// (malformed), and the numbering runs without a gap or a repeat. A corpus that
/// drifts off its own naming scheme stops being comparable with its README.
#[test]
fn corpus_names_follow_the_numbering_scheme() {
    let mut seen = Vec::new();

    for path in tex_files(&corpus_dir()) {
        let name = file_name(&path);
        let number = name
            .split(['-', '_'])
            .next()
            .and_then(|prefix| prefix.parse::<u32>().ok());
        let Some(number) = number else {
            panic!("{name}: expected a numeric `NNN-` prefix");
        };

        let malformed = name.contains(MALFORMED_MARKER);
        if malformed != (900..1000).contains(&number) {
            panic!("{name}: `{MALFORMED_MARKER}` fixtures must be numbered 9xx");
        }
        if !name.ends_with(".tex") {
            panic!("{name}: expected a `.tex` fixture");
        }

        seen.push((number, name));
    }

    seen.sort();

    let mut expected = 1u32;
    for (number, name) in &seen {
        let malformed = name.contains(MALFORMED_MARKER);
        if malformed {
            // The malformed block starts at 901 and is checked separately.
            continue;
        }
        assert_eq!(
            *number, expected,
            "{name}: valid fixtures must number {expected} here"
        );
        expected += 1;
    }
    assert_eq!(
        expected - 1,
        EXPECTED_VALID as u32,
        "valid fixtures must number 1..={EXPECTED_VALID}"
    );

    let malformed: Vec<u32> = seen
        .iter()
        .filter(|(_, name)| name.contains(MALFORMED_MARKER))
        .map(|(number, _)| *number)
        .collect();
    let expected_malformed: Vec<u32> = (901..901 + EXPECTED_MALFORMED as u32).collect();
    assert_eq!(
        malformed,
        expected_malformed,
        "malformed fixtures must number 901..={}",
        900 + EXPECTED_MALFORMED
    );
}

/// `None` when the converted body is a usable Typst math expression body,
/// otherwise the reason it is not.
///
/// The body is spliced between `$…$` by the writers without being inspected
/// again, so a fence, a markup statement or a LaTeX command surviving in it
/// would be a contract violation there rather than here.
fn unusable_body(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return Some("converted to an empty body".to_string());
    }
    if body.contains('$') {
        return Some(format!("body carries a `$` fence: {}", excerpt(body)));
    }
    if body.lines().any(|line| {
        let line = line.trim_start();
        ["#set", "#let", "#show", "#import"]
            .iter()
            .any(|keyword| line.starts_with(keyword))
    }) {
        return Some(format!("body carries Typst markup: {}", excerpt(body)));
    }
    if body.contains(r"\begin{") || body.contains(r"\end{") {
        return Some(format!(
            "body carries a LaTeX environment: {}",
            excerpt(body)
        ));
    }
    if body.contains("zws") {
        return Some(format!(
            "body carries an empty-argument placeholder: {}",
            excerpt(body)
        ));
    }
    None
}

/// First 60 characters of `body`, for a failure message.
fn excerpt(body: &str) -> String {
    body.chars().take(60).collect()
}

/// `fixtures/math`, relative to this crate.
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/math")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Every `.tex` fixture in `dir`, sorted by name.
fn tex_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("fixtures/math not readable: {error}"))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == std::ffi::OsStr::new("tex"))
        })
        .collect();
    paths.sort();
    paths
}

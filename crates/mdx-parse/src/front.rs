//! Front matter split and YAML header parsing.
//!
//! [`split_front_matter`] is byte-oriented and allocation-free: it returns
//! slices of the original source, so the Markdown parser can keep parsing the
//! body without copying it. [`parse_front`] never fails hard — malformed YAML
//! yields a `W_FRONTMATTER` warning plus empty front matter so the body still
//! renders (see `docs/CONTRACT.md`).

use mdx_core::{DiagCode, Diagnostic, FrontMatter};

/// Opening fence, and the `---` form of the closing fence.
const FENCE: &str = "---";
/// The alternative closing fence (`...`).
const FENCE_DOTS: &str = "...";

/// Split a leading YAML front matter block off `source`.
///
/// Returns `(yaml, body, body_start_line)`:
///
/// * `yaml` — the raw YAML between the fences (`None` when there is no front
///   matter), including its trailing newline.
/// * `body` — everything after the closing fence, or the whole source (minus a
///   UTF-8 BOM) when no front matter was found.
/// * `body_start_line` — the 1-based line number of the first line of `body`
///   in the original source, so diagnostics keep pointing at the right line.
///
/// Accepted shapes (CommonMark/Jekyll compatible):
///
/// * an optional UTF-8 BOM before the opening fence,
/// * `---` alone on the first line (trailing spaces/tabs allowed),
/// * a closing line of `---` or `...`,
/// * LF or CRLF line endings,
/// * an empty block (`---` immediately followed by a fence) parses as empty
///   YAML.
///
/// A missing or unterminated block is not an error: the whole source is
/// returned as the body with `body_start_line == 1`.
pub fn split_front_matter(source: &str) -> (Option<&str>, &str, usize) {
    let rest = source.strip_prefix('\u{feff}').unwrap_or(source);

    let mut offset = 0usize;
    let mut line_no = 0usize;
    let mut yaml_start = 0usize;

    for line in rest.split_inclusive('\n') {
        line_no += 1;
        let text = strip_eol(line);

        if line_no == 1 {
            if text != FENCE {
                return (None, rest, 1);
            }
            offset += line.len();
            yaml_start = offset;
            continue;
        }

        if text == FENCE || text == FENCE_DOTS {
            let yaml = &rest[yaml_start..offset];
            let body = &rest[offset + line.len()..];
            return (Some(yaml), body, line_no + 1);
        }

        offset += line.len();
    }

    (None, rest, 1)
}

/// Parse a front matter YAML block into [`FrontMatter`].
///
/// `title`, `author` and `date` become typed fields (scalars are stringified,
/// `null` means "absent"); every other top-level key is preserved verbatim in
/// [`FrontMatter::extra`] as a JSON value.
///
/// Returns empty front matter plus a `W_FRONTMATTER` warning when the YAML is
/// malformed or is not a mapping. The diagnostic's line/column are relative to
/// the YAML block (line 1 is the first line after the opening fence).
pub fn parse_front(yaml: &str) -> (FrontMatter, Option<Diagnostic>) {
    if yaml.trim().is_empty() {
        return (FrontMatter::default(), None);
    }

    match serde_yaml::from_str::<serde_yaml::Value>(yaml) {
        Ok(value) => {
            if value.as_mapping().is_some() {
                (front_from_value(&value), None)
            } else if value.as_null().is_some() {
                // Comment-only or explicitly null block.
                (FrontMatter::default(), None)
            } else {
                let diag = Diagnostic::warning(
                    DiagCode::FrontMatter,
                    "front matter must be a YAML mapping",
                )
                .at(1, 1);
                (FrontMatter::default(), Some(diag))
            }
        }
        Err(err) => {
            let mut diag = Diagnostic::warning(
                DiagCode::FrontMatter,
                format!("invalid YAML front matter: {err}"),
            );
            if let Some(location) = err.location() {
                diag = diag.at(location.line() as u32, location.column() as u32);
            }
            (FrontMatter::default(), Some(diag))
        }
    }
}

/// Strip the line terminator (and trailing blanks) from one `split_inclusive`
/// line so fences can be compared exactly.
fn strip_eol(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    line.trim_end_matches([' ', '\t'])
}

fn front_from_value(value: &serde_yaml::Value) -> FrontMatter {
    let mut front = FrontMatter::default();
    let Some(mapping) = value.as_mapping() else {
        return front;
    };

    for (key, value) in mapping.iter() {
        let Some(key) = key.as_str() else {
            continue;
        };
        match key {
            "title" => front.title = scalar_string(value),
            "author" => front.author = scalar_string(value),
            "date" => front.date = scalar_string(value),
            _ => {
                front.extra.insert(key.to_owned(), to_json(value));
            }
        }
    }

    front
}

/// Render a scalar (or any structured value) as the string form used for the
/// typed metadata fields. `null` means "absent".
fn scalar_string(value: &serde_yaml::Value) -> Option<String> {
    match to_json(value) {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s),
        other => Some(other.to_string()),
    }
}

/// Convert a YAML value into its JSON equivalent.
///
/// Structured values are converted recursively; non-string mapping keys are
/// rendered through their JSON form so the result stays deterministic and
/// order-independent (homogeneous YAML keys keep their source order inside
/// `serde_json::Map`, which is sorted by default).
fn to_json(value: &serde_yaml::Value) -> serde_json::Value {
    if let Some(b) = value.as_bool() {
        return serde_json::Value::Bool(b);
    }
    if let Some(i) = value.as_i64() {
        return serde_json::Value::Number(i.into());
    }
    if let Some(u) = value.as_u64() {
        return serde_json::Value::Number(u.into());
    }
    if let Some(f) = value.as_f64() {
        return match serde_json::Number::from_f64(f) {
            Some(n) => serde_json::Value::Number(n),
            None => serde_json::Value::String(f.to_string()),
        };
    }
    if let Some(s) = value.as_str() {
        return serde_json::Value::String(s.to_owned());
    }
    if let Some(sequence) = value.as_sequence() {
        return serde_json::Value::Array(sequence.iter().map(to_json).collect());
    }
    if let Some(mapping) = value.as_mapping() {
        let mut object = serde_json::Map::new();
        for (key, value) in mapping.iter() {
            let key = match key.as_str() {
                Some(key) => key.to_owned(),
                None => to_json(key).to_string(),
            };
            object.insert(key, to_json(value));
        }
        return serde_json::Value::Object(object);
    }
    serde_json::Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lf_front_matter() {
        let (yaml, body, line) = split_front_matter("---\ntitle: T\n---\n# Body\ntext\n");
        assert_eq!(yaml, Some("title: T\n"));
        assert_eq!(body, "# Body\ntext\n");
        assert_eq!(line, 4);
    }

    #[test]
    fn crlf_front_matter() {
        let (yaml, body, line) = split_front_matter("---\r\ntitle: T\r\n---\r\n# Body\r\n");
        assert_eq!(yaml, Some("title: T\r\n"));
        assert_eq!(body, "# Body\r\n");
        assert_eq!(line, 4);

        let (front, diag) = parse_front(yaml.unwrap_or(""));
        assert!(diag.is_none());
        assert_eq!(front.title.as_deref(), Some("T"));
    }

    #[test]
    fn bom_front_matter() {
        let (yaml, body, line) = split_front_matter("\u{feff}---\ntitle: T\n---\nbody\n");
        assert_eq!(yaml, Some("title: T\n"));
        assert_eq!(body, "body\n");
        assert_eq!(line, 4);
    }

    #[test]
    fn bom_without_front_matter_is_stripped() {
        let (yaml, body, line) = split_front_matter("\u{feff}# Title\n");
        assert_eq!(yaml, None);
        assert_eq!(body, "# Title\n");
        assert_eq!(line, 1);
    }

    #[test]
    fn dots_closing_fence() {
        let (yaml, body, line) = split_front_matter("---\na: 1\n...\nbody\n");
        assert_eq!(yaml, Some("a: 1\n"));
        assert_eq!(body, "body\n");
        assert_eq!(line, 4);
    }

    #[test]
    fn missing_closing_fence_keeps_whole_source() {
        let source = "---\ntitle: T\n\n# Body\n";
        let (yaml, body, line) = split_front_matter(source);
        assert_eq!(yaml, None);
        assert_eq!(body, source);
        assert_eq!(line, 1);
    }

    #[test]
    fn unterminated_single_fence_line() {
        let (yaml, body, line) = split_front_matter("---");
        assert_eq!(yaml, None);
        assert_eq!(body, "---");
        assert_eq!(line, 1);
    }

    #[test]
    fn empty_front_matter() {
        let (yaml, body, line) = split_front_matter("---\n---\nbody\n");
        assert_eq!(yaml, Some(""));
        assert_eq!(body, "body\n");
        assert_eq!(line, 3);

        let (front, diag) = parse_front(yaml.unwrap_or(""));
        assert!(diag.is_none());
        assert_eq!(front, FrontMatter::default());
    }

    #[test]
    fn front_matter_at_eof_without_body() {
        let (yaml, body, line) = split_front_matter("---\na: 1\n---");
        assert_eq!(yaml, Some("a: 1\n"));
        assert_eq!(body, "");
        assert_eq!(line, 4);
    }

    #[test]
    fn fence_with_trailing_blanks_is_accepted() {
        let (yaml, body, line) = split_front_matter("---  \ntitle: T\n---\t\nbody\n");
        assert_eq!(yaml, Some("title: T\n"));
        assert_eq!(body, "body\n");
        assert_eq!(line, 4);
    }

    #[test]
    fn fence_not_on_first_line_is_not_front_matter() {
        let source = "\n---\na: 1\n---\n";
        let (yaml, body, line) = split_front_matter(source);
        assert_eq!(yaml, None);
        assert_eq!(body, source);
        assert_eq!(line, 1);
    }

    #[test]
    fn no_front_matter() {
        let source = "# Title\n\ntext\n";
        let (yaml, body, line) = split_front_matter(source);
        assert_eq!(yaml, None);
        assert_eq!(body, source);
        assert_eq!(line, 1);
    }

    #[test]
    fn parse_title_author_date_and_extra() {
        let yaml = "title: Hello World\nauthor: Joe\ndate: 2026-01-01\n\
                    tags:\n  - rust\n  - docs\nbuild:\n  target: pdf\n  jobs: 4\n";
        let (front, diag) = parse_front(yaml);
        assert!(diag.is_none());
        assert_eq!(front.title.as_deref(), Some("Hello World"));
        assert_eq!(front.author.as_deref(), Some("Joe"));
        assert_eq!(front.date.as_deref(), Some("2026-01-01"));
        assert_eq!(front.extra["tags"], serde_json::json!(["rust", "docs"]));
        assert_eq!(
            front.extra["build"],
            serde_json::json!({"jobs": 4, "target": "pdf"})
        );
        assert_eq!(front.extra.len(), 2);
    }

    #[test]
    fn parse_scalar_and_null_typed_fields() {
        let (front, diag) = parse_front("title: 2026\nauthor: null\ndate: true\n");
        assert!(diag.is_none());
        assert_eq!(front.title.as_deref(), Some("2026"));
        assert!(front.author.is_none());
        assert_eq!(front.date.as_deref(), Some("true"));
        assert!(front.extra.is_empty());
    }

    #[test]
    fn parse_empty_and_comment_only() {
        let (front, diag) = parse_front("");
        assert!(diag.is_none());
        assert_eq!(front, FrontMatter::default());

        let (front, diag) = parse_front("   \n");
        assert!(diag.is_none());
        assert_eq!(front, FrontMatter::default());

        let (front, diag) = parse_front("# just a comment\n");
        assert!(diag.is_none());
        assert_eq!(front, FrontMatter::default());
    }

    #[test]
    fn malformed_yaml_warns_and_returns_empty_front_matter() {
        let (front, diag) = parse_front("title: [unterminated\n");
        let diag = diag.expect("malformed YAML must warn");
        assert_eq!(diag.code, DiagCode::FrontMatter);
        assert_eq!(diag.severity, mdx_core::Severity::Warning);
        assert!(diag.line.is_some());
        assert_eq!(front, FrontMatter::default());
    }

    #[test]
    fn non_mapping_front_matter_warns() {
        let (front, diag) = parse_front("just a scalar\n");
        let diag = diag.expect("non-mapping must warn");
        assert_eq!(diag.code, DiagCode::FrontMatter);
        assert_eq!(front, FrontMatter::default());
    }
}

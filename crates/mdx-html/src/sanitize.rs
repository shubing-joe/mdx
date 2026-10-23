//! Sanitiser for markup that is inlined verbatim into the HTML output.
//!
//! Two kinds of foreign markup reach the writer unchanged: mermaid SVG
//! rendered by `mdx-diagram` and raw HTML blocks captured by the parser. Both
//! are treated as untrusted, so `<script>` elements, `on*` event handler
//! attributes and script-bearing URLs are removed here. Styling survives
//! deliberately: `<style>` elements and `style` attributes are what makes
//! mermaid output readable, so neither is touched.

use crate::escape::{is_safe_img_url, is_safe_link_url};

/// Remove scriptable content from an HTML or SVG fragment.
///
/// Text outside tags, comments and everything that is not a script element or
/// a dangerous attribute is copied byte-for-byte, so sanitising an already
/// clean fragment is the identity.
pub(crate) fn sanitize_fragment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < input.len() {
        let Some(offset) = input[i..].find('<') else {
            out.push_str(&input[i..]);
            break;
        };
        let lt = i + offset;
        out.push_str(&input[i..lt]);
        let rest = &input[lt..];
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                Some(end) => {
                    out.push_str(&rest[..end + 3]);
                    i = lt + end + 3;
                }
                None => {
                    out.push_str(rest);
                    i = input.len();
                }
            }
            continue;
        }
        let Some(gt) = find_tag_end(input, lt) else {
            out.push_str(rest);
            break;
        };
        let tag = &input[lt..gt + 1];
        if !tag.starts_with("</") && tag_name(tag).eq_ignore_ascii_case("script") {
            i = if is_self_closing(tag) {
                gt + 1
            } else {
                find_closing_tag(input, gt + 1, "script").unwrap_or(input.len())
            };
            continue;
        }
        out.push_str(&sanitize_tag(tag));
        i = gt + 1;
    }
    out
}

/// Index of the `>` that closes the tag starting at `start`, ignoring quoted
/// `>` characters inside attribute values.
fn find_tag_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut i = start + 1;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => {
                if b == b'"' || b == b'\'' {
                    quote = Some(b);
                } else if b == b'>' {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Index just past the closing tag `</name …>` searched from `from`.
fn find_closing_tag(input: &str, from: usize, name: &str) -> Option<usize> {
    let hay = input.as_bytes();
    let needle = name.as_bytes();
    let mut i = from;
    while i + needle.len() + 2 <= hay.len() {
        if hay[i] == b'<' && hay[i + 1] == b'/' {
            let after = i + 2 + needle.len();
            if hay[i + 2..after].eq_ignore_ascii_case(needle) {
                let boundary = hay.get(after).copied();
                if matches!(boundary, Some(b) if b.is_ascii_whitespace() || b == b'>') {
                    let gt = hay[after..].iter().position(|b| *b == b'>')?;
                    return Some(after + gt + 1);
                }
            }
        }
        i += 1;
    }
    None
}

/// Element name of `tag` (which includes the surrounding `<` and `>`).
fn tag_name(tag: &str) -> &str {
    let body = tag.get(1..tag.len().saturating_sub(1)).unwrap_or("");
    let body = body.trim_start_matches('/');
    let end = body
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>' || c == '!')
        .unwrap_or(body.len());
    body.get(..end).unwrap_or("")
}

/// Whether the tag ends with the self-closing slash.
fn is_self_closing(tag: &str) -> bool {
    tag.len() >= 2 && tag[..tag.len() - 1].trim_end().ends_with('/')
}

/// Whether `name` is a syntactically valid attribute name.
fn valid_attr_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
}

/// Whether the attribute may carry a URL.
fn is_url_attribute(lower: &str) -> bool {
    matches!(
        lower,
        "href" | "src" | "xlink:href" | "action" | "formaction" | "poster" | "cite" | "data"
    )
}

/// Case-insensitive search for a `javascript:` scheme.
fn contains_javascript_scheme(value: &str) -> bool {
    const NEEDLE: &[u8] = b"javascript:";
    let hay = value.as_bytes();
    hay.windows(NEEDLE.len())
        .any(|w| w.eq_ignore_ascii_case(NEEDLE))
}

/// Drop offending attributes from one tag, returning it verbatim when clean.
fn sanitize_tag(tag: &str) -> String {
    if tag.len() < 2 || tag.starts_with("</") {
        return tag.to_string();
    }
    let bytes = tag.as_bytes();
    let end = tag.len() - 1; // index of the terminating '>'
    let mut p = 1usize;
    while p < end && !bytes[p].is_ascii_whitespace() && bytes[p] != b'/' {
        p += 1;
    }
    if p >= end {
        return tag.to_string();
    }
    let element = &tag[1..p];
    let self_closing = is_self_closing(tag);
    let mut kept: Vec<(&str, Option<String>)> = Vec::new();
    let mut needs_rewrite = false;
    let mut i = p;
    while i < end {
        while i < end && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= end {
            break;
        }
        let name_start = i;
        while i < end && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' && bytes[i] != b'/' {
            i += 1;
        }
        if name_start == i {
            i += 1;
            continue;
        }
        let name = &tag[name_start..i];
        let mut value: Option<(usize, usize, Option<u8>)> = None;
        while i < end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < end && bytes[i] == b'=' {
            i += 1;
            while i < end && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < end && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                let start = i + 1;
                i = start;
                while i < end && bytes[i] != quote {
                    i += 1;
                }
                value = Some((start, i, Some(quote)));
                if i < end {
                    i += 1;
                }
            } else {
                let start = i;
                while i < end && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' {
                    i += 1;
                }
                value = Some((start, i, None));
            }
        }
        let raw_value = value.map(|(s, e, _)| &tag[s..e]);
        let lower = name.to_ascii_lowercase();
        let remove = !valid_attr_name(name)
            || (lower.len() > 2 && lower.starts_with("on"))
            || (is_url_attribute(&lower)
                && raw_value.is_some_and(|v| !is_safe_link_url(v) && !is_safe_img_url(v)))
            || (lower == "style" && raw_value.is_some_and(contains_javascript_scheme));
        if remove {
            needs_rewrite = true;
            continue;
        }
        let requoted = match value {
            None => None,
            Some((s, e, quote)) => {
                let raw = &tag[s..e];
                // Values are copied verbatim (entities must not be double
                // escaped); only a switch to double quotes needs escaping.
                match quote {
                    Some(b'"') => Some(raw.to_string()),
                    _ => Some(raw.replace('"', "&quot;")),
                }
            }
        };
        kept.push((name, requoted));
    }
    if !needs_rewrite {
        return tag.to_string();
    }
    let mut out = String::with_capacity(tag.len());
    out.push('<');
    out.push_str(element);
    for (name, value) in kept {
        out.push(' ');
        out.push_str(name);
        if let Some(v) = value {
            out.push_str("=\"");
            out.push_str(&v);
            out.push('"');
        }
    }
    if self_closing {
        out.push_str(" /");
    }
    out.push('>');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_elements_but_keeps_style() {
        let input = "<svg><script type=\"text/javascript\">alert(1)</script><style>.a{fill:red}</style><rect/></svg>";
        let out = sanitize_fragment(input);
        assert!(!out.contains("<script"));
        assert!(!out.contains("alert(1)"));
        assert!(out.contains("<style>.a{fill:red}</style>"));
        assert!(out.contains("<rect/>"));
        assert!(out.starts_with("<svg>"));
        assert!(out.ends_with("</svg>"));
    }

    #[test]
    fn strips_self_closing_and_unterminated_scripts() {
        assert_eq!(
            sanitize_fragment("<svg><script src=\"x\"/><rect/></svg>"),
            "<svg><rect/></svg>"
        );
        assert_eq!(sanitize_fragment("<p>a</p><script>evil"), "<p>a</p>");
    }

    #[test]
    fn drops_event_handlers_and_script_urls() {
        let input = "<a xlink:href=\"javascript:alert(1)\" onclick=\"bad()\" href=\"#ok\">x</a>";
        let out = sanitize_fragment(input);
        assert!(!out.to_ascii_lowercase().contains("javascript:"));
        assert!(!out.to_ascii_lowercase().contains("onclick"));
        assert!(out.contains("href=\"#ok\""));
        assert!(out.contains(">x</a>"));
    }

    #[test]
    fn clean_markup_is_untouched() {
        let input = "<div class=\"callout\" data-x='1'><img src=\"data:image/png;base64,AA&quot;BB\"/></div>";
        assert_eq!(sanitize_fragment(input), input);
        assert_eq!(sanitize_fragment("a < b > c"), "a < b > c");
        assert_eq!(
            sanitize_fragment("<!-- <script> -->text"),
            "<!-- <script> -->text"
        );
    }
}

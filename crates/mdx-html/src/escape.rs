//! Text escaping, identifier slugs and URL sanitisation.
//!
//! Everything that reaches the HTML output passes through one of these
//! helpers: the document is untrusted input, so `<`, `>`, `&`, `"` and `'`
//! must never survive into markup, and `javascript:`-style URLs must never
//! reach an `href`/`src` attribute.

/// Escape `&`, `<`, `>`, `"` and `'`, appending to `out`.
///
/// The escaped form is valid both inside a text node and inside a
/// double-quoted attribute value.
pub(crate) fn escape_into(text: &str, out: &mut String) {
    out.reserve(text.len() + 8);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

/// Build a deterministic ASCII-ish identifier from arbitrary label text.
///
/// Alphanumeric characters (including CJK, which is valid in HTML5 `id`
/// values) are kept and ASCII letters lowercased; `-` and `_` are kept; every
/// other run of characters collapses into a single `-`. An empty result
/// becomes `x` so that generated ids are never empty.
pub(crate) fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(64));
    let mut last_dash = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if c == '-' || c == '_' {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push('x');
    }
    out
}

/// Compare `hay` against an ASCII `needle` prefix without allocating.
fn starts_with_ignore_ascii_case(hay: &str, needle: &str) -> bool {
    let hay = hay.as_bytes();
    let needle = needle.as_bytes();
    hay.len() >= needle.len() && hay[..needle.len()].eq_ignore_ascii_case(needle)
}

/// Strip leading/trailing whitespace and control characters.
///
/// Browsers ignore those before a URL scheme, so the check below must too.
fn stripped(url: &str) -> &str {
    url.trim_matches(|c: char| c.is_whitespace() || c.is_control())
}

/// Extract the URL scheme, if the string starts with `scheme:`.
fn scheme_of(url: &str) -> Option<String> {
    let url = stripped(url);
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

/// Schemes that can execute attacker-controlled code when linked to.
const DANGEROUS_SCHEMES: [&str; 3] = ["javascript", "vbscript", "data"];

/// Whether `url` may be used as a hyperlink target.
///
/// Relative references, fragments and ordinary `http(s)`/`mailto`/`tel` URLs
/// pass; `javascript:`, `vbscript:` and every `data:` URL are rejected.
pub(crate) fn is_safe_link_url(url: &str) -> bool {
    match scheme_of(url) {
        None => true,
        Some(scheme) => !DANGEROUS_SCHEMES.contains(&scheme.as_str()),
    }
}

/// Whether `url` may be used as an image `src`.
///
/// Same as [`is_safe_link_url`] except that `data:image/…` payloads are
/// allowed, since embedding resolved assets is the point of this crate.
pub(crate) fn is_safe_img_url(url: &str) -> bool {
    let url = stripped(url);
    match scheme_of(url) {
        None => true,
        Some(scheme) if scheme == "data" => starts_with_ignore_ascii_case(url, "data:image/"),
        Some(scheme) => !DANGEROUS_SCHEMES.contains(&scheme.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_all_five_characters() {
        let mut out = String::new();
        escape_into("<a href=\"x\">&'</a>", &mut out);
        assert_eq!(out, "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;");
    }

    #[test]
    fn slugify_is_deterministic() {
        assert_eq!(slugify("Note 1!"), "note-1");
        assert_eq!(slugify("a...b"), "a-b");
        assert_eq!(slugify("中文 标题"), "中文-标题");
        assert_eq!(slugify("..."), "x");
        assert_eq!(slugify("keep-under_score"), "keep-under_score");
    }

    #[test]
    fn link_urls_reject_script_schemes() {
        assert!(is_safe_link_url("https://example.com/a?b=1"));
        assert!(is_safe_link_url("#fragment"));
        assert!(is_safe_link_url("img/pic.png"));
        assert!(is_safe_link_url("mailto:a@b.c"));
        assert!(!is_safe_link_url("javascript:alert(1)"));
        assert!(!is_safe_link_url("  JaVaScRiPt:alert(1)"));
        assert!(!is_safe_link_url("data:text/html,<script>x</script>"));
        assert!(!is_safe_link_url("vbscript:msgbox(1)"));
    }

    #[test]
    fn image_urls_allow_embedded_images_only() {
        assert!(is_safe_img_url("data:image/png;base64,AAAA"));
        assert!(is_safe_img_url("data:IMAGE/SVG+xml;base64,AAAA"));
        assert!(is_safe_img_url("img/pic.png"));
        assert!(!is_safe_img_url("data:text/html,<script>x</script>"));
        assert!(!is_safe_img_url("javascript:alert(1)"));
    }
}

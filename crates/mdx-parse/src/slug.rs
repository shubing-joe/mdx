//! Heading ids: GitHub-style slugs plus duplicate disambiguation.
//!
//! The slugger is Unicode-aware rather than ASCII-only: CJK ideographs, kana,
//! hangul and other alphabetic characters survive verbatim, so a heading like
//! `中文 标题` keeps its own anchor instead of collapsing to an empty id.

use std::collections::HashSet;

/// Convert heading text into a GitHub-style anchor id.
///
/// * uppercase characters are lowercased (Unicode-aware),
/// * runs of whitespace collapse into a single `-`,
/// * `-` and `_` are kept,
/// * every other punctuation/symbol character is dropped,
/// * alphanumeric characters, including CJK, are kept as written.
///
/// Trailing `-` left behind by stripped punctuation is trimmed, so
/// `"Hello, World!"` becomes `hello-world`.
pub fn heading_id(text: &str) -> String {
    let mut out = String::with_capacity(text.len());

    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        } else if ch == '-' || ch == '_' {
            out.push(ch);
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

    out
}

/// Make `ids` unique by appending `-1`, `-2`, … to repeats.
///
/// The first occurrence keeps the original id; later ones get the smallest
/// numeric suffix that is still free, so a document already containing `intro-1`
/// never gains two anchors with the same id. Order is preserved and the result
/// depends only on the input.
pub fn dedupe(ids: Vec<String>) -> Vec<String> {
    let mut used: HashSet<String> = HashSet::with_capacity(ids.len());
    let mut out = Vec::with_capacity(ids.len());

    for id in ids {
        let mut candidate = id.clone();
        let mut suffix = 1u32;
        while !used.insert(candidate.clone()) {
            candidate = format!("{id}-{suffix}");
            suffix += 1;
        }
        out.push(candidate);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_replaces_spaces() {
        assert_eq!(heading_id("Hello World"), "hello-world");
        assert_eq!(
            heading_id("  Leading and   trailing  "),
            "leading-and-trailing"
        );
        assert_eq!(heading_id(""), "");
    }

    #[test]
    fn strips_punctuation_but_keeps_hyphen_and_underscore() {
        assert_eq!(heading_id("Hello, World!"), "hello-world");
        assert_eq!(heading_id("C++ & Rust"), "c-rust");
        assert_eq!(heading_id("Title: Sub Section"), "title-sub-section");
        assert_eq!(heading_id("keep_under-score"), "keep_under-score");
        assert_eq!(
            heading_id("(parens) [brackets] {braces}"),
            "parens-brackets-braces"
        );
        assert_eq!(heading_id("dash--dash"), "dash--dash");
    }

    #[test]
    fn cjk_headings_stay_intact() {
        assert_eq!(heading_id("中文 标题"), "中文-标题");
        assert_eq!(heading_id("汉字"), "汉字");
        assert_eq!(heading_id("日本語の見出し"), "日本語の見出し");
        assert_eq!(heading_id("Hello 世界!"), "hello-世界");
    }

    #[test]
    fn dedupe_appends_numeric_suffixes() {
        let ids = vec!["intro".to_owned(), "intro".to_owned(), "intro".to_owned()];
        assert_eq!(dedupe(ids), ["intro", "intro-1", "intro-2"]);
    }

    #[test]
    fn dedupe_avoids_collisions_with_existing_suffixed_ids() {
        let ids = vec!["a".to_owned(), "a".to_owned(), "a-1".to_owned()];
        assert_eq!(dedupe(ids), ["a", "a-1", "a-1-1"]);
    }

    #[test]
    fn dedupe_from_heading_ids() {
        let texts = ["Usage", "Usage", "Usage"];
        let ids = dedupe(texts.iter().map(|t| heading_id(t)).collect());
        assert_eq!(ids, ["usage", "usage-1", "usage-2"]);
    }

    #[test]
    fn dedupe_preserves_distinct_ids_in_order() {
        let ids = vec!["b".to_owned(), "a".to_owned(), "中文".to_owned()];
        assert_eq!(dedupe(ids), ["b", "a", "中文"]);
    }
}

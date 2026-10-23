//! Content-addressed cache keys and an in-memory memo for rendered diagrams.
//!
//! Rendering a Mermaid diagram is expensive relative to hashing its source, so
//! the renderer keys its work by a stable digest of the source text. This crate
//! deliberately carries no cryptographic hash dependency; [`key`] derives a
//! cheap, deterministic FNV-1a 64-bit digest and renders its leading 16 hex
//! digits, yielding fixed-width keys of exactly 20 characters (`mmd-` + 16).
//!
//! [`Memo`] is the matching store: a plain string map with sorted iteration so
//! that serialized output and diagnostics stay byte-stable across runs.

use std::collections::HashMap;
use std::fmt::Write as _;

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Hash `src` with FNV-1a (64-bit) and return the cache key.
///
/// The key is `"mmd-"` followed by the first 16 lowercase hex digits of the
/// digest, so every key is exactly 20 characters wide and stable for identical
/// input.
pub fn key(src: &str) -> String {
    let digest = fnv1a64(src.as_bytes());
    let mut out = String::with_capacity(4 + 16);
    out.push_str("mmd-");
    // `{:016x}` is always exactly 16 lowercase hex digits, so the key is 20
    // characters wide for every input.
    let _ = write!(out, "{digest:016x}");
    out
}

/// Raw FNV-1a 64-bit digest of `bytes`.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// In-memory memo mapping diagram cache keys to rendered SVG.
#[derive(Debug, Clone, Default)]
pub struct Memo {
    entries: HashMap<String, String>,
}

impl Memo {
    /// Create an empty memo.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Look up a previously rendered diagram by cache key.
    pub fn get(&self, key: &str) -> Option<&String> {
        self.entries.get(key)
    }

    /// Store a rendered diagram under `key`, replacing any previous value.
    pub fn put(&mut self, key: String, svg: String) {
        self.entries.insert(key, svg);
    }

    /// Number of memoized entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the memo holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remove every entry.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Iterate over `(key, svg)` pairs in ascending key order.
    ///
    /// Sorting is established here rather than inherited from the backing map
    /// so that callers can rely on identical ordering for identical contents.
    pub fn iter(&self) -> Vec<(&str, &str)> {
        let mut pairs: Vec<(&str, &str)> = self
            .entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_and_twenty_chars() {
        let k = key("graph TD; A-->B;");
        assert_eq!(k.len(), 20);
        assert!(k.starts_with("mmd-"));
        assert_eq!(k, key("graph TD; A-->B;"));
        assert!(k[4..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn key_distinguishes_sources() {
        assert_ne!(key("graph TD; A-->B;"), key("graph TD; A-->C;"));
    }

    #[test]
    fn memo_roundtrip_and_sorted_iter() {
        let mut memo = Memo::new();
        assert!(memo.get("mmd-0000000000000000").is_none());
        memo.put("mmd-zzzz".into(), "b".into());
        memo.put("mmd-aaaa".into(), "a".into());
        assert_eq!(memo.len(), 2);
        assert_eq!(memo.get("mmd-aaaa"), Some(&"a".to_string()));
        let pairs = memo.iter();
        assert_eq!(pairs, vec![("mmd-aaaa", "a"), ("mmd-zzzz", "b")]);
        memo.clear();
        assert!(memo.is_empty());
    }
}

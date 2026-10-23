//! Content-addressed cache keys, an in-memory memo, and the on-disk store for
//! rendered diagrams.
//!
//! Rendering a Mermaid diagram is expensive relative to hashing its source, so
//! the renderer keys its work by a stable digest of the source text. This crate
//! deliberately carries no cryptographic hash dependency; [`key`] derives a
//! cheap, deterministic FNV-1a 64-bit digest and renders its leading 16 hex
//! digits, yielding fixed-width keys of exactly 20 characters (`mmd-` + 16).
//!
//! Two stores share that key:
//!
//! * [`Memo`] is the in-process store: a plain string map with sorted iteration
//!   so that serialized output and diagnostics stay byte-stable across runs.
//! * [`load`] and [`store`] are the on-disk store, which survives a process
//!   restart, so a render, GUI preview, or second `mdx` run over the same
//!   document does not pay for the same diagram twice. Each entry is one file
//!   under [`dir`] holding a [`FORMAT_MARKER`] byte followed by the SVG body; a
//!   body whose marker byte differs reads as a miss, so changing the stored
//!   format invalidates old entries without a migration step.
//!
//! The on-disk store is advisory: [`load`] returns `None` for every failure and
//! [`store`] only reports one, so an unset, read-only, or full cache directory
//! can never fail a render. Entries are written atomically — a process-unique
//! temporary file is renamed onto the final name — so a concurrent reader only
//! ever sees a complete entry, and a failed write leaves none behind.

use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// One-byte format marker written ahead of every on-disk entry.
///
/// Bump it whenever the stored body changes shape (a new renderer, a new
/// wrapper); entries carrying an older marker read as a miss and are replaced by
/// the next render instead of being returned.
const FORMAT_MARKER: u8 = 1;

/// Extension of an on-disk entry, after the [`key`].
const ENTRY_EXTENSION: &str = "svg";

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

/// File name of the on-disk entry for `src`.
fn entry_name(src: &str) -> String {
    format!("{}.{ENTRY_EXTENSION}", key(src))
}

/// Directory holding cached diagrams, or `None` when none can be resolved.
///
/// A non-empty `MDX_CACHE_DIR` names the directory itself. Otherwise the store
/// follows the XDG Base Directory specification and lives at
/// `$XDG_CACHE_HOME/mdx/diagrams`, or `$HOME/.cache/mdx/diagrams` when
/// `XDG_CACHE_HOME` is unset. An empty variable is treated as unset, and when
/// neither variable yields a directory the cache is simply unavailable.
pub fn dir() -> Option<PathBuf> {
    resolve_dir(
        env::var_os("MDX_CACHE_DIR").as_deref(),
        env::var_os("XDG_CACHE_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

/// Resolution rule behind [`dir`], split out so it is testable without mutating
/// the process environment.
fn resolve_dir(
    mdx_cache_dir: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(explicit) = non_empty(mdx_cache_dir) {
        return Some(PathBuf::from(explicit));
    }
    if let Some(xdg) = non_empty(xdg_cache_home) {
        return Some(Path::new(xdg).join("mdx").join("diagrams"));
    }
    non_empty(home).map(|home| Path::new(home).join(".cache").join("mdx").join("diagrams"))
}

/// `Some(value)` when `value` is set and not empty, as the XDG specification
/// requires.
fn non_empty(value: Option<&OsStr>) -> Option<&OsStr> {
    value.filter(|candidate| !candidate.is_empty())
}

/// Reads the cached SVG for `src`, or `None` on any miss.
///
/// A miss is reported for an unavailable cache directory, a missing entry, an
/// entry whose first byte is not [`FORMAT_MARKER`], and a body that is not
/// UTF-8. Callers render normally after a miss.
pub fn load(src: &str) -> Option<String> {
    load_from(&dir()?, src)
}

/// Reads the cached SVG for `src` from `dir`, or `None` on any miss.
pub fn load_from(dir: &Path, src: &str) -> Option<String> {
    let bytes = fs::read(dir.join(entry_name(src))).ok()?;
    let (marker, body) = bytes.split_first()?;
    if *marker != FORMAT_MARKER {
        return None;
    }
    std::str::from_utf8(body).ok().map(str::to_owned)
}

/// Writes `svg` as the cached entry for `src`, creating [`dir`] as needed.
///
/// # Errors
///
/// Reports the underlying filesystem failure, or `NotFound` when no cache
/// directory can be resolved. The cache is advisory, so a caller may ignore it.
pub fn store(src: &str, svg: &str) -> io::Result<()> {
    let dir = dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no diagram cache directory"))?;
    store_in(&dir, src, svg)
}

/// Writes `svg` as the cached entry for `src` inside `dir`.
///
/// The entry appears at its final name only through a rename of a process-unique
/// temporary file, so a reader never observes a partial body and a failed write
/// leaves the previous entry untouched.
pub fn store_in(dir: &Path, src: &str, svg: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let name = entry_name(src);
    let target = dir.join(&name);
    let temporary = dir.join(format!(
        "{name}.tmp-{}-{}",
        std::process::id(),
        next_serial()
    ));

    if let Err(failure) = write_entry(&temporary, svg) {
        let _ = fs::remove_file(&temporary);
        return Err(failure);
    }
    if let Err(failure) = fs::rename(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        return Err(failure);
    }
    Ok(())
}

/// Writes one entry body: the format marker followed by the SVG.
fn write_entry(path: &Path, svg: &str) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(&[FORMAT_MARKER])?;
    file.write_all(svg.as_bytes())?;
    file.flush()
}

/// Process-unique suffix for temporary files, so two threads writing the same
/// diagram never share a temporary path.
fn next_serial() -> u64 {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    SERIAL.fetch_add(1, Ordering::Relaxed)
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

    /// A scratch directory outside the user's cache, unique to one test.
    fn scratch_dir(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "mdx-diagram-cache-{}-{}-{name}",
            std::process::id(),
            next_serial()
        ))
    }

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

    #[test]
    fn directory_resolution_prefers_the_explicit_cache_dir() {
        let explicit = OsStr::new("/tmp/mdx-explicit");
        let xdg = OsStr::new("/tmp/mdx-xdg");
        let home = OsStr::new("/tmp/mdx-home");

        assert_eq!(
            resolve_dir(Some(explicit), Some(xdg), Some(home)),
            Some(PathBuf::from("/tmp/mdx-explicit"))
        );
        assert_eq!(
            resolve_dir(None, Some(xdg), Some(home)),
            Some(PathBuf::from("/tmp/mdx-xdg/mdx/diagrams"))
        );
        assert_eq!(
            resolve_dir(None, None, Some(home)),
            Some(PathBuf::from("/tmp/mdx-home/.cache/mdx/diagrams"))
        );
        // Empty variables are unset, and with none set the cache is unavailable.
        assert_eq!(resolve_dir(Some(OsStr::new("")), None, None), None);
        assert_eq!(resolve_dir(None, None, None), None);
    }

    #[test]
    fn disk_roundtrip_through_a_temporary_directory() {
        let dir = scratch_dir("roundtrip");
        let source = "flowchart TD\nA --> B";
        let other = "flowchart TD\nA --> C";

        // Nothing is stored yet, so both sources are misses.
        assert_eq!(load_from(&dir, source), None);
        assert_eq!(load_from(&dir, other), None);

        assert!(store_in(&dir, source, "<svg>body</svg>").is_ok());
        assert_eq!(load_from(&dir, source), Some("<svg>body</svg>".to_string()));
        // Each source owns its own entry.
        assert_eq!(load_from(&dir, other), None);
        assert!(store_in(&dir, other, "<svg>other</svg>").is_ok());
        assert_eq!(load_from(&dir, other), Some("<svg>other</svg>".to_string()));

        // Storing again replaces the entry in place, and only the two final
        // entries remain: no temporary file survives a write.
        assert!(store_in(&dir, source, "<svg>replaced</svg>").is_ok());
        assert_eq!(
            load_from(&dir, source),
            Some("<svg>replaced</svg>".to_string())
        );
        assert_eq!(
            fs::read_dir(&dir)
                .map(|entries| entries.filter_map(Result::ok).count())
                .ok(),
            Some(2)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_cache_misses_on_a_corrupt_entry() {
        let dir = scratch_dir("corrupt");
        let source = "sequenceDiagram\nA->>B: hi";
        assert!(fs::create_dir_all(&dir).is_ok());
        let entry = dir.join(entry_name(source));

        // A foreign marker byte is a miss, not a hit.
        assert!(fs::write(&entry, [0x00, b'<', b's', b'v', b'g']).is_ok());
        assert_eq!(load_from(&dir, source), None);

        // So is a correctly marked body that is not UTF-8.
        assert!(fs::write(&entry, [FORMAT_MARKER, 0xff, 0xfe, 0xfd]).is_ok());
        assert_eq!(load_from(&dir, source), None);

        // An empty file, and a source that was never stored, are misses too.
        assert!(fs::write(&entry, b"").is_ok());
        assert_eq!(load_from(&dir, source), None);
        assert_eq!(load_from(&dir, "never stored"), None);

        let _ = fs::remove_dir_all(&dir);
    }
}

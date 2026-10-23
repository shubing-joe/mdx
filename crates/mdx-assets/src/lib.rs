//! Asset resolution for the `mdx` toolchain.
//!
//! `mdx-parse` keeps every image reference exactly as written and leaves its
//! [`mdx_core::AssetId`] empty; this crate is the only place that touches the
//! filesystem afterwards. [`resolve_all`] walks a [`mdx_core::Doc`] and fills
//! `Block::Image::asset` / `Inline::Image::asset` in place — plus the intrinsic
//! pixel size of a `Block::Image` the document did not size itself — by
//! resolving each `src` (relative path, absolute path, `file:`, `data:` or —
//! only when the policy allows it — `http(s)`) against an [`AssetPolicy`];
//! [`resolve_one`] exposes the same resolution for a single reference.
//!
//! Assets are content addressed: the payload is hashed with SHA-256 and the id
//! is `img-<sha256[..12]>`, so identical bytes always share one [`AssetStore`]
//! entry and store iteration is deterministic (sorted by id).

pub mod encode;
pub mod manifest;
pub mod policy;
pub mod resolve;

pub use resolve::{resolve_all, resolve_one};

use std::path::PathBuf;

use mdx_core::{AssetId, AssetKind};

/// Byte ceiling a single asset may reach under [`AssetPolicy::default`].
pub const DEFAULT_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// What the resolver is allowed to read, and how much of it.
///
/// The default is offline-safe: remote references are refused unless
/// [`AssetPolicy::allow_remote`] is set, and [`AssetPolicy::offline`] refuses
/// them even then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetPolicy {
    /// Refuse every remote reference, never opening a socket and never reading
    /// [`AssetPolicy::cache_dir`].
    pub offline: bool,
    /// Master switch for `http(s)://` references.
    pub allow_remote: bool,
    /// Directory holding the payloads of remote references, keyed by
    /// `sha256_hex(url)`. A cached payload is used without touching the
    /// network and reported as `W_ASSET_REMOTE_CACHED`.
    pub cache_dir: Option<PathBuf>,
    /// Largest accepted asset in bytes; `0` means no cap.
    pub max_bytes: u64,
}

impl Default for AssetPolicy {
    /// Remote references refused, no cache directory, [`DEFAULT_MAX_BYTES`] cap.
    fn default() -> Self {
        Self {
            offline: false,
            allow_remote: false,
            cache_dir: None,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// One resolved payload, keyed by content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// Content-addressed id: `img-<sha256[..12]>`.
    pub id: AssetId,
    /// What the payload is used for.
    pub kind: AssetKind,
    /// Original `src` as written in the document.
    pub origin: String,
    /// Media type, lowercased and without parameters.
    pub mime: String,
    /// The payload itself.
    pub bytes: Vec<u8>,
    /// Full SHA-256 of [`Asset::bytes`], lowercase hex.
    pub sha256: String,
    /// Pixel width, for payloads that decode as raster images.
    pub width: Option<u32>,
    /// Pixel height, for payloads that decode as raster images.
    pub height: Option<u32>,
}

/// Content-addressed assets of one document, deduplicated by id.
///
/// Iteration is sorted by [`AssetId`], so every manifest and every writer sees
/// the same order for the same document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetStore {
    /// Kept sorted by `id`, which is what makes [`AssetStore::iter`]
    /// deterministic and [`AssetStore::get`] a binary search.
    assets: Vec<Asset>,
}

impl AssetStore {
    /// An empty store.
    pub fn new() -> Self {
        Self { assets: Vec::new() }
    }

    /// The asset with this id, if it was resolved.
    pub fn get(&self, id: &AssetId) -> Option<&Asset> {
        let key = id.as_str();
        match self
            .assets
            .binary_search_by(|stored| stored.id.as_str().cmp(key))
        {
            Ok(index) => self.assets.get(index),
            Err(_) => None,
        }
    }

    /// Every asset, sorted by id.
    pub fn iter(&self) -> impl Iterator<Item = &Asset> {
        self.assets.iter()
    }

    /// How many distinct payloads the document uses.
    pub fn len(&self) -> usize {
        self.assets.len()
    }

    /// Whether the document references no asset at all.
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// Store `asset` unless its content is already present.
    ///
    /// An id is derived from the payload digest, so an existing entry with the
    /// same id holds identical bytes; the first one wins and the returned id is
    /// always the stored one.
    pub fn insert(&mut self, asset: Asset) -> AssetId {
        let key = asset.id.clone();
        match self
            .assets
            .binary_search_by(|stored| stored.id.as_str().cmp(key.as_str()))
        {
            Ok(_) => key,
            Err(index) => {
                self.assets.insert(index, asset);
                key
            }
        }
    }

    /// Manifest for this store (`mdx-assets/1`).
    ///
    /// Describes each payload — id, kind, origin, media type, digest, byte
    /// length and pixel size — but never embeds the payload itself.
    pub fn to_json(&self) -> serde_json::Value {
        manifest::manifest_json(self.iter())
    }
}

impl FromIterator<Asset> for AssetStore {
    /// Collect assets into a store; duplicate payloads collapse into one entry,
    /// exactly as with [`AssetStore::insert`].
    fn from_iter<I: IntoIterator<Item = Asset>>(iter: I) -> Self {
        let mut store = Self::new();
        store.extend(iter);
        store
    }
}

impl Extend<Asset> for AssetStore {
    /// Add assets one by one, keeping the store sorted and deduplicated.
    fn extend<I: IntoIterator<Item = Asset>>(&mut self, iter: I) {
        for asset in iter {
            self.insert(asset);
        }
    }
}

//! Manifest serialisation for resolved assets (schema `mdx-assets/1`) and the
//! content-addressed id derivation.
//!
//! The manifest is the machine-readable companion of a render: it lists every
//! asset with its identity, provenance and size so downstream tools (CLI
//! `--json`, MCP, tests) can compare builds without touching the payloads.
//! Payload bytes are never embedded — `bytes` is the payload *length*.
//!
//! Serialisation is deterministic: [`manifest_json`] sorts entries by id and
//! `serde_json` orders object keys, so equal stores always render equal JSON.

use crate::Asset;
use serde_json::{Value, json};

/// Schema tag of the manifest this module emits. Consumers must reject unknown
/// values rather than guessing.
pub const SCHEMA: &str = "mdx-assets/1";

/// Number of leading hex characters of the SHA-256 digest kept in an
/// [`mdx_core::AssetId`].
pub const ID_HEX_LEN: usize = 12;

/// Build the content-addressed id of an asset: `img-<sha256[..12]>`.
///
/// `sha` is used verbatim, so pass the lowercase digest produced by
/// [`crate::encode::sha256_hex`]. Digests shorter than [`ID_HEX_LEN`] are not
/// padded; the function never panics on unexpected input.
pub fn make_id(sha: &str) -> mdx_core::AssetId {
    let mut id = String::with_capacity("img-".len() + ID_HEX_LEN);
    id.push_str("img-");
    id.extend(sha.chars().take(ID_HEX_LEN));
    mdx_core::AssetId(id)
}

/// Serialise one asset as a manifest object.
///
/// Fields: `id`, `kind`, `origin`, `mime`, `sha256`, `width`, `height`,
/// `bytes`. `width`/`height` are `null` when the format carries no dimensions,
/// and `bytes` is the payload length in bytes (manifests never embed payloads).
pub fn asset_json(asset: &Asset) -> Value {
    let id = asset.id.as_str();
    let origin = asset.origin.as_str();
    let mime = asset.mime.as_str();
    let sha256 = asset.sha256.as_str();
    json!({
        "id": id,
        "kind": asset.kind,
        "origin": origin,
        "mime": mime,
        "sha256": sha256,
        "width": asset.width,
        "height": asset.height,
        "bytes": asset.bytes.len(),
    })
}

/// Serialise a whole asset set: `{"schema": "mdx-assets/1", "assets": [...]}`.
///
/// Entries are sorted by id, so iteration order of the source store does not
/// leak into the manifest.
pub fn manifest_json<'a>(assets: impl IntoIterator<Item = &'a Asset>) -> Value {
    let mut sorted: Vec<&Asset> = assets.into_iter().collect();
    sorted.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let entries: Vec<Value> = sorted.iter().map(|asset| asset_json(asset)).collect();
    json!({
        "schema": SCHEMA,
        "assets": entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdx_core::AssetKind;

    const SHA_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn sample() -> Asset {
        Asset {
            id: make_id(SHA_ABC),
            kind: AssetKind::Image,
            origin: "img/logo.png".to_string(),
            mime: "image/png".to_string(),
            bytes: vec![0u8; 7],
            sha256: SHA_ABC.to_string(),
            width: Some(16),
            height: None,
        }
    }

    #[test]
    fn make_id_keeps_first_twelve_hex_chars() {
        assert_eq!(make_id(SHA_ABC).as_str(), "img-ba7816bf8f01");
        assert_eq!(make_id("0123456789abcdef").as_str(), "img-0123456789ab");
    }

    #[test]
    fn make_id_is_total_for_short_and_empty_digests() {
        assert_eq!(make_id("abc").as_str(), "img-abc");
        assert_eq!(make_id("").as_str(), "img-");
    }

    #[test]
    fn make_id_is_stable_for_equal_content() {
        assert_eq!(make_id(SHA_ABC), make_id(SHA_ABC));
        assert_ne!(
            make_id(SHA_ABC),
            make_id("ba7816bf8f02cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn asset_json_carries_the_contract_fields() {
        let value = asset_json(&sample());
        assert_eq!(value["id"], "img-ba7816bf8f01");
        assert_eq!(value["kind"], "image");
        assert_eq!(value["origin"], "img/logo.png");
        assert_eq!(value["mime"], "image/png");
        assert_eq!(value["sha256"], SHA_ABC);
        assert_eq!(value["width"], json!(16));
        assert!(value["height"].is_null());
        // Payload length, never the payload itself.
        assert_eq!(value["bytes"], json!(7));
        assert_eq!(value.as_object().map(serde_json::Map::len), Some(8));
    }

    #[test]
    fn asset_json_serialises_every_kind() {
        let mut asset = sample();
        asset.kind = AssetKind::Font;
        assert_eq!(asset_json(&asset)["kind"], "font");
        asset.kind = AssetKind::Other;
        assert_eq!(asset_json(&asset)["kind"], "other");
    }

    #[test]
    fn manifest_json_is_tagged_and_sorted_by_id() {
        let first = sample();
        let mut second = sample();
        second.id = make_id("ffffffffffffffff");
        second.origin = "z/last.png".to_string();

        let value = manifest_json([&second, &first]);
        assert_eq!(value["schema"], "mdx-assets/1");
        assert_eq!(value["schema"], SCHEMA);
        let assets = value["assets"].as_array().map(Vec::len);
        assert_eq!(assets, Some(2));
        assert_eq!(value["assets"][0]["id"], "img-ba7816bf8f01");
        assert_eq!(value["assets"][1]["id"], "img-ffffffffffff");
    }

    #[test]
    fn manifest_json_of_no_assets_is_still_valid() {
        let value = manifest_json(Vec::<&Asset>::new());
        assert_eq!(value["schema"], SCHEMA);
        assert_eq!(value["assets"].as_array().map(Vec::len), Some(0));
    }
}

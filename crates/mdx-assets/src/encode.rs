//! Byte-level asset helpers: content hashing, `data:` URI codecs and MIME
//! sniffing.
//!
//! Every helper here is deterministic and total: no clock, no random state and
//! no panics on malformed input — [`decode_data_uri`] reports bad payloads
//! through a typed `Err(String)` instead.
//!
//! MIME resolution is ordered deliberately:
//! 1. magic bytes (authoritative — the content decides, not the name),
//! 2. `mime_guess` by file extension,
//! 3. [`OCTET_STREAM`].

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_PAD_INDIFFERENT};
use sha2::{Digest as _, Sha256};

/// Fallback MIME type used when neither the magic bytes nor the file extension
/// say anything useful.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// Maximum number of leading bytes inspected when guessing SVG from text.
const SVG_PROBE_LEN: usize = 1024;

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

/// Lowercase hexadecimal SHA-256 digest of `bytes` (exactly 64 characters).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for &byte in digest.iter() {
        out.push(HEX_LOWER[(byte >> 4) as usize] as char);
        out.push(HEX_LOWER[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Encode `bytes` as a `data:<mime>;base64,<payload>` URI.
///
/// The MIME type is used verbatim; an empty `mime` produces the still-valid
/// `data:;base64,…` form.
pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    let encoded = STANDARD.encode(bytes);
    let mut uri =
        String::with_capacity("data:".len() + mime.len() + ";base64,".len() + encoded.len());
    uri.push_str("data:");
    uri.push_str(mime);
    uri.push_str(";base64,");
    uri.push_str(&encoded);
    uri
}

/// Decode a `data:` URI into its declared MIME type and raw payload bytes.
///
/// Both RFC 2397 forms are supported:
/// * `data:image/png;base64,iVBORw0…` — the `base64` parameter is matched
///   case-insensitively and ASCII whitespace inside the payload is ignored;
///   padding is optional.
/// * `data:text/plain,hello%20world` — percent-encoded payloads.
///
/// The returned MIME type is the media type as written (lowercased, without
/// trailing `;param=value` parameters) and may be empty for `data:,x`, in which
/// case the caller should sniff the bytes.
pub fn decode_data_uri(uri: &str) -> Result<(String, Vec<u8>), String> {
    let rest = uri
        .strip_prefix("data:")
        .or_else(|| uri.strip_prefix("DATA:"))
        .ok_or_else(|| format!("not a data: URI: {uri}"))?;
    let (meta, payload) = rest
        .split_once(',')
        .ok_or_else(|| "data: URI is missing its ',' separator".to_string())?;

    let mut params = meta.split(';');
    let mime = params.next().unwrap_or("").trim().to_ascii_lowercase();
    let is_base64 = params.any(|param| param.trim().eq_ignore_ascii_case("base64"));

    if is_base64 {
        // Line-wrapped base64 is common in hand-written Markdown.
        let compact: String = payload
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        let bytes = STANDARD_PAD_INDIFFERENT
            .decode(compact.as_bytes())
            .map_err(|err| format!("invalid base64 payload in data: URI: {err}"))?;
        Ok((mime, bytes))
    } else {
        Ok((mime, percent_decode(payload)?))
    }
}

/// Guess a MIME type for an asset from its bytes and its name.
///
/// `path_or_name` may be a bare file name, a relative/absolute path or a URL —
/// only its extension is consulted. Magic bytes always win over the name, so a
/// PNG stored as `photo.jpg` is reported as `image/png`. When nothing matches,
/// the result is [`OCTET_STREAM`].
pub fn sniff_mime(path_or_name: &str, bytes: &[u8]) -> String {
    if let Some(mime) = sniff_magic(bytes) {
        return mime.to_string();
    }
    mime_guess::from_path(path_or_name)
        .first_raw()
        .unwrap_or(OCTET_STREAM)
        .to_string()
}

/// Recognize the handful of formats this crate can decode or embed.
fn sniff_magic(bytes: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.starts_with(PNG) {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    if looks_like_svg(bytes) {
        return Some("image/svg+xml");
    }
    None
}

/// Textual SVG detection: optional UTF-8 BOM and leading whitespace, then
/// either a literal `<svg` root or an XML prolog followed by one.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let head = body.get(..body.len().min(SVG_PROBE_LEN)).unwrap_or(body);
    let head = head.trim_ascii_start();

    if starts_with_ignore_ascii_case(head, b"<svg") {
        return true;
    }
    starts_with_ignore_ascii_case(head, b"<?xml")
        && head.windows(4).any(|w| w.eq_ignore_ascii_case(b"<svg"))
}

fn starts_with_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}

/// Percent-decode the payload part of a non-base64 `data:` URI.
///
/// Input is `&str`, so every non-escaped byte is already valid UTF-8; escapes
/// may introduce arbitrary bytes, which is why the output is a `Vec<u8>`.
fn percent_decode(payload: &str) -> Result<Vec<u8>, String> {
    let bytes = payload.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("truncated percent escape in data: URI payload".to_string());
            }
            let hi = hex_value(bytes[i + 1]).ok_or_else(|| {
                format!("invalid percent escape at byte {i} in data: URI payload")
            })?;
            let lo = hex_value(bytes[i + 2]).ok_or_else(|| {
                format!("invalid percent escape at byte {i} in data: URI payload")
            })?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors cross-checked with `sha256sum`.
    const SHA_EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const SHA_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const SHA_HELLO: &str = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
    const SHA_NIST: &str = "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1";
    const SHA_BINARY: &str = "2da45f2cd1f9c8e69a67abf7a6b26c282533d0a7686787a9533265418680d4d2";

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(sha256_hex(b""), SHA_EMPTY);
        assert_eq!(sha256_hex(b"abc"), SHA_ABC);
        assert_eq!(sha256_hex(b"hello world"), SHA_HELLO);
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            SHA_NIST
        );
        // Arbitrary binary input, not only ASCII.
        assert_eq!(sha256_hex(&[0x00, 0xff, 0x10]), SHA_BINARY);
    }

    #[test]
    fn sha256_is_lowercase_hex_of_fixed_width() {
        let hex = sha256_hex(b"abc");
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn data_uri_roundtrips() {
        let payload = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\xff\x00 binary";
        let uri = data_uri("image/png", payload);
        assert!(uri.starts_with("data:image/png;base64,"));
        assert_eq!(
            decode_data_uri(&uri),
            Ok(("image/png".to_string(), payload.to_vec()))
        );
    }

    #[test]
    fn data_uri_roundtrips_empty_payload() {
        let uri = data_uri("application/octet-stream", b"");
        assert_eq!(uri, "data:application/octet-stream;base64,");
        assert_eq!(
            decode_data_uri(&uri),
            Ok(("application/octet-stream".to_string(), Vec::new()))
        );
    }

    #[test]
    fn data_uri_decodes_percent_encoded_payloads() {
        assert_eq!(
            decode_data_uri("data:text/plain,hello%20world"),
            Ok(("text/plain".to_string(), b"hello world".to_vec()))
        );
        assert_eq!(
            decode_data_uri("data:image/svg+xml,%3Csvg%2F%3E"),
            Ok(("image/svg+xml".to_string(), b"<svg/>".to_vec()))
        );
        assert_eq!(
            decode_data_uri("data:,hi"),
            Ok((String::new(), b"hi".to_vec()))
        );
    }

    #[test]
    fn data_uri_tolerates_whitespace_case_and_missing_padding() {
        assert_eq!(
            decode_data_uri("data:text/plain;base64,aGVsbG8gd29ybGQ\n="),
            Ok(("text/plain".to_string(), b"hello world".to_vec()))
        );
        assert_eq!(
            decode_data_uri("data:text/plain;BASE64,aGVsbG8"),
            Ok(("text/plain".to_string(), b"hello".to_vec()))
        );
        assert_eq!(
            decode_data_uri("data:IMAGE/PNG;base64,aGVsbG8="),
            Ok(("image/png".to_string(), b"hello".to_vec()))
        );
    }

    #[test]
    fn data_uri_rejects_malformed_input() {
        assert!(decode_data_uri("https://example.com/a.png").is_err());
        assert!(decode_data_uri("data:image/png;base64").is_err());
        assert!(decode_data_uri("data:image/png;base64,!!!!").is_err());
        assert!(decode_data_uri("data:text/plain,%zz").is_err());
        assert!(decode_data_uri("data:text/plain,a%2").is_err());
    }

    #[test]
    fn sniffs_png_jpeg_gif_and_webp_magic() {
        assert_eq!(
            sniff_mime("whatever.bin", b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"),
            "image/png"
        );
        assert_eq!(
            sniff_mime("photo.jpg", b"\xff\xd8\xff\xe0\x00\x10JFIF"),
            "image/jpeg"
        );
        assert_eq!(sniff_mime("anim.bin", b"GIF89a\x01\x00"), "image/gif");
        let webp = b"RIFF\x00\x00\x00\x00WEBPVP8 ";
        assert_eq!(sniff_mime("img.bin", webp), "image/webp");
    }

    #[test]
    fn magic_beats_a_lying_extension() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        assert_eq!(sniff_mime("photo.jpg", png), "image/png");
        assert_eq!(sniff_mime("vector.txt", b"%PDF-1.7\n"), "application/pdf");
    }

    #[test]
    fn sniff_falls_back_to_extension_then_octet_stream() {
        assert_eq!(sniff_mime("icons/logo.svg", b""), "image/svg+xml");
        assert_eq!(sniff_mime("docs/paper.pdf", b""), "application/pdf");
        assert_eq!(sniff_mime("notes.txt", b"plain text"), "text/plain");
        assert_eq!(sniff_mime("blob", b"\x01\x02\x03"), OCTET_STREAM);
    }

    #[test]
    fn sniffs_svg_from_text_forms() {
        assert_eq!(
            sniff_mime(
                "inline",
                b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"
            ),
            "image/svg+xml"
        );
        assert_eq!(
            sniff_mime("inline", b"\xef\xbb\xbf\n  <SVG></SVG>"),
            "image/svg+xml"
        );
        assert_eq!(
            sniff_mime("inline", b"<?xml version=\"1.0\"?>\n<svg/>"),
            "image/svg+xml"
        );
        assert_eq!(sniff_mime("inline.txt", b"not markup at all"), "text/plain");
    }
}

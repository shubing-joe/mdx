//! Turning one image `src` into an [`Asset`].
//!
//! # Sources
//!
//! | `src` | handling |
//! |---|---|
//! | `data:…` | payload decoded in place (base64 or percent encoded), declared media type kept as a hint |
//! | `file:…` | percent-decoded, then read like a path |
//! | `http://…` | needs `policy.allow_remote && !policy.offline`, otherwise `E_ASSET_OFFLINE`; a payload cached under `policy.cache_dir` wins, otherwise the URL is fetched |
//! | `https://…` | same gate; no TLS backend is linked into this crate, so only a cached payload resolves |
//! | any other scheme | `E_ASSET_UNSUPPORTED` |
//! | absolute or relative path | relative paths resolve against `doc_dir`; a missing file is `E_ASSET_MISSING` |
//!
//! # Kind, media type and pixel size
//!
//! The media type comes from [`crate::encode::sniff_mime`] (magic bytes first,
//! then the file name or URL path extension), falling back to the media type
//! the source declared. Payloads whose header identifies a raster format this
//! build can read (PNG, JPEG, GIF, WebP, TIFF, plus BMP when the media type
//! says so) are probed for their pixel dimensions; a payload that only *claims*
//! to be such an image is rejected with `E_ASSET_UNSUPPORTED`, as is a payload
//! above `policy.max_bytes`.
//!
//! # Failure policy
//!
//! Reading never panics and never aborts a document walk: every failure becomes
//! a [`Diagnostic`] carrying the raw `src` in `Diagnostic::asset`, and the
//! reference keeps `asset: None`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mdx_core::{AssetId, AssetKind, Block, DiagCode, Diagnostic, Doc, Inline, Report};

use crate::{Asset, AssetPolicy, AssetStore, encode, manifest, policy};

/// Connect, read and write timeout for the gated remote path.
///
/// Applied per socket operation: a peer that stalls is dropped after this long,
/// and redirects plus the byte cap bound everything else.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(20);
/// Redirect hops followed before a remote reference is rejected.
const MAX_REDIRECTS: usize = 3;
/// Largest response head (status line plus headers) accepted from a host.
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// Media type assumed for a `data:` URI that declares none (RFC 2397).
const DATA_DEFAULT_MIME: &str = "text/plain";
/// Media types whose payload must decode as a raster image.
const RASTER_MIMES: [&str; 10] = [
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/pjpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/x-ms-bmp",
    "image/tiff",
    "image/x-tiff",
];
/// Media types that mark a payload as a font.
const FONT_MIMES: [&str; 7] = [
    "font/ttf",
    "font/otf",
    "font/woff",
    "font/woff2",
    "application/font-woff",
    "application/font-sfnt",
    "application/vnd.ms-fontobject",
];
/// File extensions that mark a payload as a font.
const FONT_EXTENSIONS: [&str; 6] = [".ttf", ".otf", ".ttc", ".woff", ".woff2", ".eot"];
/// Schemes recognized even without a `//` after the colon. Anything else needs
/// `//` to count, so a path such as `notes:2026.png` is not read as a URI.
const BARE_SCHEMES: [&str; 11] = [
    "data",
    "file",
    "http",
    "https",
    "ftp",
    "ftps",
    "ws",
    "wss",
    "mailto",
    "javascript",
    "blob",
];

/// Resolve a single reference.
///
/// Non-fatal findings that only carry a position in the document
/// (`W_ASSET_REMOTE_CACHED`) are dropped; use [`resolve_all`] to see them.
pub fn resolve_one(src: &str, doc_dir: &Path, policy: &AssetPolicy) -> Result<Asset, Diagnostic> {
    resolve_asset(src, doc_dir, policy).map(|(asset, _warnings)| asset)
}

/// Resolve every image reference of `doc` in place.
///
/// Returns the deduplicated store plus one diagnostic per reference that could
/// not be resolved; those references keep `asset: None`. The store's iteration
/// order is sorted by id, and identical payloads — however many references
/// point at them — occupy exactly one entry. `Block::Image` widths and heights
/// that the document left empty are filled in from the payload.
pub fn resolve_all(doc: &mut Doc, doc_dir: &Path, policy: &AssetPolicy) -> (AssetStore, Report) {
    let mut walker = Walker::new(doc_dir, policy);
    let mut report = Report::default();
    walker.blocks(&mut doc.blocks, &mut report);
    (walker.store, report)
}

/// Read `src` and build its asset, together with the warnings raised on the way.
fn resolve_asset(
    src: &str,
    doc_dir: &Path,
    policy: &AssetPolicy,
) -> Result<(Asset, Vec<Diagnostic>), Diagnostic> {
    let spec = src.trim();
    if spec.is_empty() {
        return Err(missing(src, "empty image source"));
    }
    let loaded = load(spec, doc_dir, policy)?;
    policy::check(loaded.bytes.len() as u64, policy, loaded.remote)
        .map_err(|diagnostic| diagnostic.for_asset(src))?;
    let mime = effective_mime(loaded.declared_mime.as_deref(), &loaded.name, &loaded.bytes);
    let (kind, width, height) = classify(src, &mime, &loaded.name, &loaded.bytes)?;
    let sha256 = encode::sha256_hex(&loaded.bytes);
    let asset = Asset {
        id: manifest::make_id(&sha256),
        kind,
        origin: src.to_string(),
        mime,
        bytes: loaded.bytes,
        sha256,
        width,
        height,
    };
    Ok((asset, loaded.warnings))
}

/// A source that has been read but not interpreted yet.
struct Loaded {
    /// Payload exactly as stored at the source.
    bytes: Vec<u8>,
    /// Media type the source itself declared (`data:` metadata, HTTP
    /// `Content-Type`), when it declared one.
    declared_mime: Option<String>,
    /// File name or URL path used as the extension hint for mime sniffing.
    name: String,
    /// Non-fatal findings, currently `W_ASSET_REMOTE_CACHED`.
    warnings: Vec<Diagnostic>,
    /// Whether the payload came from the network.
    remote: bool,
}

/// Where a `src` points once its scheme has been classified.
enum Source<'a> {
    /// A whole `data:` URI.
    Data(&'a str),
    /// Everything after `file:`.
    FileUri(&'a str),
    /// A whole `http(s)://` URL.
    Remote(&'a str),
    /// A filesystem path, relative or absolute.
    Path(&'a str),
    /// A URI scheme without a handler here.
    Scheme(String),
}

/// Read the payload of `src` without interpreting it as an image yet.
fn load(src: &str, doc_dir: &Path, policy: &AssetPolicy) -> Result<Loaded, Diagnostic> {
    match classify_source(src) {
        Source::Data(uri) => {
            let (mime, bytes) = encode::decode_data_uri(uri)
                .map_err(|error| unsupported(src, format!("invalid data: URI ({error})")))?;
            let declared_mime = if mime.trim().is_empty() {
                DATA_DEFAULT_MIME.to_string()
            } else {
                mime
            };
            Ok(Loaded {
                bytes,
                declared_mime: Some(declared_mime),
                name: String::new(),
                warnings: Vec::new(),
                remote: false,
            })
        }
        Source::FileUri(rest) => {
            let path = resolve_path(&file_uri_path(rest), doc_dir);
            let bytes = read_local(&path, src, policy)?;
            Ok(Loaded {
                bytes,
                declared_mime: None,
                name: path.to_string_lossy().into_owned(),
                warnings: Vec::new(),
                remote: false,
            })
        }
        Source::Path(relative_or_absolute) => {
            let path = resolve_path(Path::new(relative_or_absolute), doc_dir);
            let bytes = read_local(&path, src, policy)?;
            Ok(Loaded {
                bytes,
                declared_mime: None,
                name: path.to_string_lossy().into_owned(),
                warnings: Vec::new(),
                remote: false,
            })
        }
        Source::Remote(url) => load_remote(url, src, policy),
        Source::Scheme(scheme) => Err(unsupported(
            src,
            format!("unsupported image source scheme `{scheme}:`"),
        )),
    }
}

/// Read a local file, refusing directories and files the policy already knows
/// are too large.
fn read_local(path: &Path, origin: &str, policy: &AssetPolicy) -> Result<Vec<u8>, Diagnostic> {
    let metadata = fs::metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            missing(origin, format!("{} does not exist", path.display()))
        }
        _ => io_error(
            origin,
            format!("cannot inspect {}: {error}", path.display()),
        ),
    })?;
    if metadata.is_dir() {
        return Err(unsupported(
            origin,
            format!("{} is a directory", path.display()),
        ));
    }
    policy::check(metadata.len(), policy, false)
        .map_err(|diagnostic| diagnostic.for_asset(origin))?;
    fs::read(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            missing(origin, format!("{} does not exist", path.display()))
        }
        _ => io_error(origin, format!("cannot read {}: {error}", path.display())),
    })
}

/// Relative references are read from `doc_dir`, absolute ones as written.
fn resolve_path(path: &Path, doc_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        doc_dir.join(path)
    }
}

/// Resolve a `http(s)://` reference.
///
/// The policy gate runs first, so an offline document opens neither a socket
/// nor the cache. A payload cached as `<cache_dir>/<sha256_hex(url)>.bin` (or
/// without the extension) is used as is; otherwise `http://` is fetched and
/// `https://` is reported as unsupported.
fn load_remote(url: &str, origin: &str, policy: &AssetPolicy) -> Result<Loaded, Diagnostic> {
    policy::check(0, policy, true).map_err(|diagnostic| diagnostic.for_asset(origin))?;
    let name = remote_path(url);
    if let Some(cache_dir) = &policy.cache_dir
        && let Some(bytes) = read_cache(cache_dir, url)
    {
        let warning = Diagnostic::warning(
            DiagCode::AssetRemoteCached,
            format!("using the cached copy of `{url}`"),
        )
        .for_asset(origin);
        return Ok(Loaded {
            bytes,
            declared_mime: None,
            name,
            warnings: vec![warning],
            remote: true,
        });
    }
    match scheme_of(url).as_deref() {
        Some("http") => {
            let (bytes, content_type) =
                http_get(url, policy.max_bytes).map_err(|error| error.into_diagnostic(origin))?;
            Ok(Loaded {
                bytes,
                declared_mime: content_type,
                name,
                warnings: Vec::new(),
                remote: true,
            })
        }
        Some("https") => Err(unsupported(
            origin,
            format!(
                "`{url}` cannot be fetched because mdx-assets links no TLS backend; \
                 reference the payload by path, or put it in the cache directory as `{}`",
                encode::sha256_hex(url.as_bytes())
            ),
        )),
        _ => Err(unsupported(
            origin,
            format!("`{url}` is not a URL mdx-assets can fetch"),
        )),
    }
}

/// Look for a payload cached by [`AssetPolicy::cache_dir`]; `sha256_hex(url)`
/// is the key, stored either bare or with a `.bin` suffix.
fn read_cache(cache_dir: &Path, url: &str) -> Option<Vec<u8>> {
    let key = encode::sha256_hex(url.as_bytes());
    for name in [format!("{key}.bin"), key] {
        if let Ok(bytes) = fs::read(cache_dir.join(name)) {
            return Some(bytes);
        }
    }
    None
}

/// Media type of a payload: magic bytes and name first, declared type last.
fn effective_mime(declared: Option<&str>, name: &str, bytes: &[u8]) -> String {
    let sniffed = encode::sniff_mime(name, bytes);
    if sniffed != encode::OCTET_STREAM {
        return normalize_mime(&sniffed);
    }
    match declared.map(normalize_mime).filter(|mime| !mime.is_empty()) {
        Some(mime) => mime,
        None => encode::OCTET_STREAM.to_string(),
    }
}

/// Lowercase a media type and drop its parameters (`text/html; charset=utf-8`
/// keeps `text/html`).
fn normalize_mime(mime: &str) -> String {
    let head = mime.split(';').next().unwrap_or(mime);
    head.trim().to_ascii_lowercase()
}

/// Decide the asset kind and, for raster images, the pixel dimensions.
fn classify(
    origin: &str,
    mime: &str,
    name: &str,
    bytes: &[u8],
) -> Result<(AssetKind, Option<u32>, Option<u32>), Diagnostic> {
    if let Some(format) = raster_magic(bytes) {
        let (width, height) = probe_dimensions(bytes, format).map_err(|error| {
            unsupported(
                origin,
                format!("cannot decode {} image: {error}", format.to_mime_type()),
            )
        })?;
        return Ok((AssetKind::Image, Some(width), Some(height)));
    }
    if is_raster_mime(mime) {
        let (width, height) = probe_declared(bytes).map_err(|error| {
            unsupported(origin, format!("`{mime}` payload does not decode: {error}"))
        })?;
        return Ok((AssetKind::Image, Some(width), Some(height)));
    }
    if mime.starts_with("image/") {
        // Vector image (typically SVG): an image without pixel dimensions.
        return Ok((AssetKind::Image, None, None));
    }
    if is_font(mime, name) {
        return Ok((AssetKind::Font, None, None));
    }
    Ok((AssetKind::Other, None, None))
}

/// Raster format of `bytes` when the header is unambiguous and this build can
/// decode it.
///
/// BMP is left out on purpose: its two byte magic (`BM`) is weak enough to
/// occur in plain text, so BMP payloads are only probed when their media type
/// says they are images.
fn raster_magic(bytes: &[u8]) -> Option<image::ImageFormat> {
    let format = image::guess_format(bytes).ok()?;
    if !format.reading_enabled() || matches!(format, image::ImageFormat::Bmp) {
        return None;
    }
    Some(format)
}

/// Header-only probe for a known format; the pixel buffer is never allocated.
fn probe_dimensions(bytes: &[u8], format: image::ImageFormat) -> Result<(u32, u32), String> {
    image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|error| error.to_string())
}

/// Probe dimensions, letting the reader sniff the format from the payload.
fn probe_declared(bytes: &[u8]) -> Result<(u32, u32), String> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?
        .into_dimensions()
        .map_err(|error| error.to_string())
}

/// Whether a media type promises a raster image this crate must decode.
fn is_raster_mime(mime: &str) -> bool {
    RASTER_MIMES.contains(&mime)
}

/// Whether a payload is a font, judged by media type first and by file
/// extension second (`mime_guess` reports `.otf` as `application/font-sfnt`).
fn is_font(mime: &str, name: &str) -> bool {
    if mime.starts_with("font/") || FONT_MIMES.contains(&mime) {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    FONT_EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension))
}

/// Classify `src` by its scheme.
fn classify_source(src: &str) -> Source<'_> {
    let Some(scheme) = scheme_of_source(src) else {
        return Source::Path(src);
    };
    if scheme == "data" {
        return Source::Data(src);
    }
    if scheme == "file" {
        return match src.split_once(':') {
            Some((_, rest)) => Source::FileUri(rest),
            None => Source::Scheme(scheme),
        };
    }
    if scheme == "http" || scheme == "https" {
        return Source::Remote(src);
    }
    Source::Scheme(scheme)
}

/// Lowercased URI scheme of `src`, when it starts with one.
fn scheme_of_source(src: &str) -> Option<String> {
    let (scheme, rest) = src.split_once(':')?;
    // A single leading letter is a Windows drive, not a scheme.
    if scheme.len() < 2 {
        return None;
    }
    let mut chars = scheme.chars();
    if !chars.next()?.is_ascii_alphabetic() {
        return None;
    }
    if !chars
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.'))
    {
        return None;
    }
    let lower = scheme.to_ascii_lowercase();
    if !rest.starts_with("//") && !BARE_SCHEMES.contains(&lower.as_str()) {
        return None;
    }
    Some(lower)
}

/// Lowercased scheme of an absolute URL.
fn scheme_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

/// Convert the part of a `file:` URI after its scheme into a local path.
///
/// `file:///a/b`, `file:/a/b` and `file://localhost/a/b` all become `/a/b`;
/// any other authority is kept as a `//authority/path` path.
fn file_uri_path(rest: &str) -> PathBuf {
    let (authority, path) = match rest.strip_prefix("//") {
        Some(after) => match after.find('/') {
            Some(index) => (&after[..index], &after[index..]),
            None => (after, ""),
        },
        None => ("", rest),
    };
    let decoded = percent_decode_str(path);
    if authority.is_empty() || authority.eq_ignore_ascii_case("localhost") {
        PathBuf::from(decoded)
    } else {
        PathBuf::from(format!("//{authority}{decoded}"))
    }
}

/// Percent-decode `input`; invalid escapes (`%`, `%zz`, a trailing `%4`) are
/// kept verbatim instead of failing.
fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' {
            let high = input.get(index + 1).copied().and_then(hex_digit);
            let low = input.get(index + 2).copied().and_then(hex_digit);
            if let (Some(high), Some(low)) = (high, low) {
                output.push(high << 4 | low);
                index += 3;
                continue;
            }
        }
        output.push(input[index]);
        index += 1;
    }
    output
}

/// Percent-decode a borrowed string, replacing invalid UTF-8 rather than failing.
fn percent_decode_str(input: &str) -> String {
    String::from_utf8_lossy(&percent_decode(input.as_bytes())).into_owned()
}

/// Value of one hex digit.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// A repository-wide reference that could not be resolved.
fn missing(origin: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(DiagCode::AssetMissing, message).for_asset(origin)
}

/// A payload the resolver cannot use, whatever the reason.
fn unsupported(origin: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(DiagCode::AssetUnsupported, message).for_asset(origin)
}

/// A local read failure that is not a missing file.
fn io_error(origin: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(DiagCode::Io, message).for_asset(origin)
}

/// Depth-first walk that fills every image reference of a document.
struct Walker<'a> {
    doc_dir: &'a Path,
    policy: &'a AssetPolicy,
    store: AssetStore,
    /// One outcome per distinct `src`, so a repeated reference is read once.
    seen: BTreeMap<String, Seen>,
}

/// Memoised outcome of [`Walker::resolve`].
#[derive(Clone)]
enum Seen {
    /// Stored id plus the warnings raised while reading the payload.
    Resolved(AssetId, Vec<Diagnostic>),
    /// The diagnostic that rejected the reference.
    Failed(Diagnostic),
}

impl<'a> Walker<'a> {
    /// A walker over an empty store.
    fn new(doc_dir: &'a Path, policy: &'a AssetPolicy) -> Self {
        Self {
            doc_dir,
            policy,
            store: AssetStore::new(),
            seen: BTreeMap::new(),
        }
    }

    /// Resolve `src` once per document and replay the outcome afterwards.
    fn resolve(&mut self, src: &str) -> Seen {
        if let Some(seen) = self.seen.get(src) {
            return seen.clone();
        }
        let outcome = match resolve_asset(src, self.doc_dir, self.policy) {
            Ok((asset, warnings)) => {
                let id = self.store.insert(asset);
                Seen::Resolved(id, warnings)
            }
            Err(diagnostic) => Seen::Failed(diagnostic),
        };
        self.seen.insert(src.to_string(), outcome.clone());
        outcome
    }

    /// Walk a block list, including nested lists and quotes.
    fn blocks(&mut self, blocks: &mut [Block], report: &mut Report) {
        for block in blocks.iter_mut() {
            match block {
                Block::Image {
                    asset,
                    src,
                    width,
                    height,
                    ..
                } => {
                    // `width`/`height` are only ever filled in, never
                    // overwritten, so a size declared by the document survives.
                    if let Some(id) = self.fill(asset, src, report)
                        && let Some(resolved) = self.store.get(&id)
                    {
                        if width.is_none() {
                            *width = resolved.width;
                        }
                        if height.is_none() {
                            *height = resolved.height;
                        }
                    }
                }
                Block::Paragraph { inlines } | Block::Heading { inlines, .. } => {
                    self.inlines(inlines, report);
                }
                Block::List { items, .. } => {
                    for item in items.iter_mut() {
                        self.blocks(item, report);
                    }
                }
                Block::BlockQuote { blocks } => self.blocks(blocks, report),
                Block::FootnoteDef { blocks, .. } => self.blocks(blocks, report),
                Block::Table { header, rows, .. } => {
                    for cell in header.iter_mut() {
                        self.inlines(cell, report);
                    }
                    for row in rows.iter_mut() {
                        for cell in row.iter_mut() {
                            self.inlines(cell, report);
                        }
                    }
                }
                Block::Code { .. }
                | Block::Math { .. }
                | Block::Diagram { .. }
                | Block::ThematicBreak
                | Block::Html { .. } => {}
            }
        }
    }

    /// Walk inline content, descending into emphasis, links and the like.
    fn inlines(&mut self, inlines: &mut [Inline], report: &mut Report) {
        for inline in inlines.iter_mut() {
            match inline {
                Inline::Image { asset, src, .. } => {
                    let _ = self.fill(asset, src, report);
                }
                Inline::Emph { inlines }
                | Inline::Strong { inlines }
                | Inline::Strike { inlines }
                | Inline::Link { inlines, .. } => self.inlines(inlines, report),
                Inline::Text { .. }
                | Inline::Code { .. }
                | Inline::Math { .. }
                | Inline::FootnoteRef { .. }
                | Inline::LineBreak { .. } => {}
            }
        }
    }

    /// Fill one reference, or record why it could not be filled.
    ///
    /// Returns the stored id when the reference resolved, so callers can pick
    /// up the payload's intrinsic size.
    fn fill(
        &mut self,
        asset: &mut Option<AssetId>,
        src: &str,
        report: &mut Report,
    ) -> Option<AssetId> {
        match self.resolve(src) {
            Seen::Resolved(id, warnings) => {
                for warning in warnings {
                    report.push(warning);
                }
                *asset = Some(id.clone());
                Some(id)
            }
            Seen::Failed(diagnostic) => {
                *asset = None;
                report.push(diagnostic);
                None
            }
        }
    }
}

/// Why the gated remote path could not produce a payload.
#[derive(Debug)]
enum RemoteError {
    /// This build can never fetch this URL (no TLS backend, malformed URL).
    Unsupported(String),
    /// The host refused, misbehaved or was unreachable.
    Transport(String),
    /// The host answered that the payload does not exist.
    Missing(String),
    /// The payload is larger than the policy allows.
    TooLarge(String),
}

impl RemoteError {
    /// Map the failure onto the contract's diagnostic codes.
    fn into_diagnostic(self, origin: &str) -> Diagnostic {
        match self {
            RemoteError::Unsupported(message) => unsupported(origin, message),
            RemoteError::Missing(message) => missing(origin, message),
            RemoteError::TooLarge(message) => unsupported(origin, message),
            RemoteError::Transport(message) => io_error(origin, message),
        }
    }
}

/// Fetch an `http://` URL, following up to [`MAX_REDIRECTS`] redirects.
///
/// `max_bytes == 0` means "no cap"; a non-zero cap aborts the transfer as soon
/// as the payload exceeds it.
fn http_get(url: &str, max_bytes: u64) -> Result<(Vec<u8>, Option<String>), RemoteError> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let target = Url::parse(&current).ok_or_else(|| {
            RemoteError::Unsupported(format!("`{current}` is not a URL mdx-assets can fetch"))
        })?;
        if target.scheme != "http" {
            return Err(RemoteError::Unsupported(format!(
                "`{current}` redirects to `{}://`, which mdx-assets cannot fetch (no TLS backend)",
                target.scheme
            )));
        }
        let response = fetch_once(&target, max_bytes)?;
        match response.status {
            200..=299 => return Ok((response.body, response.content_type)),
            404 | 410 => {
                return Err(RemoteError::Missing(format!(
                    "`{current}` answered HTTP {}",
                    response.status
                )));
            }
            301 | 302 | 303 | 307 | 308 => {
                let location = response.location.ok_or_else(|| {
                    RemoteError::Transport(format!(
                        "`{current}` redirected without a `Location` header"
                    ))
                })?;
                current = resolve_location(&target, &location);
            }
            status => {
                return Err(RemoteError::Transport(format!(
                    "`{current}` answered HTTP {status}"
                )));
            }
        }
    }
    Err(RemoteError::Transport(format!(
        "`{url}` redirected more than {MAX_REDIRECTS} times"
    )))
}

/// A parsed `scheme://authority/path` URL.
struct Url {
    /// Lowercased scheme.
    scheme: String,
    /// Authority without userinfo; used for the `Host` header and redirects.
    authority: String,
    /// Host, bracketed when it is an IPv6 literal.
    host: String,
    /// Effective port.
    port: u16,
    /// Path plus query, always starting with `/`.
    path: String,
}

impl Url {
    /// Parse the subset of RFC 3986 the asset fetcher needs.
    fn parse(raw: &str) -> Option<Url> {
        let (scheme, rest) = raw.split_once("://")?;
        let scheme = scheme.trim().to_ascii_lowercase();
        if scheme.is_empty() || rest.is_empty() {
            return None;
        }
        let rest = rest.split('#').next().unwrap_or(rest);
        let (authority, path) = match rest.find(['/', '?']) {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        let authority = match authority.rsplit_once('@') {
            Some((_, host)) => host,
            None => authority,
        };
        if authority.is_empty() {
            return None;
        }
        let (host, port) = split_authority(authority, &scheme)?;
        let path = if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        };
        Some(Url {
            scheme,
            authority: authority.to_string(),
            host,
            port,
            path,
        })
    }
}

/// Split `host[:port]`, with `[..]` around IPv6 literals, defaulting the port
/// from the scheme.
fn split_authority(authority: &str, scheme: &str) -> Option<(String, u16)> {
    let default_port = if scheme == "https" { 443 } else { 80 };
    if let Some(after_bracket) = authority.strip_prefix('[') {
        let (host, after) = after_bracket.split_once(']')?;
        let host = format!("[{host}]");
        return match after.strip_prefix(':') {
            Some(port) => Some((host, port.parse::<u16>().ok()?)),
            None if after.is_empty() => Some((host, default_port)),
            None => None,
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => {
            Some((host.to_string(), port.parse::<u16>().ok()?))
        }
        _ if !authority.is_empty() => Some((authority.to_string(), default_port)),
        _ => None,
    }
}

/// Path of a URL without its query, used as the extension hint when sniffing
/// the media type.
fn remote_path(url: &str) -> String {
    let path = match Url::parse(url) {
        Some(parsed) => parsed.path,
        None => url.to_string(),
    };
    match path.split_once('?') {
        Some((head, _)) => head.to_string(),
        None => path,
    }
}

/// Head plus, for successful responses, body.
struct Response {
    /// HTTP status code.
    status: u16,
    /// `Location` header, for redirects.
    location: Option<String>,
    /// `Content-Type` header, if any.
    content_type: Option<String>,
    /// Body, empty for non-2xx responses.
    body: Vec<u8>,
}

/// Perform one request/response exchange, reading the body only for 2xx.
fn fetch_once(target: &Url, max_bytes: u64) -> Result<Response, RemoteError> {
    let mut stream = TcpStream::connect((target.host.as_str(), target.port)).map_err(|error| {
        RemoteError::Transport(format!(
            "cannot connect to {}:{}: {error}",
            target.host, target.port
        ))
    })?;
    let _ = stream.set_read_timeout(Some(REMOTE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(REMOTE_TIMEOUT));
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: mdx-assets/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        target.path, target.authority
    );
    stream.write_all(request.as_bytes()).map_err(|error| {
        RemoteError::Transport(format!(
            "cannot send the request for `{}`: {error}",
            target.authority
        ))
    })?;

    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(end) = find_head_end(&buffer) {
            break end;
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(RemoteError::Transport(format!(
                "`{}` sent a response head above {MAX_HEAD_BYTES} bytes",
                target.authority
            )));
        }
        if read_some(&mut stream, &mut buffer)? == 0 {
            return Err(RemoteError::Transport(format!(
                "`{}` closed the connection before answering",
                target.authority
            )));
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end.saturating_sub(4)]).into_owned();
    let rest = buffer.split_off(head_end);
    let (status, headers) = parse_head(&head)?;
    let location = header(&headers, "location").map(str::to_string);
    let content_type = header(&headers, "content-type").map(normalize_mime);
    if !(200..300).contains(&status) {
        return Ok(Response {
            status,
            location,
            content_type,
            body: Vec::new(),
        });
    }
    let chunked = header(&headers, "transfer-encoding")
        .map(|value| value.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    // Transfer-Encoding wins over Content-Length when both are present.
    let content_length = if chunked {
        None
    } else {
        header(&headers, "content-length").and_then(|value| value.trim().parse::<u64>().ok())
    };
    let cap = if max_bytes == 0 { u64::MAX } else { max_bytes };
    let body = read_body(&mut stream, rest, content_length, chunked, cap)?;
    Ok(Response {
        status,
        location,
        content_type,
        body,
    })
}

/// Read the body of a 2xx response, honouring `Content-Length`, chunked
/// transfer encoding and the policy cap.
fn read_body(
    stream: &mut TcpStream,
    mut buffered: Vec<u8>,
    content_length: Option<u64>,
    chunked: bool,
    cap: u64,
) -> Result<Vec<u8>, RemoteError> {
    if let Some(length) = content_length {
        if length > cap {
            return Err(RemoteError::TooLarge(format!(
                "remote payload is {length} bytes, above the {cap} byte cap"
            )));
        }
        while (buffered.len() as u64) < length {
            if read_some(stream, &mut buffered)? == 0 {
                return Err(RemoteError::Transport(format!(
                    "response body is truncated: {length} bytes announced, {} received",
                    buffered.len()
                )));
            }
        }
        if let Ok(length) = usize::try_from(length) {
            buffered.truncate(length);
        }
        return Ok(buffered);
    }
    if chunked {
        return read_chunked(stream, buffered, cap);
    }
    loop {
        if buffered.len() as u64 > cap {
            return Err(RemoteError::TooLarge(format!(
                "remote payload exceeds the {cap} byte cap"
            )));
        }
        if read_some(stream, &mut buffered)? == 0 {
            return Ok(buffered);
        }
    }
}

/// Decode a `Transfer-Encoding: chunked` body.
fn read_chunked(
    stream: &mut TcpStream,
    mut buffered: Vec<u8>,
    cap: u64,
) -> Result<Vec<u8>, RemoteError> {
    let mut body = Vec::new();
    loop {
        let line_end = loop {
            if let Some(index) = find_crlf(&buffered) {
                break index;
            }
            if read_some(stream, &mut buffered)? == 0 {
                return Err(RemoteError::Transport(
                    "connection closed inside a chunked body".to_string(),
                ));
            }
        };
        let line = String::from_utf8_lossy(&buffered[..line_end]).into_owned();
        buffered.drain(..line_end + 2);
        let size_token = line.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size_token, 16)
            .map_err(|_| RemoteError::Transport(format!("malformed chunk size `{size_token}`")))?;
        if size == 0 {
            return Ok(body);
        }
        if size > cap {
            return Err(RemoteError::TooLarge(format!(
                "remote payload exceeds the {cap} byte cap"
            )));
        }
        while (buffered.len() as u64) < size.saturating_add(2) {
            if read_some(stream, &mut buffered)? == 0 {
                return Err(RemoteError::Transport(
                    "connection closed inside a chunk".to_string(),
                ));
            }
        }
        let size = usize::try_from(size).map_err(|_| {
            RemoteError::TooLarge("remote chunk is too large to buffer".to_string())
        })?;
        body.extend_from_slice(&buffered[..size]);
        buffered.drain(..size + 2);
        if body.len() as u64 > cap {
            return Err(RemoteError::TooLarge(format!(
                "remote payload exceeds the {cap} byte cap"
            )));
        }
    }
}

/// Append up to 8 KiB read from `stream` to `buffer`; `0` means the peer closed.
fn read_some(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Result<usize, RemoteError> {
    let mut chunk = [0u8; 8192];
    let read = stream
        .read(&mut chunk)
        .map_err(|error| RemoteError::Transport(format!("cannot read the response: {error}")))?;
    buffer.extend_from_slice(&chunk[..read]);
    Ok(read)
}

/// Index just past the blank line that ends a response head.
fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

/// Index of the next CRLF in `buffer`.
fn find_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\r\n")
}

/// Split a response head into its status code and its lowercased headers.
fn parse_head(head: &str) -> Result<(u16, Vec<(String, String)>), RemoteError> {
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status = status_code(status_line)
        .ok_or_else(|| RemoteError::Transport(format!("malformed status line `{status_line}`")))?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    Ok((status, headers))
}

/// `HTTP/1.1 200 OK` becomes `200`.
fn status_code(status_line: &str) -> Option<u16> {
    let mut parts = status_line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse::<u16>().ok()
}

/// First value of a lowercased header name.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// Resolve a `Location` header against the URL that answered.
fn resolve_location(current: &Url, location: &str) -> String {
    let location = location.trim();
    if location.contains("://") {
        return location.to_string();
    }
    if let Some(rest) = location.strip_prefix("//") {
        return format!("{}://{rest}", current.scheme);
    }
    let base = if location.starts_with('/') {
        String::new()
    } else {
        match current.path.rfind('/') {
            Some(index) => current.path[..=index].to_string(),
            None => "/".to_string(),
        }
    };
    format!("{}://{}{base}{location}", current.scheme, current.authority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode;

    /// Offline policy with no byte cap, so tests never reach the network.
    fn offline_policy() -> AssetPolicy {
        AssetPolicy {
            offline: true,
            allow_remote: false,
            cache_dir: None,
            max_bytes: 0,
        }
    }

    /// A directory that exists in no test environment.
    fn absent_dir() -> &'static Path {
        Path::new("/nonexistent-mdx-assets-test-dir")
    }

    /// Per-test scratch directory under the system temp dir.
    fn scratch_dir(name: &str) -> Result<PathBuf, String> {
        let dir = std::env::temp_dir().join(format!("mdx-assets-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        Ok(dir)
    }

    /// A PNG payload built through the encoder the resolver's decoder mirrors.
    fn png_bytes(width: u32, height: u32) -> Result<Vec<u8>, String> {
        let pixels = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_pixel(
            width,
            height,
            image::Rgba([10, 20, 30, 255]),
        );
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|error| error.to_string())?;
        Ok(png)
    }

    #[test]
    fn schemes_are_classified_without_mistaking_drive_letters() {
        assert!(matches!(classify_source("img/a.png"), Source::Path(_)));
        assert!(matches!(classify_source("./a.png"), Source::Path(_)));
        assert!(matches!(classify_source("/abs/a.png"), Source::Path(_)));
        assert!(matches!(classify_source("C:\\img.png"), Source::Path(_)));
        assert!(matches!(classify_source("notes:2026.png"), Source::Path(_)));
        assert!(matches!(
            classify_source("data:text/plain,hi"),
            Source::Data(_)
        ));
        assert!(matches!(
            classify_source("file:///tmp/a.png"),
            Source::FileUri(_)
        ));
        assert!(matches!(
            classify_source("https://example.com/a.png"),
            Source::Remote(_)
        ));
        assert!(matches!(
            classify_source("ftp://example.com/a.png"),
            Source::Scheme(_)
        ));
        assert!(matches!(
            classify_source("mailto:a@example.com"),
            Source::Scheme(_)
        ));
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(file_uri_path("///tmp/a.png"), PathBuf::from("/tmp/a.png"));
        assert_eq!(file_uri_path("/tmp/a.png"), PathBuf::from("/tmp/a.png"));
        assert_eq!(
            file_uri_path("//localhost/tmp/a%20b.png"),
            PathBuf::from("/tmp/a b.png")
        );
        assert_eq!(
            file_uri_path("//host/share/a.png"),
            PathBuf::from("//host/share/a.png")
        );
    }

    #[test]
    fn percent_decoding_keeps_invalid_escapes() {
        assert_eq!(percent_decode(b"a%20b").as_slice(), b"a b".as_slice());
        assert_eq!(percent_decode(b"100%").as_slice(), b"100%".as_slice());
        assert_eq!(percent_decode(b"%zz").as_slice(), b"%zz".as_slice());
        assert_eq!(percent_decode(b"%C3%A9").as_slice(), "é".as_bytes());
    }

    #[test]
    fn urls_are_split_into_host_port_and_path() -> Result<(), String> {
        let url = Url::parse("http://example.com:8080/a/b.png?x=1#frag")
            .ok_or_else(|| "URL should parse".to_string())?;
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 8080);
        assert_eq!(url.path, "/a/b.png?x=1");
        assert_eq!(url.authority, "example.com:8080");

        let defaulted =
            Url::parse("http://example.com").ok_or_else(|| "URL should parse".to_string())?;
        assert_eq!(defaulted.port, 80);
        assert_eq!(defaulted.path, "/");

        let literal =
            Url::parse("http://[::1]:9000/x").ok_or_else(|| "URL should parse".to_string())?;
        assert_eq!(literal.host, "[::1]");
        assert_eq!(literal.port, 9000);
        assert!(Url::parse("not a url").is_none());
        Ok(())
    }

    #[test]
    fn response_heads_are_parsed() -> Result<(), String> {
        let buffer = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 3\r\n\r\nabc";
        let end = find_head_end(buffer).ok_or_else(|| "head should end".to_string())?;
        let head = String::from_utf8_lossy(&buffer[..end - 4]).into_owned();
        let (status, headers) = parse_head(&head).map_err(|error| format!("{error:?}"))?;
        assert_eq!(status, 200);
        assert_eq!(
            header(&headers, "content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(header(&headers, "content-length"), Some("3"));
        assert_eq!(
            normalize_mime(header(&headers, "content-type").unwrap_or("")),
            "text/plain"
        );
        assert_eq!(&buffer[end..], b"abc");

        let current = Url::parse("http://example.com/a/b.png")
            .ok_or_else(|| "URL should parse".to_string())?;
        assert_eq!(
            resolve_location(&current, "/c.png"),
            "http://example.com/c.png"
        );
        assert_eq!(
            resolve_location(&current, "c.png"),
            "http://example.com/a/c.png"
        );
        assert_eq!(
            resolve_location(&current, "https://other/d.png"),
            "https://other/d.png"
        );
        Ok(())
    }

    #[test]
    fn missing_local_file_reports_and_leaves_the_reference_empty() -> Result<(), String> {
        let mut doc = Doc::new(vec![Block::Image {
            asset: None,
            src: "nope.png".to_string(),
            alt: "nope".to_string(),
            title: None,
            width: None,
            height: None,
        }]);
        let (store, report) = resolve_all(&mut doc, absent_dir(), &offline_policy());
        assert!(store.is_empty());
        assert_eq!(report.diagnostics.len(), 1);
        let diagnostic = report
            .diagnostics
            .first()
            .ok_or_else(|| "expected a diagnostic".to_string())?;
        assert_eq!(diagnostic.code, DiagCode::AssetMissing);
        assert_eq!(diagnostic.asset.as_deref(), Some("nope.png"));
        match doc.blocks.first() {
            Some(Block::Image { asset, .. }) => assert!(asset.is_none()),
            _ => return Err("image block disappeared".to_string()),
        }
        Ok(())
    }

    #[test]
    fn remote_reference_is_refused_when_offline() -> Result<(), String> {
        let diagnostic = resolve_one("https://example.com/a.png", absent_dir(), &offline_policy())
            .err()
            .ok_or_else(|| "offline policy must refuse remote references".to_string())?;
        assert_eq!(diagnostic.code, DiagCode::AssetOffline);
        assert_eq!(
            diagnostic.asset.as_deref(),
            Some("https://example.com/a.png")
        );

        let online = AssetPolicy {
            offline: false,
            allow_remote: false,
            cache_dir: None,
            max_bytes: 0,
        };
        let diagnostic = resolve_one("http://example.com/a.png", absent_dir(), &online)
            .err()
            .ok_or_else(|| "remote references need allow_remote".to_string())?;
        assert_eq!(diagnostic.code, DiagCode::AssetOffline);
        Ok(())
    }

    #[test]
    fn unsupported_scheme_is_reported() -> Result<(), String> {
        let diagnostic = resolve_one("ftp://example.com/a.png", absent_dir(), &offline_policy())
            .err()
            .ok_or_else(|| "ftp must not resolve".to_string())?;
        assert_eq!(diagnostic.code, DiagCode::AssetUnsupported);
        assert_eq!(diagnostic.asset.as_deref(), Some("ftp://example.com/a.png"));
        Ok(())
    }

    #[test]
    fn data_uri_payload_is_decoded() -> Result<(), String> {
        let asset = resolve_one(
            "data:text/plain,hello%20world",
            absent_dir(),
            &offline_policy(),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(asset.bytes.as_slice(), b"hello world".as_slice());
        assert_eq!(asset.mime, "text/plain");
        assert_eq!(asset.kind, AssetKind::Other);
        assert_eq!(asset.width, None);
        assert_eq!(
            asset.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(asset.id.as_str(), "img-b94d27b9934d");
        Ok(())
    }

    #[test]
    fn raster_payload_is_probed_for_its_size() -> Result<(), String> {
        let png = png_bytes(3, 2)?;
        let uri = encode::data_uri("image/png", &png);
        let asset = resolve_one(&uri, absent_dir(), &offline_policy())
            .map_err(|error| error.to_string())?;
        assert_eq!(asset.kind, AssetKind::Image);
        assert_eq!(asset.mime, "image/png");
        assert_eq!((asset.width, asset.height), (Some(3), Some(2)));

        // The same payload inside a document also sizes the image block.
        let expected_id = asset.id.clone();
        let mut doc = Doc::new(vec![Block::Image {
            asset: None,
            src: uri,
            alt: "pixels".to_string(),
            title: None,
            width: None,
            height: None,
        }]);
        let (store, report) = resolve_all(&mut doc, absent_dir(), &offline_policy());
        assert!(report.diagnostics.is_empty());
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.get(&expected_id).map(|stored| stored.sha256.as_str()),
            Some(asset.sha256.as_str())
        );
        match doc.blocks.first() {
            Some(Block::Image {
                asset,
                width,
                height,
                ..
            }) => {
                assert_eq!(asset.as_ref(), Some(&expected_id));
                assert_eq!((*width, *height), (Some(3), Some(2)));
            }
            _ => return Err("image block disappeared".to_string()),
        }
        Ok(())
    }

    #[test]
    fn relative_references_resolve_against_doc_dir() -> Result<(), String> {
        let dir = scratch_dir("relative")?;
        let png = png_bytes(3, 2)?;
        fs::write(dir.join("pic.png"), &png).map_err(|error| error.to_string())?;

        let asset =
            resolve_one("pic.png", &dir, &offline_policy()).map_err(|error| error.to_string())?;
        assert_eq!(asset.origin, "pic.png");
        assert_eq!(asset.mime, "image/png");
        assert_eq!(asset.kind, AssetKind::Image);
        assert_eq!((asset.width, asset.height), (Some(3), Some(2)));
        assert_eq!(asset.sha256, encode::sha256_hex(&png));

        // A sub-path and a file: URI reach the same bytes, hence the same id.
        fs::create_dir_all(dir.join("nested")).map_err(|error| error.to_string())?;
        fs::write(dir.join("nested").join("pic.png"), &png).map_err(|error| error.to_string())?;
        let nested = resolve_one("nested/pic.png", &dir, &offline_policy())
            .map_err(|error| error.to_string())?;
        assert_eq!(nested.id, asset.id);
        let uri = format!("file://{}/pic.png", dir.display());
        let via_uri =
            resolve_one(&uri, &dir, &offline_policy()).map_err(|error| error.to_string())?;
        assert_eq!(via_uri.id, asset.id);
        assert_eq!(via_uri.origin, uri);

        // A file that claims to be a PNG but does not decode is unsupported.
        fs::write(dir.join("broken.png"), b"not an image").map_err(|error| error.to_string())?;
        let diagnostic = resolve_one("broken.png", &dir, &offline_policy())
            .err()
            .ok_or_else(|| "broken.png must be refused".to_string())?;
        assert_eq!(diagnostic.code, DiagCode::AssetUnsupported);
        assert_eq!(diagnostic.asset.as_deref(), Some("broken.png"));

        // Oversized payloads are refused before they are read into memory.
        let capped = AssetPolicy {
            offline: true,
            allow_remote: false,
            cache_dir: None,
            max_bytes: 4,
        };
        let diagnostic = resolve_one("pic.png", &dir, &capped)
            .err()
            .ok_or_else(|| "oversized payload must be refused".to_string())?;
        assert_eq!(diagnostic.code, DiagCode::AssetUnsupported);

        let _ = fs::remove_file(dir.join("nested").join("pic.png"));
        let _ = fs::remove_dir(dir.join("nested"));
        let _ = fs::remove_file(dir.join("pic.png"));
        let _ = fs::remove_file(dir.join("broken.png"));
        let _ = fs::remove_dir(&dir);
        Ok(())
    }

    #[test]
    fn cached_remote_payload_is_used_without_network() -> Result<(), String> {
        let dir = scratch_dir("cache")?;
        // `.invalid` never resolves, so a cache hit is the only way this passes.
        let url = "https://example.invalid/host.png";
        let key = encode::sha256_hex(url.as_bytes());
        fs::write(dir.join(format!("{key}.bin")), png_bytes(4, 5)?)
            .map_err(|error| error.to_string())?;
        let policy = AssetPolicy {
            offline: false,
            allow_remote: true,
            cache_dir: Some(dir.clone()),
            max_bytes: 0,
        };
        let mut doc = Doc::new(vec![Block::Image {
            asset: None,
            src: url.to_string(),
            alt: String::new(),
            title: None,
            width: None,
            height: None,
        }]);
        let (store, report) = resolve_all(&mut doc, absent_dir(), &policy);
        assert!(!report.has_errors());
        assert!(report.has_code(DiagCode::AssetRemoteCached));
        let asset = store
            .iter()
            .next()
            .ok_or_else(|| "expected the cached asset".to_string())?;
        assert_eq!(asset.kind, AssetKind::Image);
        assert_eq!(asset.mime, "image/png");
        assert_eq!((asset.width, asset.height), (Some(4), Some(5)));
        assert_eq!(asset.origin, url);

        let _ = fs::remove_file(dir.join(format!("{key}.bin")));
        let _ = fs::remove_dir(&dir);
        Ok(())
    }

    #[test]
    fn declared_raster_that_does_not_decode_is_unsupported() -> Result<(), String> {
        let diagnostic = resolve_one(
            "data:image/png;base64,aGVsbG8gd29ybGQ=",
            absent_dir(),
            &offline_policy(),
        )
        .err()
        .ok_or_else(|| "undecodable image must be refused".to_string())?;
        assert_eq!(diagnostic.code, DiagCode::AssetUnsupported);
        Ok(())
    }

    #[test]
    fn identical_payloads_share_one_store_entry() -> Result<(), String> {
        let uri = "data:text/plain,hello%20world";
        let mut doc = Doc::new(vec![
            Block::Paragraph {
                inlines: vec![Inline::Image {
                    asset: None,
                    src: uri.to_string(),
                    alt: "a".to_string(),
                    title: None,
                }],
            },
            Block::Image {
                asset: None,
                src: uri.to_string(),
                alt: "b".to_string(),
                title: None,
                width: None,
                height: None,
            },
        ]);
        let (store, report) = resolve_all(&mut doc, absent_dir(), &offline_policy());
        assert!(report.diagnostics.is_empty());
        assert_eq!(store.len(), 1);
        let ids: Vec<&str> = store.iter().map(|asset| asset.id.as_str()).collect();
        assert_eq!(ids, vec!["img-b94d27b9934d"]);
        for block in &doc.blocks {
            match block {
                Block::Paragraph { inlines } => match inlines.first() {
                    Some(Inline::Image { asset, .. }) => {
                        assert_eq!(
                            asset.as_ref().map(AssetId::as_str),
                            Some("img-b94d27b9934d")
                        );
                    }
                    _ => return Err("inline image disappeared".to_string()),
                },
                Block::Image { asset, .. } => {
                    assert_eq!(
                        asset.as_ref().map(AssetId::as_str),
                        Some("img-b94d27b9934d")
                    );
                }
                _ => return Err("unexpected block".to_string()),
            }
        }
        Ok(())
    }

    #[test]
    fn store_iteration_is_sorted_by_id() {
        let mut store = AssetStore::new();
        for (id, digest) in [
            ("img-zzzzzzzzzzzz", "z"),
            ("img-aaaaaaaaaaaa", "a"),
            ("img-mmmmmmmmmmmm", "m"),
        ] {
            store.insert(Asset {
                id: AssetId(id.to_string()),
                kind: AssetKind::Other,
                origin: id.to_string(),
                mime: "application/octet-stream".to_string(),
                bytes: digest.as_bytes().to_vec(),
                sha256: digest.to_string(),
                width: None,
                height: None,
            });
        }
        let ids: Vec<&str> = store.iter().map(|asset| asset.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["img-aaaaaaaaaaaa", "img-mmmmmmmmmmmm", "img-zzzzzzzzzzzz"]
        );
        assert_eq!(store.len(), 3);
        assert!(
            store
                .get(&AssetId("img-mmmmmmmmmmmm".to_string()))
                .is_some()
        );
        assert!(
            store
                .get(&AssetId("img-000000000000".to_string()))
                .is_none()
        );
        let manifest = store.to_json();
        assert_eq!(manifest["schema"].as_str(), Some("mdx-assets/1"));
        assert_eq!(manifest["assets"].as_array().map(Vec::len), Some(3));
    }
}

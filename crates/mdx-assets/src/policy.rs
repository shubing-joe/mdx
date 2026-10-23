//! Asset admission policy: offline mode, remote gating and size caps.
//!
//! [`check`] is the single gate every payload passes before it enters an
//! [`crate::AssetStore`], so the rules live in exactly one place and cannot
//! drift between `resolve_one` and `resolve_all`. The policy itself
//! ([`crate::AssetPolicy`]) is defined in the crate root.
//!
//! `http(s)://` assets are the only reason `mdx-assets` ever touches the
//! network, so they are refused twice over: in `--offline` mode always, and
//! otherwise unless the caller opted in with `allow_remote`.

use crate::AssetPolicy;
use mdx_core::{DiagCode, Diagnostic};

/// Enforce `policy` for a payload of `bytes_len` bytes.
///
/// `remote` marks payloads that would require a network fetch.
///
/// * `remote && policy.offline` → `E_ASSET_OFFLINE`, even if `allow_remote`
///   is set: offline means offline.
/// * `remote && !policy.allow_remote` → `E_ASSET_OFFLINE`.
/// * `bytes_len > policy.max_bytes` → `E_ASSET_UNSUPPORTED`. The limit is
///   inclusive, and `max_bytes == 0` disables the cap entirely.
///
/// Local assets are never rejected for being remote, and are only subject to
/// the size cap. The returned [`Diagnostic`] deliberately carries no `asset`
/// field: the caller knows the originating `src` and attaches it with
/// [`Diagnostic::for_asset`], which keeps this gate independent of Markdown
/// positions.
pub fn check(bytes_len: u64, policy: &AssetPolicy, remote: bool) -> Result<(), Diagnostic> {
    if remote {
        if policy.offline {
            return Err(Diagnostic::error(
                DiagCode::AssetOffline,
                "remote asset refused: offline mode is enabled",
            ));
        }
        if !policy.allow_remote {
            return Err(Diagnostic::error(
                DiagCode::AssetOffline,
                "remote asset refused: remote assets are not allowed",
            ));
        }
    }

    if policy.max_bytes != 0 && bytes_len > policy.max_bytes {
        return Err(Diagnostic::error(
            DiagCode::AssetUnsupported,
            format!(
                "asset payload of {bytes_len} bytes exceeds the limit of {} bytes",
                policy.max_bytes
            ),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdx_core::Severity;

    fn policy(offline: bool, allow_remote: bool, max_bytes: u64) -> AssetPolicy {
        AssetPolicy {
            offline,
            allow_remote,
            cache_dir: None,
            max_bytes,
        }
    }

    #[test]
    fn local_assets_pass_and_the_limit_is_inclusive() {
        assert!(check(0, &policy(false, false, 100), false).is_ok());
        assert!(check(10, &policy(false, false, 100), false).is_ok());
        assert!(check(100, &policy(false, false, 100), false).is_ok());
        // An offline policy never blocks a local payload.
        assert!(check(42, &policy(true, false, 100), false).is_ok());
    }

    #[test]
    fn offline_rejects_remote_even_when_remote_is_allowed() {
        let err = check(1, &policy(true, true, 1024), true)
            .expect_err("offline policy must reject remote assets");
        assert_eq!(err.code, DiagCode::AssetOffline);
        assert_eq!(err.code.as_str(), "E_ASSET_OFFLINE");
        assert_eq!(err.severity, Severity::Error);
        assert!(err.asset.is_none());
    }

    #[test]
    fn remote_requires_explicit_opt_in() {
        let err = check(1, &policy(false, false, 1024), true)
            .expect_err("remote assets must be gated behind allow_remote");
        assert_eq!(err.code, DiagCode::AssetOffline);

        assert!(check(1, &policy(false, true, 1024), true).is_ok());
        assert!(check(1024, &policy(false, true, 1024), true).is_ok());
    }

    #[test]
    fn oversize_payloads_are_rejected() {
        let err = check(101, &policy(false, true, 100), false)
            .expect_err("payload above max_bytes must be rejected");
        assert_eq!(err.code, DiagCode::AssetUnsupported);
        assert_eq!(err.code.as_str(), "E_ASSET_UNSUPPORTED");
        assert!(err.message.contains("101"));
        assert!(err.message.contains("100"));
    }

    #[test]
    fn a_zero_cap_disables_the_size_limit() {
        assert!(check(u64::MAX, &policy(false, true, 0), false).is_ok());
        assert!(check(u64::MAX, &policy(false, true, 0), true).is_ok());
    }

    #[test]
    fn offline_wins_over_the_size_cap() {
        let err = check(u64::MAX, &policy(true, true, 1), true)
            .expect_err("offline must be reported before the size cap");
        assert_eq!(err.code, DiagCode::AssetOffline);
    }
}

//! Process exit codes (stable contract).
//!
//! | code | meaning                                  |
//! |------|------------------------------------------|
//! | 0    | success                                  |
//! | 1    | render/parse failure                     |
//! | 2    | usage/config error                       |
//! | 3    | unresolved assets (missing/offline/type) |

use crate::{DiagCode, Report};

pub const OK: i32 = 0;
pub const FAILED: i32 = 1;
pub const USAGE: i32 = 2;
pub const ASSETS: i32 = 3;

pub fn exit_code(report: &Report) -> i32 {
    if !report.has_errors() {
        return OK;
    }
    let asset_codes = [
        DiagCode::AssetMissing,
        DiagCode::AssetUnsupported,
        DiagCode::AssetOffline,
    ];
    if report.diagnostics.iter().any(|d| asset_codes.contains(&d.code)) {
        ASSETS
    } else {
        FAILED
    }
}

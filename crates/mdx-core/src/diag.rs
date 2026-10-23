//! Diagnostics with stable codes. Codes are part of the CLI/MCP contract.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// Stable machine-readable codes. Never rename; add only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagCode {
    #[serde(rename = "E_PARSE")]
    Parse,
    #[serde(rename = "E_ASSET_MISSING")]
    AssetMissing,
    #[serde(rename = "E_ASSET_UNSUPPORTED")]
    AssetUnsupported,
    #[serde(rename = "E_ASSET_OFFLINE")]
    AssetOffline,
    #[serde(rename = "E_MATH_CONVERT")]
    MathConvert,
    #[serde(rename = "E_DIAGRAM_PARSE")]
    DiagramParse,
    #[serde(rename = "E_DIAGRAM_RENDER")]
    DiagramRender,
    #[serde(rename = "E_TYPST")]
    Typst,
    #[serde(rename = "E_IO")]
    Io,
    #[serde(rename = "E_CONFIG")]
    Config,
    #[serde(rename = "W_FRONTMATTER")]
    FrontMatter,
    #[serde(rename = "W_UNSUPPORTED_HTML")]
    UnsupportedHtml,
    #[serde(rename = "W_ASSET_REMOTE_CACHED")]
    AssetRemoteCached,
}

impl DiagCode {
    pub fn as_str(self) -> &'static str {
        match self {
            DiagCode::Parse => "E_PARSE",
            DiagCode::AssetMissing => "E_ASSET_MISSING",
            DiagCode::AssetUnsupported => "E_ASSET_UNSUPPORTED",
            DiagCode::AssetOffline => "E_ASSET_OFFLINE",
            DiagCode::MathConvert => "E_MATH_CONVERT",
            DiagCode::DiagramParse => "E_DIAGRAM_PARSE",
            DiagCode::DiagramRender => "E_DIAGRAM_RENDER",
            DiagCode::Typst => "E_TYPST",
            DiagCode::Io => "E_IO",
            DiagCode::Config => "E_CONFIG",
            DiagCode::FrontMatter => "W_FRONTMATTER",
            DiagCode::UnsupportedHtml => "W_UNSUPPORTED_HTML",
            DiagCode::AssetRemoteCached => "W_ASSET_REMOTE_CACHED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagCode,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
}

impl Diagnostic {
    pub fn error(code: DiagCode, message: impl Into<String>) -> Self {
        Self { code, severity: Severity::Error, message: message.into(), line: None, column: None, asset: None }
    }

    pub fn warning(code: DiagCode, message: impl Into<String>) -> Self {
        Self { code, severity: Severity::Warning, message: message.into(), line: None, column: None, asset: None }
    }

    pub fn at(mut self, line: u32, column: u32) -> Self {
        self.line = Some(line);
        self.column = Some(column);
        self
    }

    pub fn for_asset(mut self, asset: impl Into<String>) -> Self {
        self.asset = Some(asset.into());
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub diagnostics: Vec<Diagnostic>,
}

impl Report {
    pub fn push(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }

    pub fn extend(&mut self, other: Report) {
        self.diagnostics.extend(other.diagnostics);
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn has_code(&self, code: DiagCode) -> bool {
        self.diagnostics.iter().any(|d| d.code == code)
    }

    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Error)
    }
}

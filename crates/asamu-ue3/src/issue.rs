//! Non-fatal findings from structural cross-checks.

use std::fmt;

use serde::Serialize;

/// How serious a cross-check finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Severity {
    /// Unusual but the package is still usable.
    Warning,
    /// Some part of the package is unusable (e.g. an export payload out of range).
    Error,
}

/// One cross-check finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// Severity.
    pub severity: Severity,
    /// Description.
    pub message: String,
}

impl Issue {
    /// A warning.
    pub fn warning(message: impl Into<String>) -> Self {
        Issue {
            severity: Severity::Warning,
            message: message.into(),
        }
    }

    /// An error-level finding.
    pub fn error(message: impl Into<String>) -> Self {
        Issue {
            severity: Severity::Error,
            message: message.into(),
        }
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, "{s}: {}", self.message)
    }
}

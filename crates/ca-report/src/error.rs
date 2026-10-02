//! Errors a report render raises.

/// What stopped a report.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReportError {
    /// The writer refused the bytes.
    #[error("report output failed: {0}")]
    Io(#[from] std::io::Error),
    /// The caller raised the cancellation flag before the render finished.
    #[error("report cancelled")]
    Cancelled,
    /// An option combination the format cannot carry.
    #[error("{0}")]
    Unsupported(String),
}

/// Result of a report render.
pub type Result<T> = std::result::Result<T, ReportError>;

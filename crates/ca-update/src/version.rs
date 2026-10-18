//! Build versions: `YYYY.MM.DD.NNNN`.

/// A build version: year, month, day and build number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version([u32; 4]);

impl Version {
    /// Parse `YYYY.MM.DD.NNNN`, with or without a leading `v`.
    ///
    /// The four parts compare as numbers, so `2026.10.1.1` is newer than
    /// `2026.9.30.9`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let mut parts = [0u32; 4];
        let mut pieces = text.split('.');
        for part in &mut parts {
            let piece = pieces.next()?;
            if piece.is_empty() || !piece.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            *part = piece.parse().ok()?;
        }
        if pieces.next().is_some() {
            return None;
        }
        Some(Self(parts))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [year, month, day, build] = self.0;
        write!(f, "{year:04}.{month:02}.{day:02}.{build:04}")
    }
}

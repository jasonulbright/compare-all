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

    /// The Windows Installer `ProductVersion` of this build, `YY.MM.NNNN`:
    /// the installer compares three fields, so the day is left out. `None`
    /// for a version that form cannot carry.
    #[must_use]
    pub fn installer_version(&self) -> Option<InstallerVersion> {
        let [year, month, _, build] = self.0;
        let year = year.checked_sub(2000).filter(|year| *year <= 255)?;
        if !(1..=12).contains(&month) || build > 65_535 {
            return None;
        }
        Some(InstallerVersion([year, month, build]))
    }
}

/// A Windows Installer `ProductVersion` of three fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstallerVersion([u32; 3]);

impl InstallerVersion {
    /// Parse `A.B.C` as the installer reads a product version: the first two
    /// fields at most 255, the third at most 65535. A fourth field, which the
    /// installer ignores, is refused.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = [0u32; 3];
        let mut pieces = text.split('.');
        for part in &mut parts {
            let piece = pieces.next()?;
            if piece.is_empty() || piece.len() > 5 || !piece.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *part = piece.parse().ok()?;
        }
        if pieces.next().is_some() || parts[0] > 255 || parts[1] > 255 || parts[2] > 65_535 {
            return None;
        }
        Some(Self(parts))
    }
}

impl std::fmt::Display for InstallerVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [major, minor, build] = self.0;
        write!(f, "{major}.{minor}.{build}")
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [year, month, day, build] = self.0;
        write!(f, "{year:04}.{month:02}.{day:02}.{build:04}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installer_version_is_year_month_build() {
        let version = |text| Version::parse(text).and_then(|v| v.installer_version());
        assert_eq!(
            version("2026.09.25.0031"),
            InstallerVersion::parse("26.9.31")
        );
        assert_eq!(
            version("2026.12.31.12345"),
            InstallerVersion::parse("26.12.12345")
        );
        assert_eq!(version("1999.12.31.0001"), None);
        assert_eq!(version("2256.01.01.0001"), None);
        assert_eq!(version("2026.01.01.65536"), None);
        assert!(version("2026.10.18.0007") > version("2026.10.01.0001"));
        assert!(version("2027.01.01.0001") > version("2026.12.31.0009"));
    }

    #[test]
    fn an_installer_version_has_three_fields_in_range() {
        assert!(InstallerVersion::parse("26.10.7").is_some());
        for bad in [
            "26.10",
            "26.10.7.1",
            "256.1.1",
            "1.256.1",
            "1.1.65536",
            "a.1.1",
            "1..1",
            "",
        ] {
            assert_eq!(InstallerVersion::parse(bad), None, "{bad}");
        }
    }
}

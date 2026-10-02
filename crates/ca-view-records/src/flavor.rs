//! What sets the three record comparisons apart.
//!
//! The registry, version and media views share one display, one row model and
//! one job pipeline. The flavor names the engine that reads a side, the session
//! kind the view answers for and the report it writes.

use ca_session::SessionKind;
use ca_ui::report::{RecordKind, ReportKind};

/// Which record comparison a view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flavor {
    /// Registry export files or live registry keys.
    Registry,
    /// Version resources of two Windows binaries.
    Version,
    /// Tags and stream facts of two media files.
    Media,
}

impl Flavor {
    /// The session kind this flavor answers for.
    #[must_use]
    pub const fn kind(self) -> SessionKind {
        match self {
            Self::Registry => SessionKind::RegistryCompare,
            Self::Version => SessionKind::VersionCompare,
            Self::Media => SessionKind::MediaCompare,
        }
    }

    /// The report dialog kind of this flavor.
    #[must_use]
    pub const fn report_kind(self) -> ReportKind {
        match self {
            Self::Registry => ReportKind::Registry,
            Self::Version => ReportKind::Version,
            Self::Media => ReportKind::Media,
        }
    }

    /// The record report this flavor writes.
    #[must_use]
    pub const fn record_kind(self) -> RecordKind {
        match self {
            Self::Registry => RecordKind::Registry,
            Self::Version => RecordKind::Version,
            Self::Media => RecordKind::Media,
        }
    }

    /// Stable name the widget identifiers of this flavor start with.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Registry => "registry-compare",
            Self::Version => "version-compare",
            Self::Media => "media-compare",
        }
    }

    /// Heading of the group column: a key, a block or a stream.
    #[must_use]
    pub const fn group_heading(self) -> &'static str {
        match self {
            Self::Registry => "Key",
            Self::Version => "Block",
            Self::Media => "Stream",
        }
    }

    /// True when the flavor carries an importance setting per field.
    ///
    /// The registry session has a specs page only, so every registry
    /// difference is important.
    #[must_use]
    pub const fn has_importance(self) -> bool {
        !matches!(self, Self::Registry)
    }
}

#[cfg(test)]
mod tests {
    use super::Flavor;

    #[test]
    fn each_flavor_names_its_own_kind_and_report() {
        let all = [Flavor::Registry, Flavor::Version, Flavor::Media];
        for (index, flavor) in all.iter().enumerate() {
            for other in &all[index + 1..] {
                assert_ne!(flavor.kind(), other.kind());
                assert_ne!(flavor.report_kind(), other.report_kind());
                assert_ne!(flavor.id(), other.id());
            }
        }
        assert!(!Flavor::Registry.has_importance());
        assert!(Flavor::Media.has_importance());
    }
}

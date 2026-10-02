//! The kinds of comparison a session can perform.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// Kind of comparison a session performs.
///
/// Marked non-exhaustive: further comparison types are added later and callers
/// must keep a fallback arm. A document written by a newer build can name a
/// kind this build has no variant for; that identifier is carried in
/// [`SessionKind::Unknown`] so it survives a load and a save unchanged.
///
/// The serialized form is the stable identifier from [`SessionKind::id`], which
/// is also the tag of the matching [`crate::settings::SessionSettings`]
/// variant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum SessionKind {
    /// Two folder trees side by side.
    FolderCompare,
    /// Two or three folder trees combined into an output folder.
    FolderMerge,
    /// Two folder trees reconciled by copy and delete operations.
    FolderSync,
    /// Two text files side by side.
    TextCompare,
    /// Two text files merged against a common ancestor into an output pane.
    TextMerge,
    /// A single text file in an editor with no comparison.
    TextEdit,
    /// A patch or diff file rendered as a comparison.
    TextPatch,
    /// Two tabular files compared cell by cell.
    TableCompare,
    /// Two files compared byte by byte in hexadecimal.
    HexCompare,
    /// Two images compared pixel by pixel.
    PictureCompare,
    /// Two registry trees or registry files.
    RegistryCompare,
    /// Two media files compared by their tags and streams.
    MediaCompare,
    /// Version information resources of two executables.
    VersionCompare,
    /// A kind named by a document this build does not understand, held by its
    /// identifier so the document can be written back unchanged.
    Unknown(String),
}

impl SessionKind {
    /// Every kind this build ships, in the order the launcher lists them.
    pub const ALL: &'static [SessionKind] = &[
        SessionKind::FolderCompare,
        SessionKind::FolderMerge,
        SessionKind::FolderSync,
        SessionKind::TextCompare,
        SessionKind::TextMerge,
        SessionKind::TextEdit,
        SessionKind::TextPatch,
        SessionKind::TableCompare,
        SessionKind::HexCompare,
        SessionKind::PictureCompare,
        SessionKind::RegistryCompare,
        SessionKind::MediaCompare,
        SessionKind::VersionCompare,
    ];

    /// Stable identifier used in stored files and on the command line.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            SessionKind::FolderCompare => "folder-compare",
            SessionKind::FolderMerge => "folder-merge",
            SessionKind::FolderSync => "folder-sync",
            SessionKind::TextCompare => "text-compare",
            SessionKind::TextMerge => "text-merge",
            SessionKind::TextEdit => "text-edit",
            SessionKind::TextPatch => "text-patch",
            SessionKind::TableCompare => "table-compare",
            SessionKind::HexCompare => "hex-compare",
            SessionKind::PictureCompare => "picture-compare",
            SessionKind::RegistryCompare => "registry-compare",
            SessionKind::MediaCompare => "media-compare",
            SessionKind::VersionCompare => "version-compare",
            SessionKind::Unknown(id) => id,
        }
    }

    /// Human readable name shown on launcher buttons and in menus.
    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            SessionKind::FolderCompare => "Folder Compare",
            SessionKind::FolderMerge => "Folder Merge",
            SessionKind::FolderSync => "Folder Sync",
            SessionKind::TextCompare => "Text Compare",
            SessionKind::TextMerge => "Text Merge",
            SessionKind::TextEdit => "Text Edit",
            SessionKind::TextPatch => "Text Patch",
            SessionKind::TableCompare => "Table Compare",
            SessionKind::HexCompare => "Hex Compare",
            SessionKind::PictureCompare => "Picture Compare",
            SessionKind::RegistryCompare => "Registry Compare",
            SessionKind::MediaCompare => "Media Compare",
            SessionKind::VersionCompare => "Version Compare",
            SessionKind::Unknown(id) => id,
        }
    }

    /// True when this build has no variant for the kind.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self, SessionKind::Unknown(_))
    }

    /// Reads a kind from its identifier, keeping an unrecognized identifier in
    /// [`SessionKind::Unknown`] rather than rejecting it. Stored documents use
    /// this; user and command line input uses [`FromStr`], which rejects.
    #[must_use]
    pub fn from_id_or_unknown(id: &str) -> Self {
        SessionKind::from_str(id).unwrap_or_else(|_| SessionKind::Unknown(id.to_owned()))
    }

    /// True when the kind browses folder trees rather than individual files.
    #[must_use]
    pub fn is_folder_kind(&self) -> bool {
        matches!(
            self,
            SessionKind::FolderCompare | SessionKind::FolderMerge | SessionKind::FolderSync
        )
    }

    /// Number of input sides the kind reads. A merge reads a third ancestor
    /// side; an editor reads one. An unknown kind is assumed to read two.
    #[must_use]
    pub fn side_count(&self) -> u8 {
        match self {
            SessionKind::TextEdit | SessionKind::TextPatch => 1,
            SessionKind::FolderMerge | SessionKind::TextMerge => 3,
            _ => 2,
        }
    }
}

impl fmt::Display for SessionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.title())
    }
}

impl Serialize for SessionKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

impl<'de> Deserialize<'de> for SessionKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Ok(SessionKind::from_id_or_unknown(&id))
    }
}

/// Failure to read a [`SessionKind`] from its stable identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown session kind: {0}")]
pub struct KindParseError(pub String);

impl FromStr for SessionKind {
    type Err = KindParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        SessionKind::ALL
            .iter()
            .find(|k| k.id() == s)
            .cloned()
            .ok_or_else(|| KindParseError(s.to_owned()))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_round_trip() {
        for kind in SessionKind::ALL {
            assert_eq!(&SessionKind::from_str(kind.id()).unwrap(), kind);
        }
    }

    #[test]
    fn identifiers_are_unique() {
        let mut ids: Vec<&str> = SessionKind::ALL.iter().map(SessionKind::id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn all_lists_thirteen_kinds() {
        assert_eq!(SessionKind::ALL.len(), 13);
    }

    #[test]
    fn unknown_identifier_is_rejected_by_from_str() {
        assert!(SessionKind::from_str("chart-compare").is_err());
    }

    #[test]
    fn merge_kinds_read_three_sides() {
        assert_eq!(SessionKind::TextMerge.side_count(), 3);
        assert_eq!(SessionKind::TextEdit.side_count(), 1);
        assert_eq!(SessionKind::HexCompare.side_count(), 2);
    }

    #[test]
    fn a_kind_serializes_as_its_identifier() {
        let json = serde_json::to_string(&SessionKind::FolderCompare).unwrap();
        assert_eq!(json, r#""folder-compare""#);
        let back: SessionKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SessionKind::FolderCompare);
    }

    #[test]
    fn an_unknown_kind_survives_a_round_trip() {
        let kind: SessionKind = serde_json::from_str(r#""chart-compare""#).unwrap();
        assert_eq!(kind, SessionKind::Unknown("chart-compare".to_owned()));
        assert!(kind.is_unknown());
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            r#""chart-compare""#,
            "an unknown kind is written back unchanged"
        );
    }

    #[test]
    fn an_unknown_kind_reports_neutral_shape() {
        let kind = SessionKind::Unknown("chart-compare".to_owned());
        assert!(!kind.is_folder_kind());
        assert_eq!(kind.side_count(), 2);
        assert_eq!(kind.title(), "chart-compare");
    }
}

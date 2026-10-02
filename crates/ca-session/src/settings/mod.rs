//! Typed settings, grouped per session kind to mirror the session settings
//! dialog of that kind.
//!
//! Every group exists twice: a fully populated struct with a value for every
//! field, and an override struct whose fields are all optional. Stored layers
//! hold overrides; a view reads the populated form produced by
//! [`crate::layer`].

pub mod binary;
pub mod common;
pub mod defaults;
pub mod folder;
pub(crate) mod macros;
pub mod table;
pub mod text;

use crate::error::{Error, Result};
use crate::kind::SessionKind;
use serde::Serialize;
use serde_json::Value;

pub use binary::{
    HexCompareOverride, HexCompareSettings, MediaCompareOverride, MediaCompareSettings,
    PictureCompareOverride, PictureCompareSettings, RegistryCompareOverride,
    RegistryCompareSettings, VersionCompareOverride, VersionCompareSettings,
};
pub use common::{
    AlignmentAlgorithm, EncodingChoice, FileFormatChoice, FormatOverride, FormatSettings,
    ReplacementItem, ReplacementOverride, ReplacementSettings, RuleSide, SpecsOverride,
    SpecsSettings,
};
pub use folder::{
    FolderCompareOverride, FolderCompareSettings, FolderMergeOverride, FolderMergeSettings,
    FolderSyncOverride, FolderSyncSettings,
};
pub use table::{TableCompareOverride, TableCompareSettings};
pub use text::{
    TextCompareOverride, TextCompareSettings, TextEditOverride, TextEditSettings,
    TextMergeOverride, TextMergeSettings, TextPatchOverride, TextPatchSettings,
};

/// Reads the kind tag out of a settings object this build does not understand.
fn kind_of_unknown(value: &Value) -> SessionKind {
    value.get("kind").and_then(Value::as_str).map_or_else(
        || SessionKind::Unknown(String::new()),
        SessionKind::from_id_or_unknown,
    )
}

/// Deserializes a known session payload without its tag, leaving the tag for
/// the enum dispatcher instead of capturing it in the payload's flattened
/// unknown fields.
fn known_settings<T: serde::de::DeserializeOwned>(value: &Value) -> Option<T> {
    let mut value = value.clone();
    value.as_object_mut()?.remove("kind");
    serde_json::from_value(value).ok()
}

/// Declares the two kind-tagged enums and their dispatch from one variant list.
macro_rules! session_settings_enums {
    ( $( $variant:ident : $full:ty => $over:ty ),* $(,)? ) => {
        /// Fully populated settings of one session, tagged by kind.
        ///
        /// The tag is the kind's stable identifier, the same string
        /// [`SessionKind::id`] returns.
        #[derive(Debug, Clone, PartialEq, Serialize)]
        #[serde(tag = "kind", rename_all = "kebab-case")]
        #[non_exhaustive]
        pub enum SessionSettings {
            $(
                #[doc = concat!("Settings of a ", stringify!($variant), " session.")]
                $variant($full),
            )*
            /// Settings of a kind this build does not understand, carried
            /// through unchanged.
            #[serde(untagged)]
            Unknown(Value),
        }

        /// Partial settings of one session, tagged by kind. Unset fields fall
        /// through to the layer below.
        #[derive(Debug, Clone, PartialEq, Serialize)]
        #[serde(tag = "kind", rename_all = "kebab-case")]
        #[non_exhaustive]
        pub enum SessionSettingsOverride {
            $(
                #[doc = concat!("Overrides for a ", stringify!($variant), " session.")]
                $variant($over),
            )*
            /// Overrides of a kind this build does not understand, carried
            /// through unchanged.
            #[serde(untagged)]
            Unknown(Value),
        }

        impl<'de> serde::Deserialize<'de> for SessionSettings {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = <Value as serde::Deserialize>::deserialize(deserializer)?;
                let Some(id) = value.get("kind").and_then(Value::as_str) else {
                    return Ok(Self::Unknown(value));
                };
                match SessionKind::from_id_or_unknown(id) {
                    $(
                        SessionKind::$variant => match known_settings::<$full>(&value) {
                            Some(settings) => Ok(Self::$variant(settings)),
                            None => Ok(Self::Unknown(value)),
                        },
                    )*
                    SessionKind::Unknown(_) => Ok(Self::Unknown(value)),
                }
            }
        }

        impl<'de> serde::Deserialize<'de> for SessionSettingsOverride {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = <Value as serde::Deserialize>::deserialize(deserializer)?;
                let Some(id) = value.get("kind").and_then(Value::as_str) else {
                    return Ok(Self::Unknown(value));
                };
                match SessionKind::from_id_or_unknown(id) {
                    $(
                        SessionKind::$variant => match known_settings::<$over>(&value) {
                            Some(settings) => Ok(Self::$variant(settings)),
                            None => Ok(Self::Unknown(value)),
                        },
                    )*
                    SessionKind::Unknown(_) => Ok(Self::Unknown(value)),
                }
            }
        }

        impl SessionSettings {
            /// Kind these settings belong to.
            #[must_use]
            pub fn kind(&self) -> SessionKind {
                match self {
                    $( SessionSettings::$variant(_) => SessionKind::$variant, )*
                    SessionSettings::Unknown(value) => kind_of_unknown(value),
                }
            }

            /// The sides, the editing switch and the description, or `None`
            /// for a kind this build does not understand.
            #[must_use]
            pub fn specs(&self) -> Option<&common::SpecsSettings> {
                match self {
                    $( SessionSettings::$variant(value) => Some(&value.specs), )*
                    SessionSettings::Unknown(_) => None,
                }
            }

            /// The specs group for editing, or `None` for a kind this build
            /// does not understand.
            pub fn specs_mut(&mut self) -> Option<&mut common::SpecsSettings> {
                match self {
                    $( SessionSettings::$variant(value) => Some(&mut value.specs), )*
                    SessionSettings::Unknown(_) => None,
                }
            }

            /// Built-in defaults for a kind. An unknown kind has no built-ins,
            /// so it yields settings carrying nothing but the kind tag.
            #[must_use]
            pub fn defaults_for(kind: &SessionKind) -> Self {
                match kind {
                    $( SessionKind::$variant => SessionSettings::$variant(<$full>::default()), )*
                    SessionKind::Unknown(id) => {
                        SessionSettings::Unknown(serde_json::json!({ "kind": id }))
                    }
                }
            }
        }

        impl SessionSettingsOverride {
            /// Kind these overrides belong to.
            #[must_use]
            pub fn kind(&self) -> SessionKind {
                match self {
                    $( SessionSettingsOverride::$variant(_) => SessionKind::$variant, )*
                    SessionSettingsOverride::Unknown(value) => kind_of_unknown(value),
                }
            }

            /// An override for a kind that sets nothing.
            #[must_use]
            pub fn empty_for(kind: &SessionKind) -> Self {
                match kind {
                    $(
                        SessionKind::$variant => {
                            SessionSettingsOverride::$variant(<$over>::default())
                        }
                    )*
                    SessionKind::Unknown(id) => {
                        SessionSettingsOverride::Unknown(serde_json::json!({ "kind": id }))
                    }
                }
            }

            /// Builds an override that sets every field of `full`.
            #[must_use]
            pub fn from_full(full: &SessionSettings) -> Self {
                match full {
                    $(
                        SessionSettings::$variant(value) => {
                            SessionSettingsOverride::$variant(<$over>::from_full(value))
                        }
                    )*
                    SessionSettings::Unknown(value) => {
                        SessionSettingsOverride::Unknown(value.clone())
                    }
                }
            }

            /// Clears the sides and the description, which name one comparison
            /// and never belong to the defaults a new session starts from.
            pub fn clear_specs(&mut self) {
                match self {
                    $(
                        SessionSettingsOverride::$variant(over) => {
                            over.specs = Default::default();
                        }
                    )*
                    SessionSettingsOverride::Unknown(_) => {}
                }
            }

            /// Writes every field this override sets onto `base`.
            ///
            /// # Errors
            /// Returns [`Error::KindMismatch`] when the override and the target
            /// belong to different kinds, or [`Error::UnreadableSettings`]
            /// when a recognized kind carries fields this build cannot read.
            pub fn apply_to(&self, base: &mut SessionSettings) -> Result<()> {
                match (self, base) {
                    $(
                        (
                            SessionSettingsOverride::$variant(over),
                            SessionSettings::$variant(target),
                        ) => {
                            over.apply_to(target);
                            Ok(())
                        }
                    )*
                    (
                        SessionSettingsOverride::Unknown(over),
                        target @ SessionSettings::Unknown(_),
                    ) if kind_of_unknown(over) == target.kind() => {
                        *target = SessionSettings::Unknown(over.clone());
                        Ok(())
                    }
                    (SessionSettingsOverride::Unknown(over), target)
                        if kind_of_unknown(over) == target.kind() =>
                    {
                        Err(Error::UnreadableSettings {
                            kind: target.kind(),
                        })
                    }
                    #[allow(unreachable_patterns)]
                    (over, target) => Err(Error::KindMismatch {
                        expected: target.kind(),
                        found: over.kind(),
                    }),
                }
            }

            /// Folds `other` into this override; fields set by `other` win.
            ///
            /// # Errors
            /// Returns [`Error::KindMismatch`] when the two belong to different
            /// kinds.
            pub fn merge(&mut self, other: &Self) -> Result<()> {
                match (self, other) {
                    $(
                        (
                            SessionSettingsOverride::$variant(target),
                            SessionSettingsOverride::$variant(source),
                        ) => {
                            target.merge(source);
                            Ok(())
                        }
                    )*
                    (
                        target @ SessionSettingsOverride::Unknown(_),
                        SessionSettingsOverride::Unknown(source),
                    ) if target.kind() == kind_of_unknown(source) => {
                        *target = SessionSettingsOverride::Unknown(source.clone());
                        Ok(())
                    }
                    #[allow(unreachable_patterns)]
                    (target, source) => Err(Error::KindMismatch {
                        expected: target.kind(),
                        found: source.kind(),
                    }),
                }
            }

            /// True when the override sets nothing. Unknown settings carrying
            /// only their kind tag set nothing either.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                match self {
                    $( SessionSettingsOverride::$variant(over) => over.is_empty(), )*
                    SessionSettingsOverride::Unknown(value) => value
                        .as_object()
                        .is_some_and(|object| {
                            object.keys().all(|key| key == "kind")
                        }),
                }
            }
        }
    };
}

session_settings_enums! {
    FolderCompare: FolderCompareSettings => FolderCompareOverride,
    FolderMerge: FolderMergeSettings => FolderMergeOverride,
    FolderSync: FolderSyncSettings => FolderSyncOverride,
    TextCompare: TextCompareSettings => TextCompareOverride,
    TextMerge: TextMergeSettings => TextMergeOverride,
    TextEdit: TextEditSettings => TextEditOverride,
    TextPatch: TextPatchSettings => TextPatchOverride,
    TableCompare: TableCompareSettings => TableCompareOverride,
    HexCompare: HexCompareSettings => HexCompareOverride,
    PictureCompare: PictureCompareSettings => PictureCompareOverride,
    RegistryCompare: RegistryCompareSettings => RegistryCompareOverride,
    MediaCompare: MediaCompareSettings => MediaCompareOverride,
    VersionCompare: VersionCompareSettings => VersionCompareOverride,
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
    fn every_kind_has_settings_of_its_own_kind() {
        for kind in SessionKind::ALL {
            assert_eq!(&SessionSettings::defaults_for(kind).kind(), kind);
            assert_eq!(&SessionSettingsOverride::empty_for(kind).kind(), kind);
        }
    }

    #[test]
    fn every_settings_struct_round_trips_through_json() {
        for kind in SessionKind::ALL {
            let settings = SessionSettings::defaults_for(kind);
            let json = serde_json::to_string(&settings).unwrap();
            let back: SessionSettings = serde_json::from_str(&json).unwrap();
            assert_eq!(back, settings, "{kind} settings did not round trip");
        }
    }

    #[test]
    fn every_override_round_trips_through_json() {
        for kind in SessionKind::ALL {
            let full = SessionSettings::defaults_for(kind);
            let over = SessionSettingsOverride::from_full(&full);
            let json = serde_json::to_string(&over).unwrap();
            let back: SessionSettingsOverride = serde_json::from_str(&json).unwrap();
            assert_eq!(back, over, "{kind} override did not round trip");
        }
    }

    #[test]
    fn table_settings_keep_large_unknown_numbers_inside_the_known_kind() {
        const JSON: &str = r#"{"kind":"table-compare","future":{"count":1234567890123456789012345678901234567890}}"#;
        const NUMBER: &str = "1234567890123456789012345678901234567890";
        let expected: Value = serde_json::from_str(NUMBER).unwrap();

        let settings: SessionSettings = serde_json::from_str(JSON).unwrap();
        let SessionSettings::TableCompare(settings) = &settings else {
            panic!("known table settings fell through to the unknown-kind arm");
        };
        assert_eq!(
            settings
                .unknown
                .get("future")
                .and_then(|value| value.get("count")),
            Some(&expected)
        );
        assert!(serde_json::to_string(&settings).unwrap().contains(NUMBER));

        let over: SessionSettingsOverride = serde_json::from_str(JSON).unwrap();
        let SessionSettingsOverride::TableCompare(over) = over else {
            panic!("known table override fell through to the unknown-kind arm");
        };
        assert_eq!(
            over.unknown
                .get("future")
                .and_then(|value| value.get("count")),
            Some(&expected)
        );
    }

    #[test]
    fn an_empty_override_serializes_without_field_keys() {
        let over = SessionSettingsOverride::empty_for(&SessionKind::HexCompare);
        assert!(over.is_empty());
        let json = serde_json::to_string(&over).unwrap();
        assert!(!json.contains("bytesPerRow"), "{json}");
    }

    #[test]
    fn a_full_override_restores_every_field() {
        let mut edited = FolderCompareSettings::default();
        edited.comparison.compare_timestamps = false;
        edited.comparison.timestamp_tolerance_seconds = 7;
        edited.name_filters.exclude_files = vec!["*.tmp".to_owned()];
        let over = SessionSettingsOverride::from_full(&SessionSettings::FolderCompare(edited));

        let mut base = SessionSettings::defaults_for(&SessionKind::FolderCompare);
        over.apply_to(&mut base).unwrap();
        let SessionSettings::FolderCompare(result) = base else {
            panic!("kind changed");
        };
        assert!(!result.comparison.compare_timestamps);
        assert_eq!(result.comparison.timestamp_tolerance_seconds, 7);
        assert_eq!(result.name_filters.exclude_files, vec!["*.tmp".to_owned()]);
    }

    #[test]
    fn applying_across_kinds_is_rejected() {
        let over = SessionSettingsOverride::empty_for(&SessionKind::TextCompare);
        let mut base = SessionSettings::defaults_for(&SessionKind::HexCompare);
        assert!(over.apply_to(&mut base).is_err());
    }

    #[test]
    fn unknown_fields_survive_at_every_nesting_level() {
        let source = r#"{
            "kind": "text-compare",
            "alignment": { "skewTolerance": 9, "futureAlignField": true },
            "futureGroup": { "a": 1 }
        }"#;
        let over: SessionSettingsOverride = serde_json::from_str(source).unwrap();
        let SessionSettingsOverride::TextCompare(text) = &over else {
            panic!("kind changed");
        };
        assert_eq!(text.alignment.skew_tolerance, Some(9));
        assert_eq!(
            text.alignment.unknown["futureAlignField"],
            Value::Bool(true),
            "a field inside a known group is kept"
        );
        assert_eq!(
            text.unknown["futureGroup"],
            serde_json::json!({ "a": 1 }),
            "a whole group beside the known ones is kept"
        );

        let written: Value = serde_json::from_str(&serde_json::to_string(&over).unwrap()).unwrap();
        assert_eq!(written, serde_json::from_str::<Value>(source).unwrap());
    }

    #[test]
    fn unknown_fields_reach_the_resolved_settings() {
        let over: SessionSettingsOverride = serde_json::from_str(
            r#"{"kind":"text-compare","alignment":{"futureAlignField":true},"futureGroup":1}"#,
        )
        .unwrap();
        let mut base = SessionSettings::defaults_for(&SessionKind::TextCompare);
        over.apply_to(&mut base).unwrap();
        let SessionSettings::TextCompare(text) = &base else {
            panic!("kind changed");
        };
        assert_eq!(
            text.alignment.unknown["futureAlignField"],
            Value::Bool(true)
        );
        assert_eq!(text.unknown["futureGroup"], Value::from(1));
    }

    #[test]
    fn an_unknown_field_makes_an_override_non_empty() {
        let over: SessionSettingsOverride =
            serde_json::from_str(r#"{"kind":"text-compare","futureGroup":1}"#).unwrap();
        assert!(!over.is_empty());
        assert!(SessionSettingsOverride::empty_for(&SessionKind::TextCompare).is_empty());
    }

    #[test]
    fn settings_of_an_unknown_kind_survive_a_round_trip() {
        let source = r#"{"kind":"chart-compare","axes":{"x":"time"}}"#;
        let over: SessionSettingsOverride = serde_json::from_str(source).unwrap();
        assert_eq!(
            over.kind(),
            SessionKind::Unknown("chart-compare".to_owned())
        );
        let written: Value = serde_json::from_str(&serde_json::to_string(&over).unwrap()).unwrap();
        assert_eq!(written, serde_json::from_str::<Value>(source).unwrap());
    }

    #[test]
    fn unknown_settings_of_different_kinds_do_not_mix() {
        let mut first: SessionSettingsOverride =
            serde_json::from_str(r#"{"kind":"chart-compare"}"#).unwrap();
        let second: SessionSettingsOverride =
            serde_json::from_str(r#"{"kind":"graph-compare"}"#).unwrap();
        assert!(first.merge(&second).is_err());
        let same: SessionSettingsOverride =
            serde_json::from_str(r#"{"kind":"chart-compare","axes":1}"#).unwrap();
        first.merge(&same).unwrap();
        assert!(!first.is_empty());
    }

    #[test]
    fn clearing_specs_leaves_the_other_groups_alone() {
        let mut edited = TextCompareSettings::default();
        edited.specs.description = "one comparison".to_owned();
        edited.specs.left = Some(crate::location::SideLocation::local("/tmp/left"));
        edited.alignment.skew_tolerance = 42;
        let mut over = SessionSettingsOverride::from_full(&SessionSettings::TextCompare(edited));
        over.clear_specs();

        let SessionSettingsOverride::TextCompare(text) = &over else {
            panic!("kind changed");
        };
        assert!(text.specs.is_empty());
        assert_eq!(text.alignment.skew_tolerance, Some(42));
    }

    #[test]
    fn later_overrides_win_when_merged() {
        let mut first = text::TextAlignmentOverride::default();
        first.skew_tolerance = Some(10);
        first.use_closeness_matching = Some(false);
        let mut second = text::TextAlignmentOverride::default();
        second.skew_tolerance = Some(20);
        first.merge(&second);
        assert_eq!(first.skew_tolerance, Some(20));
        assert_eq!(first.use_closeness_matching, Some(false));
    }

    #[test]
    fn documented_defaults_hold() {
        let text = TextCompareSettings::default();
        assert!(!text.importance.compare_line_endings);
        assert_eq!(text.format.left_format, FileFormatChoice::detected());
        assert_eq!(text.format.right_encoding, EncodingChoice::from_format());
        let folder = FolderCompareSettings::default();
        assert_eq!(
            folder.comparison.binary_size_threshold_bytes,
            4 * 1024 * 1024
        );
    }
}

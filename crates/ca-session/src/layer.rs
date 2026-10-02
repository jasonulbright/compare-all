//! Three layer resolution of settings.
//!
//! Layer one is the built-in defaults compiled into the program. Layer two is
//! the per-kind defaults the user edits, which decide what a newly created
//! session of that kind starts from. Layer three is the overrides a single
//! session carries. Resolution walks the three in that order and yields a
//! fully populated struct; views read only the result.

use crate::error::{Error, Result};
use crate::kind::SessionKind;
use crate::settings::{SessionSettings, SessionSettingsOverride};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Settings of one session after all three layers are applied.
pub type ResolvedSettings = SessionSettings;

/// The editable layers: per-kind defaults for new sessions.
///
/// The built-in layer is not stored, so a file written by a build with
/// different built-ins still resolves against the current ones.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsLayers {
    /// Per-kind defaults, absent for kinds the user has never edited. A key
    /// naming a kind this build has no variant for is kept with its value.
    #[serde(default)]
    session_defaults: BTreeMap<SessionKind, SessionSettingsOverride>,
    /// Layers written by another build, preserved verbatim.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl SettingsLayers {
    /// Layers with no user edits, so every kind resolves to its built-ins.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in defaults of a kind, ignoring both editable layers.
    #[must_use]
    pub fn built_in(kind: &SessionKind) -> SessionSettings {
        SessionSettings::defaults_for(kind)
    }

    /// The stored defaults for new sessions of a kind, if the user edited them.
    #[must_use]
    pub fn session_defaults(&self, kind: &SessionKind) -> Option<&SessionSettingsOverride> {
        self.session_defaults.get(kind)
    }

    /// Kinds whose defaults the user has edited.
    pub fn edited_kinds(&self) -> impl Iterator<Item = &SessionKind> + '_ {
        self.session_defaults.keys()
    }

    /// Settings a newly created session of a kind starts from: the built-ins
    /// with the stored per-kind defaults applied.
    #[must_use]
    pub fn resolve_defaults(&self, kind: &SessionKind) -> ResolvedSettings {
        let mut resolved = Self::built_in(kind);
        if let Some(defaults) = self.session_defaults.get(kind) {
            // Kinds match by construction: the map is keyed by the override's
            // own kind, so a mismatch cannot be produced through the setters.
            let _ = defaults.apply_to(&mut resolved);
        }
        resolved
    }

    /// Settings of one session: the per-kind defaults with the session's own
    /// overrides applied.
    ///
    /// # Errors
    /// Returns [`Error::KindMismatch`] when `overrides` belongs to another kind.
    pub fn resolve(
        &self,
        kind: &SessionKind,
        overrides: &SessionSettingsOverride,
    ) -> Result<ResolvedSettings> {
        if &overrides.kind() != kind {
            return Err(Error::KindMismatch {
                expected: kind.clone(),
                found: overrides.kind(),
            });
        }
        let mut resolved = self.resolve_defaults(kind);
        overrides.apply_to(&mut resolved)?;
        Ok(resolved)
    }

    /// Replaces the stored defaults of a kind.
    ///
    /// # Errors
    /// Returns [`Error::KindMismatch`] when `defaults` belongs to another kind.
    pub fn set_session_defaults(
        &mut self,
        kind: &SessionKind,
        defaults: SessionSettingsOverride,
    ) -> Result<()> {
        if &defaults.kind() != kind {
            return Err(Error::KindMismatch {
                expected: kind.clone(),
                found: defaults.kind(),
            });
        }
        self.session_defaults.insert(kind.clone(), defaults);
        Ok(())
    }

    /// Promotes one session's resolved settings to the defaults every new
    /// session of that kind starts from.
    ///
    /// The sides and the description name one particular comparison, so they
    /// are dropped rather than promoted: a new session of the kind would
    /// otherwise open already pointing at the previous session's paths.
    ///
    /// # Errors
    /// Returns [`Error::KindMismatch`] when `settings` belongs to another kind.
    pub fn update_session_defaults_from(
        &mut self,
        kind: &SessionKind,
        settings: &ResolvedSettings,
    ) -> Result<()> {
        if &settings.kind() != kind {
            return Err(Error::KindMismatch {
                expected: kind.clone(),
                found: settings.kind(),
            });
        }
        let mut defaults = SessionSettingsOverride::from_full(settings);
        defaults.clear_specs();
        self.session_defaults.insert(kind.clone(), defaults);
        Ok(())
    }

    /// Drops the stored defaults of one kind, so new sessions of that kind
    /// start from the built-ins again.
    pub fn reset_to_defaults(&mut self, kind: &SessionKind) {
        self.session_defaults.remove(kind);
    }

    /// Drops the stored defaults of every kind.
    pub fn reset_all_to_defaults(&mut self) {
        self.session_defaults.clear();
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
    use crate::settings::folder::{FolderCompareOverride, FolderComparisonOverride};
    use crate::settings::text::{TextAlignmentOverride, TextCompareOverride};
    use crate::settings::SessionSettings;

    fn folder_defaults_override(tolerance: u32) -> SessionSettingsOverride {
        SessionSettingsOverride::FolderCompare(FolderCompareOverride {
            comparison: FolderComparisonOverride {
                timestamp_tolerance_seconds: Some(tolerance),
                compare_filename_case: Some(true),
                ..FolderComparisonOverride::default()
            },
            ..FolderCompareOverride::default()
        })
    }

    #[test]
    fn with_no_edits_resolution_yields_built_ins() {
        let layers = SettingsLayers::new();
        for kind in SessionKind::ALL {
            let empty = SessionSettingsOverride::empty_for(kind);
            assert_eq!(
                layers.resolve(kind, &empty).unwrap(),
                SettingsLayers::built_in(kind)
            );
        }
    }

    #[test]
    fn session_defaults_sit_above_built_ins() {
        let mut layers = SettingsLayers::new();
        layers
            .set_session_defaults(&SessionKind::FolderCompare, folder_defaults_override(30))
            .unwrap();

        let SessionSettings::FolderCompare(resolved) =
            layers.resolve_defaults(&SessionKind::FolderCompare)
        else {
            panic!("kind changed");
        };
        assert_eq!(resolved.comparison.timestamp_tolerance_seconds, 30);
        assert!(resolved.comparison.compare_filename_case);
        assert!(resolved.comparison.compare_size, "untouched field survives");
    }

    #[test]
    fn session_overrides_sit_above_session_defaults() {
        let mut layers = SettingsLayers::new();
        layers
            .set_session_defaults(&SessionKind::FolderCompare, folder_defaults_override(30))
            .unwrap();

        let per_session = SessionSettingsOverride::FolderCompare(FolderCompareOverride {
            comparison: FolderComparisonOverride {
                timestamp_tolerance_seconds: Some(2),
                ..FolderComparisonOverride::default()
            },
            ..FolderCompareOverride::default()
        });

        let SessionSettings::FolderCompare(resolved) = layers
            .resolve(&SessionKind::FolderCompare, &per_session)
            .unwrap()
        else {
            panic!("kind changed");
        };
        assert_eq!(resolved.comparison.timestamp_tolerance_seconds, 2);
        assert!(
            resolved.comparison.compare_filename_case,
            "the middle layer still supplies fields the session leaves unset"
        );
    }

    #[test]
    fn reset_to_defaults_drops_only_the_named_kind() {
        let mut layers = SettingsLayers::new();
        layers
            .set_session_defaults(&SessionKind::FolderCompare, folder_defaults_override(30))
            .unwrap();
        layers
            .set_session_defaults(
                &SessionKind::TextCompare,
                SessionSettingsOverride::TextCompare(TextCompareOverride {
                    alignment: TextAlignmentOverride {
                        skew_tolerance: Some(9),
                        ..TextAlignmentOverride::default()
                    },
                    ..TextCompareOverride::default()
                }),
            )
            .unwrap();

        layers.reset_to_defaults(&SessionKind::FolderCompare);
        assert_eq!(
            layers.resolve_defaults(&SessionKind::FolderCompare),
            SettingsLayers::built_in(&SessionKind::FolderCompare)
        );
        assert!(layers.session_defaults(&SessionKind::TextCompare).is_some());

        layers.reset_all_to_defaults();
        assert_eq!(layers.edited_kinds().count(), 0);
    }

    #[test]
    fn update_session_defaults_from_a_session_reproduces_its_settings() {
        let mut layers = SettingsLayers::new();
        let mut edited = crate::settings::TextCompareSettings::default();
        edited.importance.compare_line_endings = true;
        edited.alignment.skew_tolerance = 42;
        let edited = SessionSettings::TextCompare(edited);

        layers
            .update_session_defaults_from(&SessionKind::TextCompare, &edited)
            .unwrap();

        assert_eq!(layers.resolve_defaults(&SessionKind::TextCompare), edited);
    }

    #[test]
    fn promoted_defaults_carry_no_sides_and_no_description() {
        let mut layers = SettingsLayers::new();
        let mut edited = crate::settings::TextCompareSettings::default();
        edited.specs.left = Some(crate::location::SideLocation::local("/tmp/left"));
        edited.specs.right = Some(crate::location::SideLocation::local("/tmp/right"));
        edited.specs.description = "last comparison".to_owned();
        edited.alignment.skew_tolerance = 42;

        layers
            .update_session_defaults_from(
                &SessionKind::TextCompare,
                &SessionSettings::TextCompare(edited),
            )
            .unwrap();

        let SessionSettings::TextCompare(resolved) =
            layers.resolve_defaults(&SessionKind::TextCompare)
        else {
            panic!("kind changed");
        };
        assert_eq!(
            resolved.specs.left, None,
            "a new session starts with no side"
        );
        assert_eq!(resolved.specs.right, None);
        assert!(resolved.specs.description.is_empty());
        assert_eq!(
            resolved.alignment.skew_tolerance, 42,
            "every other group is still promoted"
        );
    }

    #[test]
    fn resolving_with_another_kind_is_rejected() {
        let layers = SettingsLayers::new();
        let over = SessionSettingsOverride::empty_for(&SessionKind::TextCompare);
        assert!(layers.resolve(&SessionKind::HexCompare, &over).is_err());
    }

    #[test]
    fn layers_round_trip_through_json() {
        let mut layers = SettingsLayers::new();
        layers
            .set_session_defaults(&SessionKind::FolderCompare, folder_defaults_override(5))
            .unwrap();
        let json = serde_json::to_string(&layers).unwrap();
        let back: SettingsLayers = serde_json::from_str(&json).unwrap();
        assert_eq!(back, layers);
    }

    #[test]
    fn defaults_of_an_unknown_kind_survive_a_round_trip() {
        let source = r#"{"sessionDefaults":{"chart-compare":{"kind":"chart-compare","axes":1}}}"#;
        let layers: SettingsLayers = serde_json::from_str(source).unwrap();
        let kind = SessionKind::Unknown("chart-compare".to_owned());
        assert!(layers.session_defaults(&kind).is_some());
        let written: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&layers).unwrap()).unwrap();
        assert_eq!(
            written,
            serde_json::from_str::<serde_json::Value>(source).unwrap()
        );
    }
}

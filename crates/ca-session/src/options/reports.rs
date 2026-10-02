//! The report settings one view kind was last run with.
//!
//! The report engine owns the typed layouts and options; this document holds
//! only their stable names, so the settings layer never depends on the engine
//! and a name this build does not know is written back unchanged.

use crate::location::StoredPath;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Where a report is written.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReportTarget {
    /// A file the user names.
    #[default]
    File,
    /// The clipboard, as text.
    Clipboard,
    /// A printer.
    Printer,
    /// A value this build does not understand.
    #[serde(untagged)]
    Unknown(Value),
}

/// What one view kind was last asked for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[allow(clippy::struct_excessive_bools)]
pub struct ReportPreference {
    /// Stable name of the layout. Empty means the built-in layout.
    pub layout: String,
    /// Where the report goes.
    pub target: ReportTarget,
    /// Stable name of the document format. Empty means the built-in format.
    pub format: String,
    /// Stable name of the display filter. Empty means the built-in filter.
    pub display: String,
    /// Stable name of the patch dialect. Empty means the built-in dialect.
    pub patch_format: String,
    /// Treat a difference that does not matter as a match.
    pub ignore_unimportant: bool,
    /// Write line numbers or byte addresses.
    pub line_numbers: bool,
    /// Lines kept around a difference. Zero means the built-in count.
    pub context_lines: u32,
    /// Start the file in another program once it is written.
    pub open_after_saving: bool,
    /// The file the last report was written to.
    pub last_file: Option<StoredPath>,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// The report settings of every view kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReportOptions {
    /// View name to what that view was last asked for.
    pub views: BTreeMap<String, ReportPreference>,
    /// Fields written by another build.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl ReportOptions {
    /// What `view` was last asked for, where it was asked for anything.
    #[must_use]
    pub fn view(&self, view: &str) -> Option<&ReportPreference> {
        self.views.get(view)
    }

    /// Stores what `view` was asked for.
    pub fn set_view(&mut self, view: &str, preference: ReportPreference) {
        self.views.insert(view.to_owned(), preference);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::{ReportOptions, ReportPreference, ReportTarget};

    #[test]
    fn a_view_with_no_entry_states_nothing() {
        assert_eq!(ReportOptions::default().view("text"), None);
    }

    #[test]
    fn a_stored_preference_comes_back() {
        let mut options = ReportOptions::default();
        options.set_view(
            "hex",
            ReportPreference {
                layout: "interleaved".to_owned(),
                target: ReportTarget::Clipboard,
                line_numbers: true,
                ..ReportPreference::default()
            },
        );
        let held = options.view("hex").unwrap();
        assert_eq!(held.layout, "interleaved");
        assert_eq!(held.target, ReportTarget::Clipboard);
        assert!(held.line_numbers);
    }

    #[test]
    fn an_unknown_target_survives_a_round_trip() {
        let target: ReportTarget = serde_json::from_str("\"fax\"").unwrap();
        assert!(matches!(target, ReportTarget::Unknown(_)));
        assert_eq!(serde_json::to_string(&target).unwrap(), "\"fax\"");
    }
}

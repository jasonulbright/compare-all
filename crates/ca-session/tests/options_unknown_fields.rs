//! An unknown field of the options document survives a load and a save.
//!
//! The document is populated so that every page, every stored choice and every
//! list item is present, then a probe key is injected into every JSON object at
//! every depth. A type that drops unknown fields fails here without anybody
//! adding a fixture line for it.

#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use ca_session::options::{
    BackupLocation, ColorGroup, ComparisonPriority, LineEndingsOnSave, NewSessionPlacement,
    OpenWithEntry, ProgramOptions, QuickCompareMethod, ReportPreference, ReportTarget, Rgb,
    SynchronizeConfirmations, ThemeChoice, ThumbnailMode, WorkingFolder, OPTIONS_FILE,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Objects whose keys are data rather than field names, and whose values are
/// not strings. A probe key inserted into one of these would have to satisfy
/// the map's value type, so those objects are left alone. A map of strings is
/// still probed, because a probe value satisfies it and is written back.
fn is_typed_map(path: &str) -> bool {
    path == "/commands/views" || path == "/reports/views" || path.ends_with("/shortcuts")
}

fn is_wide_value_map(path: &str) -> bool {
    is_typed_map(path) || path == "/archives/masks" || path.ends_with("/slots")
}

/// Options with every page populated and every stored choice present.
fn populated() -> ProgramOptions {
    let mut options = ProgramOptions::default();

    "Morning".clone_into(&mut options.startup.load_workspace);
    "Evening".clone_into(&mut options.startup.save_workspace_on_exit);
    options.startup.show_quick_compare = true;
    options.startup.quick_compare_method = QuickCompareMethod::from_id("rules");
    options.tabs.new_session_placement = NewSessionPlacement::from_id("newWindow");
    options.appearance.theme = ThemeChoice::from_id("dark");
    options.appearance.enable_preview = true;
    options.appearance.fonts.editor_point_size = 14.0;
    for group in ColorGroup::ALL {
        let pair = options.appearance.palettes.group_mut(*group);
        pair.table_mut(true).set("same_line", Rgb::new(1, 2, 3));
        pair.table_mut(false).set("same_text", Rgb::new(4, 5, 6));
    }
    options.text_editing.tab_stop = 4;
    options.text_editing.line_endings_on_save = LineEndingsOnSave::from_id("lf");
    options.backups.location = BackupLocation::from_id("namedFolder");
    options.backups.folder = Some(std::path::PathBuf::from("/var/backups").into());
    options.file_operations.synchronize_confirmations =
        SynchronizeConfirmations::from_id("yesToAll");
    options
        .archives
        .masks
        .insert("zip".to_owned(), "*.zip".to_owned());
    options
        .archives
        .masks
        .insert("rar".to_owned(), String::new());
    options.tweaks.comparison_priority = ComparisonPriority::from_id("high");
    options.tweaks.thumbnail_mode = ThumbnailMode::from_id("allowScrolling");
    options.tweaks.name_filter_presets = vec!["*.rs".to_owned()];
    options.tweaks.shared_sessions_file = Some(std::path::PathBuf::from("/srv/shared.pkg").into());

    options.open_with.entries = vec![
        OpenWithEntry {
            description: "Editor".to_owned(),
            program: std::path::PathBuf::from("editor").into(),
            arguments: vec!["%f".to_owned(), "+%l".to_owned()],
            shortcut: "Ctrl+E".to_owned(),
            working_folder: WorkingFolder::default().with_id("parentFolder"),
            ..OpenWithEntry::default()
        },
        OpenWithEntry {
            description: "Viewer".to_owned(),
            working_folder: WorkingFolder::Named {
                path: std::path::PathBuf::from("/start").into(),
                unknown: BTreeMap::new(),
            },
            ..OpenWithEntry::default()
        },
    ];

    options
        .commands
        .set_shortcuts("text", "FindNext", vec!["F8".to_owned()]);
    options.commands.set_toolbar(
        "folder",
        vec!["Reload".to_owned(), "Synchronize".to_owned()],
    );
    options
        .commands
        .set_hidden_from_menu("folder", "About", true);
    options
        .commands
        .set_hidden_from_toolbar("folder", "Peek", true);

    options.reports.set_view(
        "text",
        ReportPreference {
            layout: "side-by-side".to_owned(),
            target: ReportTarget::File,
            format: "html".to_owned(),
            display: "mismatches".to_owned(),
            patch_format: "unified".to_owned(),
            ignore_unimportant: true,
            line_numbers: true,
            context_lines: 5,
            open_after_saving: true,
            last_file: Some(std::path::PathBuf::from("/tmp/report.html").into()),
            ..ReportPreference::default()
        },
    );
    options.reports.set_view(
        "folder",
        ReportPreference {
            target: ReportTarget::Clipboard,
            ..ReportPreference::default()
        },
    );
    options
}

/// One step of a path into a JSON document.
#[derive(Debug, Clone)]
enum Step {
    Key(String),
    Index(usize),
}

fn render(path: &[Step]) -> String {
    let mut text = String::new();
    for step in path {
        text.push('/');
        match step {
            Step::Key(key) => text.push_str(key),
            Step::Index(index) => text.push_str(&index.to_string()),
        }
    }
    text
}

/// Inserts one probe key into every object, and returns where each one landed.
fn inject(value: &mut Value, path: &mut Vec<Step>, probes: &mut Vec<(Vec<Step>, String)>) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                path.push(Step::Index(index));
                inject(item, path, probes);
                path.pop();
            }
        }
        Value::Object(object) => {
            let here = render(path);
            if !is_typed_map(&here) {
                let key = format!("probe{}", probes.len());
                let marker = format!("kept at {here}");
                object.insert(key, Value::String(marker.clone()));
                probes.push((path.clone(), marker));
            }
            let keys: Vec<String> = object.keys().cloned().collect();
            for key in keys {
                if key.starts_with("probe") {
                    continue;
                }
                path.push(Step::Key(key.clone()));
                if let Some(child) = object.get_mut(&key) {
                    inject(child, path, probes);
                }
                path.pop();
            }
        }
        _ => {}
    }
}

/// Inserts an arbitrary precision value into each object with an unknown-field
/// map, recording the field's location for a byte-exact value check.
fn inject_wide_integer(
    value: &mut Value,
    path: &mut Vec<Step>,
    probes: &mut Vec<(Vec<Step>, String)>,
) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                path.push(Step::Index(index));
                inject_wide_integer(item, path, probes);
                path.pop();
            }
        }
        Value::Object(object) => {
            let here = render(path);
            if !is_wide_value_map(&here) {
                let key = format!("wideIntegerProbe{}", probes.len());
                let Ok(number) = serde_json::from_str::<Value>("18446744073709551616") else {
                    panic!("arbitrary precision JSON number must parse");
                };
                object.insert(key.clone(), number);
                probes.push((path.clone(), key));
            }
            let keys: Vec<String> = object.keys().cloned().collect();
            for key in keys {
                if key.starts_with("wideIntegerProbe") {
                    continue;
                }
                path.push(Step::Key(key.clone()));
                if let Some(child) = object.get_mut(&key) {
                    inject_wide_integer(child, path, probes);
                }
                path.pop();
            }
        }
        _ => {}
    }
}

/// Reads whatever sits at `path`.
fn at<'a>(value: &'a Value, path: &[Step]) -> Option<&'a Value> {
    let mut current = value;
    for step in path {
        current = match step {
            Step::Key(key) => current.get(key)?,
            Step::Index(index) => current.get(index)?,
        };
    }
    Some(current)
}

fn holds(object: &Map<String, Value>, marker: &str) -> bool {
    object.values().any(|value| value.as_str() == Some(marker))
}

#[test]
fn every_probe_survives_a_load_and_a_save_of_the_options_document() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(OPTIONS_FILE);
    populated().save_replacing(&file).unwrap();

    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let mut probes = Vec::new();
    inject(&mut document, &mut Vec::new(), &mut probes);
    assert!(
        probes.len() > 30,
        "only {} objects were probed",
        probes.len()
    );
    std::fs::write(&file, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let load = ProgramOptions::load(&file).unwrap();
    assert!(
        load.recovered_backup.is_none(),
        "the probed document did not parse"
    );
    let mut loaded = *load.options;
    loaded.save(&file).unwrap();
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();

    for (path, marker) in &probes {
        let place = render(path);
        let Some(Value::Object(object)) = at(&after, path) else {
            panic!("{place} is gone from the saved document");
        };
        assert!(holds(object, marker), "the probe at {place} was dropped");
    }
}

#[test]
fn wide_unknown_integers_survive_a_load_and_save_at_every_object_depth() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(OPTIONS_FILE);
    populated().save_replacing(&file).unwrap();
    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let mut probes = Vec::new();
    inject_wide_integer(&mut document, &mut Vec::new(), &mut probes);
    assert!(probes.len() > 30, "only {} objects probed", probes.len());
    std::fs::write(&file, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let load = ProgramOptions::load(&file).unwrap();
    assert!(
        load.recovered_backup.is_none(),
        "valid newer values were quarantined"
    );
    let mut loaded = *load.options;
    loaded.save(&file).unwrap();
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();

    let expected = serde_json::from_str::<Value>("18446744073709551616").unwrap();
    for (path, key) in &probes {
        assert_eq!(
            at(&after, path).and_then(|object| object.get(key)),
            Some(&expected),
            "wide value lost at {}#{key}",
            render(path)
        );
    }
}

#[test]
fn the_populated_options_round_trip_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(OPTIONS_FILE);
    let mut written = populated();
    written.save_replacing(&file).unwrap();
    let read = *ProgramOptions::load(&file).unwrap().options;
    assert_eq!(read, written);
}

#[test]
fn a_saved_document_re_saves_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(OPTIONS_FILE);
    populated().save_replacing(&file).unwrap();
    let first = std::fs::read_to_string(&file).unwrap();
    let mut loaded = *ProgramOptions::load(&file).unwrap().options;
    loaded.save(&file).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), first);
}

#[test]
fn a_choice_this_build_does_not_know_is_kept_rather_than_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(OPTIONS_FILE);
    populated().save_replacing(&file).unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    let text = text.replace("\"kind\": \"dark\"", "\"kind\": \"sepia\"");
    std::fs::write(&file, &text).unwrap();

    let mut loaded = *ProgramOptions::load(&file).unwrap().options;
    assert_eq!(loaded.appearance.theme.id(), "unknown");
    loaded.save(&file).unwrap();
    assert!(std::fs::read_to_string(&file).unwrap().contains("sepia"));
}

#[test]
fn a_root_level_unknown_field_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(OPTIONS_FILE);
    ProgramOptions::default().save_replacing(&file).unwrap();
    let mut document: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    document
        .as_object_mut()
        .unwrap()
        .insert("probeRoot".to_owned(), Value::String("x".to_owned()));
    std::fs::write(&file, serde_json::to_string_pretty(&document).unwrap()).unwrap();
    let mut loaded = *ProgramOptions::load(&file).unwrap().options;
    assert_eq!(
        loaded.unknown.get("probeRoot").and_then(Value::as_str),
        Some("x"),
        "root unknown = {:?}",
        loaded.unknown
    );
    loaded.save(&file).unwrap();
    assert!(std::fs::read_to_string(&file)
        .unwrap()
        .contains("probeRoot"));
}

//! A settings document written by a later build round trips through this one.
//!
//! The probe walks a serialised options document and injects an unknown key at
//! every depth, including inside every nested object. Each injection must
//! deserialise, keep the unknown key, and write it back.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_records::media::MediaCompareOptions;
use ca_records::media::MediaReadOptions;
use ca_records::registry::plan::{EditOp, EditPlan, Side};
use ca_records::registry::value::ValueName;
use ca_records::registry::{LiveOptions, RegistryCompareOptions};
use ca_records::version::{VersionCompareOptions, VersionReadOptions};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

/// Name of the injected key. It is not a field of any type here.
const PROBE: &str = "fieldFromALaterBuild";

/// Inject the probe into every object of `value`, one document per object.
fn injections(value: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut paths: Vec<Vec<PathStep>> = Vec::new();
    collect(value, &mut Vec::new(), &mut paths);
    for path in paths {
        let mut copy = value.clone();
        if inject(&mut copy, &path) {
            out.push(copy);
        }
    }
    out
}

#[derive(Debug, Clone)]
enum PathStep {
    Key(String),
    Index(usize),
}

fn collect(value: &Value, path: &mut Vec<PathStep>, out: &mut Vec<Vec<PathStep>>) {
    match value {
        Value::Object(map) => {
            out.push(path.clone());
            for (key, child) in map {
                path.push(PathStep::Key(key.clone()));
                collect(child, path, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(PathStep::Index(index));
                collect(child, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

fn inject(value: &mut Value, path: &[PathStep]) -> bool {
    let mut node = value;
    for step in path {
        node = match (step, node) {
            (PathStep::Key(key), Value::Object(map)) => match map.get_mut(key) {
                Some(child) => child,
                None => return false,
            },
            (PathStep::Index(index), Value::Array(items)) => match items.get_mut(*index) {
                Some(child) => child,
                None => return false,
            },
            _ => return false,
        };
    }
    match node {
        Value::Object(map) => {
            map.insert(PROBE.to_owned(), json!({ "kept": true }));
            true
        }
        _ => false,
    }
}

fn probe<T>(value: &T, name: &str)
where
    T: Serialize + DeserializeOwned,
{
    let document = serde_json::to_value(value).expect("serialise");
    let cases = injections(&document);
    assert!(!cases.is_empty(), "{name} has no object to probe");
    for case in cases {
        let parsed: T = serde_json::from_value(case.clone())
            .unwrap_or_else(|error| panic!("{name} refused an unknown key: {error}\n{case}"));
        let written = serde_json::to_value(&parsed).expect("serialise");
        assert!(
            written.to_string().contains(PROBE),
            "{name} lost the unknown key\nin:  {case}\nout: {written}"
        );
    }
}

#[test]
fn every_options_document_keeps_an_unknown_key_at_every_depth() {
    probe(&RegistryCompareOptions::default(), "RegistryCompareOptions");
    probe(&LiveOptions::default(), "LiveOptions");
    probe(&VersionReadOptions::default(), "VersionReadOptions");
    probe(&VersionCompareOptions::default(), "VersionCompareOptions");
    probe(&MediaReadOptions::default(), "MediaReadOptions");
    probe(&MediaCompareOptions::default(), "MediaCompareOptions");
}

#[test]
fn an_edit_plan_keeps_an_unknown_key_at_every_depth() {
    let plan = EditPlan::new()
        .with(EditOp::copy_key(Side::Left, "HKEY_CURRENT_USER\\A"))
        .with(EditOp::copy_value(
            Side::Right,
            "HKEY_CURRENT_USER\\A",
            ValueName::from_raw("N"),
        ));
    probe(&plan, "EditPlan");
}

#[test]
fn an_unknown_enum_value_parses_into_the_unknown_arm() {
    let document = json!({ "view": "bit-128" });
    let options: LiveOptions = serde_json::from_value(document).expect("parse");
    assert!(matches!(
        options.view,
        ca_records::registry::RegistryView::Unknown(_)
    ));
    let written = serde_json::to_value(&options).expect("serialise");
    assert!(written.to_string().contains("bit-128"));
}

#[test]
fn an_unknown_plan_operation_is_refused_on_apply() {
    let document = json!({ "ops": [ { "shred-everything": { "keyPath": "A" } } ] });
    let plan: EditPlan = serde_json::from_value(document).expect("parse");
    assert!(matches!(plan.ops.first(), Some(EditOp::Unknown(_))));
    let mut left = ca_records::registry::RegFile::empty(ca_records::registry::RegFileVersion::V5);
    let mut right = left.clone();
    assert!(plan.apply_to_files(&mut left, &mut right).is_err());
}

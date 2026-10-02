//! Every settings page answers the two layer actions the dialog offers.
//!
//! The walk picks one representative field per page of every session kind, so
//! a page added later is covered without a line being added here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_session::settings::{SessionSettings, SessionSettingsOverride};
use ca_session::{SessionKind, SettingsLayers};
use ca_ui::settings::{tabs_for, Field, FieldShape, FieldValue, SettingsDialog};

/// A value the field accepts that differs from the one it holds.
///
/// A shape with no second value to offer yields nothing, so the page is left
/// out rather than asserted against a value it cannot take.
fn other_value(field: &Field, value: &FieldValue) -> Option<FieldValue> {
    match value {
        FieldValue::Flag(on) => Some(FieldValue::Flag(!on)),
        // One below the stored value stays inside the field's own width and
        // its declared limit, which one above does not.
        FieldValue::Count(number) => Some(FieldValue::Count(match number {
            0 => 1,
            other => other - 1,
        })),
        FieldValue::Signed(number) => Some(FieldValue::Signed(number + 1)),
        FieldValue::Decimal(number) => Some(FieldValue::Decimal(number + 1.0)),
        FieldValue::Text(text) => Some(FieldValue::Text(format!("{text}a"))),
        FieldValue::Lines(_) => Some(FieldValue::Lines(vec!["zz".to_owned()])),
        FieldValue::Choice(id) => field
            .choices
            .iter()
            .find(|choice| choice.id != id)
            .map(|choice| FieldValue::Choice(choice.id.to_owned())),
        FieldValue::Replacements(_) | FieldValue::Columns(_) => None,
    }
}

/// One field of `fields` that states a value the page can change, with that
/// value.
fn representative(
    fields: &'static [Field],
    settings: &SessionSettings,
) -> Option<(&'static Field, FieldValue)> {
    fields.iter().find_map(|field| {
        if matches!(field.shape, FieldShape::Replacements | FieldShape::Columns)
            || !field.is_bound()
        {
            return None;
        }
        let value = field.read(settings)?;
        let other = other_value(field, &value)?;
        let mut probe = settings.clone();
        field.write(&mut probe, &other);
        // A field that reads back what it held clamped or refused the value,
        // so it states nothing this walk can assert against.
        (field.read(&probe)? == other).then_some((field, other))
    })
}

fn dialog_for(kind: &SessionKind, layers: &SettingsLayers) -> SettingsDialog {
    SettingsDialog::new(
        kind.clone(),
        &SessionSettingsOverride::empty_for(kind),
        layers,
        1,
    )
}

/// Every page of every kind offers a field the walk can exercise, so no page
/// is silently skipped.
#[test]
fn every_page_states_a_field_the_layer_actions_reach() {
    for kind in SessionKind::ALL {
        let defaults = SessionSettings::defaults_for(kind);
        for tab in tabs_for(kind) {
            if tab.fields.iter().all(|field| {
                matches!(field.shape, FieldShape::Replacements | FieldShape::Columns)
                    || !field.is_bound()
            }) {
                continue;
            }
            assert!(
                representative(tab.fields, &defaults).is_some(),
                "{kind} page {} states no field a layer action can reach",
                tab.name
            );
        }
    }
}

/// A field no engine reads is drawn but takes no edit, and says why.
///
/// A field is either bound to an engine or carries a reason it is not, so no
/// control looks as though it changed something.
#[test]
fn a_field_no_engine_reads_takes_no_edit_and_states_a_reason() {
    let layers = SettingsLayers::new();
    for kind in SessionKind::ALL {
        let built_in = SessionSettings::defaults_for(kind);
        for field in tabs_for(kind).iter().flat_map(|tab| tab.fields.iter()) {
            let Some(reason) = field.unavailable else {
                continue;
            };
            assert!(
                !reason.trim().is_empty(),
                "{kind} {} is not bound and gives no reason",
                field.key
            );
            let Some(value) = field.read(&built_in) else {
                continue;
            };
            let Some(other) = other_value(field, &value) else {
                continue;
            };
            let mut dialog = dialog_for(kind, &layers);
            dialog.set_value(field.key, &other);
            assert_eq!(
                dialog.value(field.key),
                Some(value),
                "{kind} {} took an edit no engine reads",
                field.key
            );
            assert!(
                dialog.to_override().is_empty(),
                "{kind} {} wrote an override no engine reads",
                field.key
            );
        }
    }
}

/// A field put back to the layer below inherits again and leaves the override
/// empty.
#[test]
fn resetting_a_field_of_any_page_inherits_again() {
    let layers = SettingsLayers::new();
    for kind in SessionKind::ALL {
        let defaults = SessionSettings::defaults_for(kind);
        for tab in tabs_for(kind) {
            let Some((field, value)) = representative(tab.fields, &defaults) else {
                continue;
            };
            let mut dialog = dialog_for(kind, &layers);
            dialog.set_value(field.key, &value);
            assert!(
                dialog.is_overridden(field.key),
                "{kind} {} was not marked as stated by the session",
                field.key
            );
            dialog.reset_field(field.key);
            assert!(
                !dialog.is_overridden(field.key),
                "{kind} {} still states a value of its own",
                field.key
            );
            assert!(
                dialog.to_override().is_empty(),
                "{kind} {} left something in the override",
                field.key
            );
        }
    }
}

/// Resetting the whole dialog returns every page at once.
#[test]
fn resetting_everything_returns_every_page() {
    let layers = SettingsLayers::new();
    for kind in SessionKind::ALL {
        let defaults = SessionSettings::defaults_for(kind);
        let mut dialog = dialog_for(kind, &layers);
        let mut touched = 0_usize;
        for tab in tabs_for(kind) {
            if let Some((field, value)) = representative(tab.fields, &defaults) {
                dialog.set_value(field.key, &value);
                touched += 1;
            }
        }
        if touched == 0 {
            continue;
        }
        assert!(!dialog.to_override().is_empty(), "{kind} stated nothing");
        dialog.reset_all();
        assert!(
            dialog.to_override().is_empty(),
            "{kind} still states a value after a full reset"
        );
    }
}

/// The page naming the sides and the description of one comparison.
const SPECS_PAGE: &str = "Specs";

/// A field written to the session defaults reaches the next session of the
/// kind, which then inherits it rather than restating it.
///
/// The specs page is left out: it names one comparison, so the defaults a new
/// session starts from never carry it.
#[test]
fn updating_the_session_defaults_reaches_the_next_session() {
    for kind in SessionKind::ALL {
        let built_in = SessionSettings::defaults_for(kind);
        for tab in tabs_for(kind).iter().filter(|tab| tab.name != SPECS_PAGE) {
            let Some((field, value)) = representative(tab.fields, &built_in) else {
                continue;
            };
            let mut layers = SettingsLayers::new();
            let mut edited = layers.resolve_defaults(kind);
            field.write(&mut edited, &value);
            layers
                .update_session_defaults_from(kind, &edited)
                .unwrap_or_else(|error| panic!("{kind} {} was refused: {error}", field.key));

            let dialog = dialog_for(kind, &layers);
            assert_eq!(
                dialog.value(field.key),
                Some(value),
                "{kind} {} did not reach the next session",
                field.key
            );
            assert!(
                !dialog.is_overridden(field.key),
                "{kind} {} is restated rather than inherited",
                field.key
            );
            assert!(
                dialog.to_override().is_empty(),
                "{kind} {} wrote an override for an inherited value",
                field.key
            );
        }
    }
}

/// The sides and the description name one comparison, so writing the session
/// defaults drops them rather than starting every later session on them.
#[test]
fn the_session_defaults_never_carry_the_sides_of_one_comparison() {
    for kind in SessionKind::ALL {
        let built_in = SessionSettings::defaults_for(kind);
        let Some(specs) = tabs_for(kind).iter().find(|tab| tab.name == SPECS_PAGE) else {
            continue;
        };
        let Some((field, value)) = representative(specs.fields, &built_in) else {
            continue;
        };
        let mut layers = SettingsLayers::new();
        let mut edited = layers.resolve_defaults(kind);
        field.write(&mut edited, &value);
        layers
            .update_session_defaults_from(kind, &edited)
            .unwrap_or_else(|error| panic!("{kind} {} was refused: {error}", field.key));

        assert_ne!(
            dialog_for(kind, &layers).value(field.key),
            Some(value),
            "{kind} {} reached the defaults a new session starts from",
            field.key
        );
    }
}

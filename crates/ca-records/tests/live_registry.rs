//! The live registry reader.
//!
//! The Windows test creates one throwaway key under
//! `HKEY_CURRENT_USER\Software\compare-all-tests`, reads it back and deletes
//! it. It never touches `HKEY_LOCAL_MACHINE` and never reads or writes a key
//! it did not create. When the key cannot be created the test reports the skip
//! and passes, so a locked down machine does not fail the suite.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ca_records::registry::live;
use ca_records::registry::{LiveOptions, RegistrySpec, RegistryView};
use ca_records::RecordError;

#[test]
fn a_remote_address_is_refused_with_a_reason() {
    let spec = RegistrySpec::parse(r"reg:\\OtherMachine\HKEY_USERS\X").expect("spec");
    let error = live::read(&spec, &LiveOptions::default()).expect_err("refused");
    match error {
        RecordError::Unsupported(reason) => assert_eq!(reason, live::remote_reason()),
        other => panic!("expected an unsupported error, got {other}"),
    }
    assert!(!live::remote_is_available());
}

#[test]
fn an_unknown_view_is_refused() {
    let spec = RegistrySpec::parse(r"HKEY_CURRENT_USER\Software").expect("spec");
    let options = LiveOptions {
        view: RegistryView::Unknown(serde_json::json!("bit-128")),
        ..LiveOptions::default()
    };
    assert!(matches!(
        live::read(&spec, &options),
        Err(RecordError::Unsupported(_))
    ));
}

#[cfg(not(windows))]
#[test]
fn a_platform_without_a_registry_reports_the_limit() {
    assert!(!live::is_available());
    let spec = RegistrySpec::parse(r"HKEY_CURRENT_USER\Software").expect("spec");
    assert!(matches!(
        live::read(&spec, &LiveOptions::default()),
        Err(RecordError::Unsupported(_))
    ));
}

#[cfg(windows)]
mod windows_only {
    use super::{live, LiveOptions, RegistrySpec, RegistryView};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS};
    use winreg::RegKey;

    /// Parent of every key these tests create.
    const TEST_ROOT: &str = r"Software\compare-all-tests";

    /// A key name no other run uses.
    fn unique_name() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        name_at(nanos)
    }

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Two calls inside one clock tick read the same time, so the counter
    /// keeps their names apart.
    fn name_at(nanos: u128) -> String {
        let count = COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("{nanos}-{}-{count}", std::process::id())
    }

    struct ThrowawayKey {
        path: String,
    }

    impl ThrowawayKey {
        /// Create the key and fill it, or report why it could not be created.
        fn create() -> Option<Self> {
            let path = format!("{TEST_ROOT}\\{}", unique_name());
            let root = RegKey::predef(HKEY_CURRENT_USER);
            let (key, _) = root.create_subkey(&path).ok()?;
            key.set_value("Text", &"value").ok()?;
            key.set_value("Number", &42u32).ok()?;
            key.set_value("Multi", &vec!["a".to_owned(), "b".to_owned()])
                .ok()?;
            let (child, _) = key.create_subkey("Child").ok()?;
            child.set_value("Inner", &"deep").ok()?;
            Some(Self { path })
        }
    }

    impl Drop for ThrowawayKey {
        fn drop(&mut self) {
            let root = RegKey::predef(HKEY_CURRENT_USER);
            let _ = root.delete_subkey_all(&self.path);
            // The parent is removed only when this run emptied it.
            if let Ok(parent) = root.open_subkey_with_flags(TEST_ROOT, KEY_ALL_ACCESS) {
                if parent.enum_keys().next().is_none() {
                    let _ = root.delete_subkey(TEST_ROOT);
                }
            }
        }
    }

    #[test]
    fn a_throwaway_key_reads_back_with_its_values_and_child() {
        let Some(key) = ThrowawayKey::create() else {
            println!("skipped: the throwaway key could not be created");
            return;
        };
        let spec =
            RegistrySpec::parse(&format!(r"reg:\\HKEY_CURRENT_USER\{}", key.path)).expect("spec");
        let tree = live::read(&spec, &LiveOptions::default()).expect("read");
        assert_eq!(tree.record("Text").expect("text value").display, "value");
        assert_eq!(
            tree.record("Number").expect("number value").type_name,
            "REG_DWORD"
        );
        assert_eq!(tree.record("Multi").expect("multi value").display, "a | b");
        let child = tree.child("Child").expect("child key");
        assert_eq!(child.record("Inner").expect("inner value").display, "deep");
        assert_eq!(tree.record_count(), 4);
    }

    #[test]
    fn the_non_recursive_read_stops_at_the_named_key() {
        let Some(key) = ThrowawayKey::create() else {
            println!("skipped: the throwaway key could not be created");
            return;
        };
        let spec = RegistrySpec::parse(&format!(r"HKEY_CURRENT_USER\{}", key.path)).expect("spec");
        let options = LiveOptions {
            recursive: false,
            ..LiveOptions::default()
        };
        let tree = live::read(&spec, &options).expect("read");
        assert!(tree.children.is_empty());
        assert_eq!(tree.record_count(), 3);
    }

    #[test]
    fn each_view_choice_opens_the_same_user_key() {
        let Some(key) = ThrowawayKey::create() else {
            println!("skipped: the throwaway key could not be created");
            return;
        };
        let spec = RegistrySpec::parse(&format!(r"HKEY_CURRENT_USER\{}", key.path)).expect("spec");
        for view in [
            RegistryView::Native,
            RegistryView::Bit32,
            RegistryView::Bit64,
        ] {
            let options = LiveOptions {
                view,
                ..LiveOptions::default()
            };
            assert!(live::read(&spec, &options).is_ok());
        }
    }

    #[test]
    fn a_key_that_does_not_exist_is_reported_as_missing() {
        let spec = RegistrySpec::parse(&format!(
            r"HKEY_CURRENT_USER\{TEST_ROOT}\{}-absent",
            unique_name()
        ))
        .expect("spec");
        assert!(live::read(&spec, &LiveOptions::default()).is_err());
    }

    #[test]
    fn names_taken_in_a_row_all_differ() {
        let mut names: Vec<String> = (0..64).map(|_| name_at(7)).collect();
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn the_reader_is_available_on_this_platform() {
        assert!(live::is_available());
    }
}

/// The live writer. Every test works inside one throwaway key,
/// `HKEY_CURRENT_USER\Software\compare-all-tests\<pid>-<nonce>`, which a guard
/// deletes on every exit path, a failed assertion included.
#[cfg(windows)]
mod writer {
    use ca_records::limits::Limits;
    use ca_records::registry::live::{self, WriteConsent};
    use ca_records::registry::plan::{live_steps, DeleteForm, EditOp, EditPlan, Side};
    use ca_records::registry::{
        LiveOptions, LiveStep, RegFile, RegistrySpec, ValueData, ValueName,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS};
    use winreg::RegKey;

    const TEST_ROOT: &str = r"Software\compare-all-tests";

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct Throwaway {
        sub: String,
    }

    impl Throwaway {
        fn create() -> Option<Self> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default();
            let nonce = format!("{nanos}{}", COUNTER.fetch_add(1, Ordering::SeqCst));
            let sub = format!(r"{TEST_ROOT}\{}-{nonce}", std::process::id());
            RegKey::predef(HKEY_CURRENT_USER).create_subkey(&sub).ok()?;
            Some(Self { sub })
        }

        fn path(&self) -> String {
            format!(r"HKEY_CURRENT_USER\{}", self.sub)
        }

        fn spec(&self) -> RegistrySpec {
            RegistrySpec::parse(&self.path()).expect("spec")
        }

        fn export(&self) -> RegFile {
            live::export(&self.spec(), &LiveOptions::default()).expect("export")
        }
    }

    impl Drop for Throwaway {
        fn drop(&mut self) {
            let root = RegKey::predef(HKEY_CURRENT_USER);
            let _ = root.delete_subkey_all(&self.sub);
            // The shared parent goes only when this run left it empty; a
            // parent that still holds a key refuses the plain delete.
            if let Ok(parent) = root.open_subkey_with_flags(TEST_ROOT, KEY_ALL_ACCESS) {
                if parent.enum_keys().next().is_none() {
                    let _ = root.delete_subkey(TEST_ROOT);
                }
            }
        }
    }

    fn consent() -> WriteConsent {
        WriteConsent {
            confirmed: true,
            protected_hive_confirmed: false,
        }
    }

    /// `model` with `ops` applied, as a view builds it.
    fn edited(model: &RegFile, ops: Vec<EditOp>) -> RegFile {
        let mut left = model.clone();
        let mut right = RegFile::empty(model.version);
        EditPlan {
            ops,
            ..EditPlan::new()
        }
        .apply_to_files_with(&mut left, &mut right, DeleteForm::Remove)
        .expect("apply");
        left
    }

    fn name(text: &str) -> ValueName {
        ValueName::from_raw(text)
    }

    #[test]
    fn the_writer_creates_writes_and_deletes_inside_its_base_key() {
        let Some(key) = Throwaway::create() else {
            println!("skipped: the throwaway key could not be created");
            return;
        };
        let base = key.path();
        let child = format!(r"{base}\Child");
        let before = key.export();
        let after = edited(
            &before,
            vec![
                EditOp::create_key(Side::Left, &child),
                EditOp::set_value(Side::Left, &child, name("Text"), &ValueData::Sz("t".into())),
                EditOp::set_value(Side::Left, &base, ValueName::Default, &ValueData::Qword(9)),
                EditOp::set_value(
                    Side::Left,
                    &base,
                    name("List"),
                    &ValueData::MultiSz(vec!["a".into(), "b".into()]),
                ),
            ],
        );
        let steps = live_steps(&before, &after);
        live::apply_confirmed(&key.spec(), &steps, consent()).expect("write");
        let now = key.export();
        assert!(live_steps(&now, &after).is_empty(), "{now:?}");

        let smaller = edited(
            &now,
            vec![
                EditOp::delete_key(Side::Left, &child),
                EditOp::delete_value(Side::Left, &base, name("List")),
            ],
        );
        let steps = live_steps(&now, &smaller);
        live::apply_confirmed(&key.spec(), &steps, consent()).expect("delete");
        let last = key.export();
        assert_eq!(last.keys.len(), 1);
        assert_eq!(last.keys[0].entries.len(), 1);
    }

    #[test]
    fn nothing_is_written_without_consent_or_with_a_step_outside_the_base() {
        let Some(key) = Throwaway::create() else {
            println!("skipped: the throwaway key could not be created");
            return;
        };
        let before = key.export();
        let after = edited(
            &before,
            vec![EditOp::create_key(
                Side::Left,
                format!(r"{}\Child", key.path()),
            )],
        );
        let steps = live_steps(&before, &after);
        assert!(live::apply_confirmed(&key.spec(), &steps, WriteConsent::default()).is_err());
        assert_eq!(key.export(), before);
        let mut mixed = steps.clone();
        mixed.push(LiveStep::CreateKey {
            key_path: format!(r"{}-outside", key.path()),
        });
        assert!(live::apply_confirmed(&key.spec(), &mixed, consent()).is_err());
        assert_eq!(key.export(), before, "the step inside the base ran");
    }

    #[test]
    fn a_backup_read_before_the_writes_puts_every_touched_key_back() {
        let Some(key) = Throwaway::create() else {
            println!("skipped: the throwaway key could not be created");
            return;
        };
        let base = key.path();
        let kept = format!(r"{base}\Kept");
        let empty = key.export();
        let seeded = edited(
            &empty,
            vec![
                EditOp::set_value(Side::Left, &base, name("A"), &ValueData::Sz("a".into())),
                EditOp::set_value(Side::Left, &base, name("B"), &ValueData::Dword(1)),
                EditOp::create_key(Side::Left, &kept),
                EditOp::set_value(Side::Left, &kept, name("K"), &ValueData::Binary(vec![1, 2])),
            ],
        );
        live::apply_confirmed(&key.spec(), &live_steps(&empty, &seeded), consent()).expect("seed");
        let original = key.export();
        let changed = edited(
            &original,
            vec![
                EditOp::set_value(
                    Side::Left,
                    &base,
                    name("A"),
                    &ValueData::Sz("changed".into()),
                ),
                EditOp::delete_value(Side::Left, &base, name("B")),
                EditOp::set_value(Side::Left, &base, name("C"), &ValueData::Sz("new".into())),
                EditOp::delete_key(Side::Left, &kept),
                EditOp::create_key(Side::Left, format!(r"{base}\Fresh")),
            ],
        );
        let steps = live_steps(&original, &changed);
        let backup = live::backup(&key.spec(), &steps, &LiveOptions::default()).expect("backup");
        let bytes = backup.to_bytes().expect("bytes");
        let reread = RegFile::parse(&bytes, &Limits::default()).expect("parse");
        live::apply_confirmed(&key.spec(), &steps, consent()).expect("write");
        let now = key.export();
        assert!(live_steps(&now, &changed).is_empty());

        // Importing the backup over the changed key leaves the state its
        // blocks describe when they follow the current ones.
        let mut imported = now.clone();
        imported.keys.extend(reread.keys);
        let restore = live_steps(&now, &imported);
        live::apply_confirmed(&key.spec(), &restore, consent()).expect("restore");
        assert!(live_steps(&key.export(), &original).is_empty());
    }
}

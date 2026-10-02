//! Writing a text pane back to its file.
//!
//! The safe write itself is the parent module: the changed-on-disk check, the
//! temporary file and the rename over the target. What is added here is the
//! encode, which can fail: an encode that cannot represent the text is
//! reported rather than written, so a lossy save is a choice the user makes
//! and not a side effect.

use crate::save::{self, Baseline, FileSystem, SaveMessage, SaveOutcome, SaveRules};
use crate::worker::Job;
use ca_text::{EolStyle, LoadedText, SaveError, TextBuffer};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What the caller has already agreed to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveConsent {
    /// The user accepted a save that cannot reproduce every character.
    pub accept_loss: bool,
    /// The user accepted overwriting a file that changed on disk.
    pub accept_disk_change: bool,
}

/// Write `loaded` to `path`, encoding it first.
///
/// `expected` is the baseline taken when the file was read; [`Baseline::Unchecked`] skips the
/// changed-on-disk check, which is what a Save As to a new name wants. The
/// check runs before the encode, so a file that changed underneath is reported
/// rather than encoded for nothing.
pub fn save(
    files: &dyn FileSystem,
    path: &Path,
    loaded: &LoadedText,
    expected: Baseline,
    consent: SaveConsent,
) -> SaveOutcome {
    save_with_endings(files, path, loaded, expected, consent, None)
}

/// Write `loaded` to `path` as [`save`] does, with every line ending rewritten
/// to `line_endings` when one is named.
///
/// A rewrite that changes the text counts as an edit, so a file that did not
/// decode cleanly asks for consent to the loss rather than replaying its bytes.
pub fn save_with_endings(
    files: &dyn FileSystem,
    path: &Path,
    loaded: &LoadedText,
    expected: Baseline,
    consent: SaveConsent,
    line_endings: Option<EolStyle>,
) -> SaveOutcome {
    if let Some(outcome) =
        save::check_disk_change(files, path, expected, consent.accept_disk_change)
    {
        return outcome;
    }
    let converted = line_endings.and_then(|style| with_line_endings(loaded, style));
    let loaded = converted.as_ref().unwrap_or(loaded);
    let bytes = match loaded.to_bytes() {
        Ok(bytes) => bytes,
        Err(reason) => {
            if !consent.accept_loss {
                return SaveOutcome::WouldLose(describe(&reason));
            }
            loaded.to_bytes_lossy()
        }
    };
    save::save(files, path, &bytes, expected, consent.accept_disk_change)
}

/// A copy of `loaded` whose line endings are all `style`, or nothing when
/// every ending already is.
fn with_line_endings(loaded: &LoadedText, style: EolStyle) -> Option<LoadedText> {
    let text = loaded.buffer.text();
    let converted = ca_text::eol::convert(&text, style);
    if converted == text {
        return None;
    }
    let mut copy = loaded.clone();
    copy.buffer = TextBuffer::from_text("");
    copy.buffer.insert(0, &converted);
    Some(copy)
}

/// A sentence naming what a save would lose.
#[must_use]
pub fn describe(reason: &SaveError) -> String {
    match reason {
        SaveError::Unmappable { ch, encoding, .. } => {
            format!("{encoding} cannot represent the character {ch:?} in this file.")
        }
        SaveError::LossyLoad => {
            "The file did not decode cleanly, so saving cannot reproduce its original bytes."
                .to_owned()
        }
    }
}

/// Run a save on a worker thread.
#[must_use]
pub fn spawn(
    path: PathBuf,
    loaded: LoadedText,
    expected: Baseline,
    consent: SaveConsent,
    rules: SaveRules,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Job<SaveMessage> {
    save::spawn_with(notify, move || {
        save_with_endings(
            &rules.files(),
            &path,
            &loaded,
            expected,
            consent,
            rules.line_endings,
        )
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{save, SaveConsent};
    use crate::save::{Baseline, FileSystem, SaveOutcome, Stamp};
    use ca_text::{DecodeOptions, LoadedText};
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeFiles {
        content: Mutex<HashMap<PathBuf, Vec<u8>>>,
        writable: Mutex<bool>,
        change_after_stamp: Mutex<Option<Vec<u8>>>,
    }

    impl FakeFiles {
        fn with(path: &Path, bytes: &[u8]) -> Self {
            let files = Self {
                writable: Mutex::new(true),
                ..Self::default()
            };
            files.put(path, bytes);
            files
        }

        fn bytes(&self, path: &Path) -> Option<Vec<u8>> {
            self.content.lock().ok()?.get(path).cloned()
        }

        fn put(&self, path: &Path, bytes: &[u8]) {
            if let Ok(mut map) = self.content.lock() {
                map.insert(path.to_path_buf(), bytes.to_vec());
            }
        }
    }

    impl FileSystem for FakeFiles {
        fn stamp(&self, path: &Path) -> Option<Stamp> {
            let stamp = self.bytes(path).map(|bytes| Stamp {
                size: bytes.len() as u64,
                modified: None,
                identity: None,
            });
            if let Ok(mut pending) = self.change_after_stamp.lock() {
                if let Some(bytes) = pending.take() {
                    self.put(path, &bytes);
                }
            }
            stamp
        }

        fn is_writable(&self, _path: &Path) -> bool {
            self.writable.lock().map(|flag| *flag).unwrap_or(false)
        }

        fn replace(&self, target: &Path, bytes: &[u8]) -> io::Result<()> {
            let Ok(mut map) = self.content.lock() else {
                return Err(io::Error::other("locked"));
            };
            map.insert(target.to_path_buf(), bytes.to_vec());
            Ok(())
        }
    }

    fn loaded(bytes: &[u8]) -> LoadedText {
        LoadedText::load(bytes, &DecodeOptions::default())
    }

    fn loaded_utf8(bytes: &[u8]) -> LoadedText {
        LoadedText::load(
            bytes,
            &DecodeOptions {
                forced: Some(ca_text::TextEncoding::Utf8),
                ..DecodeOptions::default()
            },
        )
    }

    fn path() -> PathBuf {
        PathBuf::from("/work/file.txt")
    }

    #[test]
    fn an_edited_file_is_written_through_a_temporary_and_renamed() {
        let target = path();
        let files = FakeFiles::with(&target, b"one\n");
        let stamp = Baseline::of(&files, &target);
        let mut text = loaded(b"one\n");
        text.buffer.insert(0, "x");
        let outcome = save(&files, &target, &text, stamp, SaveConsent::default());
        assert!(matches!(outcome, SaveOutcome::Saved(_)));
        assert_eq!(files.bytes(&target).as_deref(), Some(b"xone\n".as_slice()));
        assert!(files.bytes(&target.with_extension("saving")).is_none());
    }

    #[test]
    fn a_file_that_changed_on_disk_is_not_overwritten() {
        let target = path();
        let files = FakeFiles::with(&target, b"one\n");
        let stamp = Baseline::of(&files, &target);
        files.put(&target, b"one\ntwo\n");
        let text = loaded(b"one\n");
        let outcome = save(&files, &target, &text, stamp, SaveConsent::default());
        assert_eq!(outcome, SaveOutcome::ChangedOnDisk);
        assert_eq!(
            files.bytes(&target).as_deref(),
            Some(b"one\ntwo\n".as_slice())
        );
    }

    #[test]
    fn a_change_during_text_encoding_is_not_overwritten() {
        let target = path();
        let files = FakeFiles::with(&target, b"one\n");
        let baseline = Baseline::of(&files, &target);
        *files.change_after_stamp.lock().unwrap() = Some(b"someone else wrote".to_vec());
        let mut text = loaded(b"one\n");
        text.buffer.insert(0, "x");
        assert_eq!(
            save(&files, &target, &text, baseline, SaveConsent::default()),
            SaveOutcome::ChangedOnDisk
        );
        assert_eq!(files.bytes(&target).unwrap(), b"someone else wrote");
    }

    #[test]
    fn an_accepted_disk_change_lets_the_save_through() {
        let target = path();
        let files = FakeFiles::with(&target, b"one\n");
        let stamp = Baseline::of(&files, &target);
        files.put(&target, b"changed\n");
        let text = loaded(b"one\n");
        let consent = SaveConsent {
            accept_disk_change: true,
            ..SaveConsent::default()
        };
        let outcome = save(&files, &target, &text, stamp, consent);
        assert!(matches!(outcome, SaveOutcome::Saved(_)));
        assert_eq!(files.bytes(&target).as_deref(), Some(b"one\n".as_slice()));
    }

    #[test]
    fn an_edited_lossy_load_asks_before_it_writes() {
        let target = path();
        let files = FakeFiles::with(&target, b"one\n");
        let mut text = loaded_utf8(&[0xF0, 0x28, b'\n']);
        text.buffer.insert(0, "x");
        assert!(text.had_errors);
        let outcome = save(
            &files,
            &target,
            &text,
            Baseline::Unchecked,
            SaveConsent::default(),
        );
        assert!(matches!(outcome, SaveOutcome::WouldLose(_)));
        assert_eq!(files.bytes(&target).as_deref(), Some(b"one\n".as_slice()));
    }

    #[test]
    fn an_accepted_loss_writes_the_lossy_bytes() {
        let target = path();
        let files = FakeFiles::with(&target, b"one\n");
        let mut text = loaded_utf8(&[0xF0, 0x28, b'\n']);
        text.buffer.insert(0, "x");
        let consent = SaveConsent {
            accept_loss: true,
            ..SaveConsent::default()
        };
        let outcome = save(&files, &target, &text, Baseline::Unchecked, consent);
        assert!(matches!(outcome, SaveOutcome::Saved(_)));
        assert_ne!(files.bytes(&target).as_deref(), Some(b"one\n".as_slice()));
    }

    #[test]
    fn an_unedited_lossy_load_saves_the_bytes_it_read() {
        let target = path();
        let original = [0xF0, 0x28, b'\n'];
        let files = FakeFiles::with(&target, &original);
        let text = loaded_utf8(&original);
        let outcome = save(
            &files,
            &target,
            &text,
            Baseline::Unchecked,
            SaveConsent::default(),
        );
        assert!(matches!(outcome, SaveOutcome::Saved(_)));
        assert_eq!(files.bytes(&target).as_deref(), Some(original.as_slice()));
    }

    #[test]
    fn a_read_only_target_is_refused_before_anything_is_written() {
        let target = path();
        let files = FakeFiles::with(&target, b"one\n");
        if let Ok(mut flag) = files.writable.lock() {
            *flag = false;
        }
        let text = loaded(b"one\n");
        assert_eq!(
            save(
                &files,
                &target,
                &text,
                Baseline::Unchecked,
                SaveConsent::default()
            ),
            SaveOutcome::NotWritable
        );
        assert_eq!(files.bytes(&target).as_deref(), Some(b"one\n".as_slice()));
    }
}

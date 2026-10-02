//! The quick comparison of two files.
//!
//! The answer is the exit code. Nothing is written and nothing is printed
//! except a failure.

use std::path::Path;

use ca_fs::{Cancel, ContentOutcome, RulesComparer};
use ca_script::rules::TextRules;

use crate::args::{QuickKind, QuickRun};
use crate::exit;

/// Bytes read at a time while a file is compared.
const BUFFER: usize = 64 * 1024;

/// Compare the two files and return the exit code.
#[must_use]
pub fn compare(run: &QuickRun) -> u8 {
    for path in [run.left.as_path(), run.right.as_path()] {
        if !path.is_file() {
            eprintln!("cannot read {}", path.display());
            return exit::SCRIPT_PATHS;
        }
    }
    match run.kind {
        QuickKind::Size => match (length(&run.left), length(&run.right)) {
            (Some(left), Some(right)) if left == right => exit::BINARY_SAME,
            (Some(_), Some(_)) => exit::BINARY_DIFFERENT,
            _ => exit::SCRIPT_PATHS,
        },
        QuickKind::Crc => {
            let cancel = Cancel::default();
            match (
                ca_fs::crc32(&run.left, &cancel, BUFFER),
                ca_fs::crc32(&run.right, &cancel, BUFFER),
            ) {
                (Ok(left), Ok(right)) if left == right => exit::BINARY_SAME,
                (Ok(_), Ok(_)) => exit::BINARY_DIFFERENT,
                (Err(error), _) | (_, Err(error)) => {
                    eprintln!("{error}");
                    exit::SCRIPT_PATHS
                }
            }
        }
        QuickKind::Binary => {
            let cancel = Cancel::default();
            match ca_fs::binary_equal(&run.left, &run.right, &cancel, BUFFER) {
                Ok(true) => exit::BINARY_SAME,
                Ok(false) => exit::BINARY_DIFFERENT,
                Err(error) => {
                    eprintln!("{error}");
                    exit::SCRIPT_PATHS
                }
            }
        }
        QuickKind::RulesBased => {
            match TextRules::new().compare(&run.left, &run.right, &ca_fs::Cancel::new()) {
                Ok(ContentOutcome::BinarySame | ContentOutcome::RulesSame) => exit::RULES_SAME,
                Ok(ContentOutcome::UnimportantDifferences) => exit::SIMILAR,
                Ok(_) => exit::RULES_DIFFERENT,
                Err(error) => {
                    eprintln!("{error}");
                    exit::SCRIPT_PATHS
                }
            }
        }
    }
}

fn length(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|data| data.len())
}

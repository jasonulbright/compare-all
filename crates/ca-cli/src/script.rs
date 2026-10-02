//! Running a script file named on the command line.

use ca_script::{RunError, Session, SessionOptions, Substitution};
use std::io::Read;
use std::path::Path;

use crate::args::ScriptRun;
use crate::exit;

/// Read the script file, run it, and return the exit code.
///
/// A time a command names, the clock variables and the log stamps are wall
/// clocks in the current zone of the machine.
#[must_use]
pub fn run(request: &ScriptRun) -> u8 {
    let source = match read_script_file(&request.file) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("cannot read {}: {error}", request.file.display());
            return exit::SCRIPT_NOT_LOADED;
        }
    };
    let working_directory = std::env::current_dir().unwrap_or_default();
    let offset_seconds = i64::from(ca_fs::local_offset_seconds());
    let session_options = SessionOptions {
        dry_run: request.dry_run,
        left_read_only: request.read_only.left,
        right_read_only: request.read_only.right,
        working_directory,
        profiles: std::sync::Arc::new(crate::profiles::StoredProfiles::of_settings()),
        archive_types: ca_script::paths::stored_archive_types(),
        offset_seconds,
        ..SessionOptions::default()
    };
    let mut session = Session::new(session_options);
    let subst = Substitution::from_environment(request.arguments.clone())
        .with_offset_seconds(offset_seconds);
    match ca_script::run_script(&source, &subst, &mut session) {
        Ok(outcome) => {
            if !request.silent {
                for step in &outcome.steps {
                    if let Some(error) = &step.error {
                        eprintln!("line {}: {error}", step.span.line);
                    }
                }
            }
            if outcome.failures == 0 {
                exit::SUCCESS
            } else {
                exit::UNKNOWN_ERROR
            }
        }
        Err(error) => {
            eprintln!("{error}");
            match error {
                RunError::Load(_) => exit::SCRIPT_NOT_LOADED,
                RunError::Syntax(_) => exit::SCRIPT_SYNTAX,
                RunError::LoadFailed(_) => exit::SCRIPT_PATHS,
                RunError::Stopped(_) => exit::UNKNOWN_ERROR,
            }
        }
    }
}

fn read_script_file(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let limit = ca_script::lex::MAX_SCRIPT_BYTES;
    if file.metadata()?.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("script file exceeds the {limit}-byte limit"),
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("script file exceeds the {limit}-byte limit"),
        ));
    }
    decode_script(bytes)
}

/// Script text from its bytes: UTF-8, with or without a byte order mark, or
/// UTF-16 in either order when a mark names it.
fn decode_script(bytes: Vec<u8>) -> std::io::Result<String> {
    let invalid = |detail: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, detail);
    let utf16 = |body: &[u8], unit: fn([u8; 2]) -> u16| {
        if !body.len().is_multiple_of(2) {
            return Err(invalid("a UTF-16 script holds an odd number of bytes"));
        }
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|pair| unit([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).map_err(|_| invalid("stream did not contain valid UTF-16"))
    };
    match bytes.as_slice() {
        [0xEF, 0xBB, 0xBF, body @ ..] => String::from_utf8(body.to_vec())
            .map_err(|_| invalid("stream did not contain valid UTF-8")),
        [0xFF, 0xFE, body @ ..] => utf16(body, u16::from_le_bytes),
        [0xFE, 0xFF, body @ ..] => utf16(body, u16::from_be_bytes),
        _ => String::from_utf8(bytes).map_err(|_| invalid("stream did not contain valid UTF-8")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::read_script_file;
    use ca_script::lex::MAX_SCRIPT_BYTES;
    use std::io::Write;

    #[test]
    fn an_oversized_script_is_refused_from_its_file_size() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large-script.txt");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"# script intentionally oversized").unwrap();
        file.set_len(MAX_SCRIPT_BYTES + 1).unwrap();

        let error = read_script_file(&path).expect_err("oversized script refused");

        assert!(error.to_string().contains("byte limit"));
    }

    #[test]
    fn a_script_with_a_byte_order_mark_reads_as_its_text() {
        let directory = tempfile::tempdir().unwrap();
        let text = "log normal \"x.log\"\n";
        let mut little_endian = vec![0xFF, 0xFE];
        let mut big_endian = vec![0xFE, 0xFF];
        for unit in text.encode_utf16() {
            little_endian.extend_from_slice(&unit.to_le_bytes());
            big_endian.extend_from_slice(&unit.to_be_bytes());
        }
        let mut utf8 = vec![0xEF, 0xBB, 0xBF];
        utf8.extend_from_slice(text.as_bytes());
        for (name, bytes) in [
            ("utf8.txt", utf8),
            ("le.txt", little_endian),
            ("be.txt", big_endian),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(read_script_file(&path).unwrap(), text, "{name}");
        }
    }
}

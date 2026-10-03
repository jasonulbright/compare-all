//! Starting a copy of the test binary.

use std::io::ErrorKind;
use std::process::{Command, Output};
use std::time::Duration;

/// How many times a start that fails with "text file busy" is repeated.
const BUSY_ATTEMPTS: u32 = 100;

/// Run `command` to completion, repeating the start only while it fails with
/// "text file busy". Every other error, and the last busy error, is returned.
///
/// A child forked by a parallel test while a copy of the binary is still open
/// for writing inherits that handle, and executing the copy then fails with
/// "text file busy" until the child has started.
pub fn output(command: &mut Command) -> std::io::Result<Output> {
    let mut attempts = 0;
    loop {
        match command.output() {
            Err(error)
                if error.kind() == ErrorKind::ExecutableFileBusy && attempts < BUSY_ATTEMPTS =>
            {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            outcome => return outcome,
        }
    }
}

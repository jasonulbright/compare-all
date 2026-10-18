//! Authenticode signer checks.
//!
//! The Windows implementation asks the operating system's own verifier
//! (`Get-AuthenticodeSignature` in Windows `PowerShell`) for the trust status and
//! the signer certificate of a file. That keeps the repository free of
//! `unsafe` code. The verifier needs `powershell.exe` in the Windows folder; a
//! machine without it cannot verify, and an update then fails closed.

use std::path::Path;

/// The common name every release file is signed with.
pub const RELEASE_SIGNER: &str = "Jason Ulbright";

/// Reports the signer of a file whose signature the operating system trusts.
pub trait SignatureVerifier: Send + Sync {
    /// The subject of the signer certificate.
    ///
    /// # Errors
    /// Returns why the file is not validly signed.
    fn signer_subject(&self, file: &Path) -> Result<String, String>;
}

/// Check that `file` is validly signed by `expected_common_name`.
///
/// # Errors
/// Returns why the signature is missing, untrusted or from another signer.
pub fn verify_signer(
    verifier: &dyn SignatureVerifier,
    file: &Path,
    expected_common_name: &str,
) -> Result<(), String> {
    let subject = verifier.signer_subject(file)?;
    match common_name(&subject) {
        Some(name) if name == expected_common_name => Ok(()),
        Some(name) => Err(format!(
            "{} is signed by {name}, not by {expected_common_name}",
            display_name(file)
        )),
        None => Err(format!("{} has no signer name", display_name(file))),
    }
}

fn display_name(file: &Path) -> String {
    file.file_name().map_or_else(
        || "the file".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The `CN` value of a distinguished name such as `CN=A, O="B, C", C=US`.
#[must_use]
pub fn common_name(subject: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in subject.chars() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => parts.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    parts.push(current);
    parts.iter().find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("CN")
            .then(|| value.trim().to_owned())
    })
}

/// The verifier of this platform.
#[must_use]
pub fn system_verifier() -> Box<dyn SignatureVerifier> {
    #[cfg(windows)]
    {
        Box::new(windows::PowerShellVerifier)
    }
    #[cfg(not(windows))]
    {
        Box::new(Unavailable)
    }
}

/// A verifier for platforms with no Authenticode support: it always refuses.
#[derive(Debug, Clone, Copy)]
pub struct Unavailable;

impl SignatureVerifier for Unavailable {
    fn signer_subject(&self, _file: &Path) -> Result<String, String> {
        Err("this platform cannot verify Authenticode signatures".to_owned())
    }
}

#[cfg(windows)]
mod windows {
    use super::SignatureVerifier;
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const LIMIT: Duration = Duration::from_secs(120);
    const SCRIPT: &str = "$ErrorActionPreference='Stop';\
        $s=Get-AuthenticodeSignature -LiteralPath $env:CA_UPDATE_FILE;\
        if($s.Status -ne 'Valid'){[Console]::Out.Write('STATUS:'+$s.Status);exit 3};\
        [Console]::Out.Write('OK:'+$s.SignerCertificate.Subject)";

    pub struct PowerShellVerifier;

    impl SignatureVerifier for PowerShellVerifier {
        fn signer_subject(&self, file: &Path) -> Result<String, String> {
            let root = std::env::var_os("SystemRoot").ok_or("SystemRoot is not set")?;
            let shell = Path::new(&root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe");
            let mut child = ca_io::host_command::host_command(shell)
                .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
                .env("CA_UPDATE_FILE", file)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map_err(|error| format!("cannot start the signature check: {error}"))?;
            let started = Instant::now();
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) if started.elapsed() > LIMIT => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("the signature check did not finish".to_owned());
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(error) => return Err(error.to_string()),
                }
            };
            let mut text = String::new();
            if let Some(mut out) = child.stdout.take() {
                let _ = out.read_to_string(&mut text);
            }
            let text = text.trim();
            match text.strip_prefix("OK:") {
                Some(subject) if status.success() => Ok(subject.to_owned()),
                _ => Err(format!(
                    "the signature of the file is not valid ({})",
                    text.strip_prefix("STATUS:").unwrap_or("check failed")
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static str);
    impl SignatureVerifier for Fixed {
        fn signer_subject(&self, _: &Path) -> Result<String, String> {
            Ok(self.0.to_owned())
        }
    }

    #[test]
    fn the_common_name_is_read_from_a_subject() {
        assert_eq!(
            common_name("CN=Jason Ulbright, O=\"Ulbright, Jason\", C=US").as_deref(),
            Some("Jason Ulbright")
        );
        assert_eq!(common_name("O=x"), None);
    }

    #[test]
    fn another_signer_is_refused() {
        let file = Path::new("a.msi");
        assert!(verify_signer(&Fixed("CN=Jason Ulbright, C=US"), file, RELEASE_SIGNER).is_ok());
        assert!(verify_signer(&Fixed("CN=Someone Else"), file, RELEASE_SIGNER).is_err());
        assert!(verify_signer(&Fixed("CN=Jason Ulbright Ltd"), file, RELEASE_SIGNER).is_err());
    }
}

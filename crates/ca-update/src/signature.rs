//! Authenticode signer checks.
//!
//! The Windows implementation asks the operating system's own verifier
//! (`Get-AuthenticodeSignature` in Windows `PowerShell`) for the trust status,
//! the signer certificate and its chain. That keeps the repository free of
//! `unsafe` code. The verifier needs `powershell.exe` in the Windows folder and
//! the full language mode; without them the check fails and no update is
//! staged.
//!
//! The operating system answers "valid" for a chain to any root in the
//! Windows store, including a root an organization or an attacker added. A
//! file therefore passes only when every condition of [`check_facts`] holds:
//! an embedded (not catalog) signature with a timestamp, a chain that ends at
//! the pinned root, the signing service key usage and the key usage of the
//! release certificate profile, and the release common name.

use std::path::Path;

/// The common name every release file is signed with.
pub const RELEASE_SIGNER: &str = "Jason Ulbright";

/// SHA-1 thumbprint of the root every release certificate chains to
/// (Microsoft Identity Verification Root Certificate Authority 2020).
pub const RELEASE_ROOT_THUMBPRINT: &str = "F40042E2E5F7E8EF8189FED15519AECE42C3BFA2";

/// Key usage of every public-trust certificate of the signing service.
pub const SIGNING_SERVICE_EKU: &str = "1.3.6.1.4.1.311.97.1.0";

/// Key usage of the certificate profile that signs releases. Every certificate
/// of one profile carries the same value; it changes only when the profile is
/// created again, and then this value and the release check change together.
pub const RELEASE_PROFILE_EKU: &str = "1.3.6.1.4.1.311.97.230016310.8135122.214234108.684336535";

/// What the operating system reports about the signature of a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignerFacts {
    /// Trust status, `Valid` when the operating system trusts the signature.
    pub status: String,
    /// `Authenticode` for a signature embedded in the file, `Catalog` for a
    /// signature that comes from a system catalog.
    pub signature_type: String,
    /// True when the signature carries a timestamp.
    pub timestamped: bool,
    /// True when the chain of the signer certificate was built without error.
    pub chain_built: bool,
    /// SHA-1 thumbprint of the last certificate of the chain.
    pub root_thumbprint: String,
    /// Extended key usages of the signer certificate.
    pub key_usages: Vec<String>,
    /// Subject of the signer certificate.
    pub subject: String,
}

impl SignerFacts {
    /// The facts of a file signed the way every release file is, by
    /// `subject`.
    #[must_use]
    pub fn release(subject: &str) -> Self {
        Self {
            status: "Valid".to_owned(),
            signature_type: "Authenticode".to_owned(),
            timestamped: true,
            chain_built: true,
            root_thumbprint: RELEASE_ROOT_THUMBPRINT.to_owned(),
            key_usages: vec![
                SIGNING_SERVICE_EKU.to_owned(),
                "1.3.6.1.5.5.7.3.3".to_owned(),
                RELEASE_PROFILE_EKU.to_owned(),
            ],
            subject: subject.to_owned(),
        }
    }

    /// Read the `KEY=value` lines the verifier script prints. The output must
    /// end with `END`, so a cut-off answer is refused.
    ///
    /// # Errors
    /// Returns why the text is not a complete answer.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut facts = Self::default();
        let mut seen = std::collections::BTreeSet::new();
        let mut ended = false;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if ended {
                return Err("the signature check printed more than expected".to_owned());
            }
            if line == "END" {
                ended = true;
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err("the signature check printed an unknown line".to_owned());
            };
            if key != "EKU" && !seen.insert(key.to_owned()) {
                return Err(format!("the signature check printed {key} twice"));
            }
            match key {
                "STATUS" => value.clone_into(&mut facts.status),
                "TYPE" => value.clone_into(&mut facts.signature_type),
                "TIMESTAMP" => facts.timestamped = value == "1",
                "CHAIN" => facts.chain_built = value.eq_ignore_ascii_case("true"),
                "ROOT" => value.clone_into(&mut facts.root_thumbprint),
                "EKU" => facts.key_usages.push(value.to_owned()),
                "SUBJECT" => value.clone_into(&mut facts.subject),
                _ => return Err("the signature check printed an unknown line".to_owned()),
            }
        }
        if ended {
            Ok(facts)
        } else {
            Err("the signature check did not finish its answer".to_owned())
        }
    }
}

/// Reports what the operating system knows about the signature of a file.
pub trait SignatureVerifier: Send + Sync {
    /// The signature facts of `file`.
    ///
    /// # Errors
    /// Returns why the facts could not be read.
    fn signer_facts(&self, file: &Path) -> Result<SignerFacts, String>;
}

/// Check that `file` is a release file signed by `expected_common_name`.
///
/// # Errors
/// Returns why the signature is missing, untrusted or from another signer.
pub fn verify_signer(
    verifier: &dyn SignatureVerifier,
    file: &Path,
    expected_common_name: &str,
) -> Result<(), String> {
    let facts = verifier.signer_facts(file)?;
    check_facts(&facts, expected_common_name)
        .map_err(|reason| format!("{}: {reason}", display_name(file)))
}

/// Check the facts of one signature against the release signature.
///
/// # Errors
/// Returns the first condition the signature fails.
pub fn check_facts(facts: &SignerFacts, expected_common_name: &str) -> Result<(), String> {
    if facts.status != "Valid" {
        return Err(format!("the signature is not valid ({})", facts.status));
    }
    if facts.signature_type != "Authenticode" {
        return Err(format!(
            "the file carries no signature of its own ({})",
            facts.signature_type
        ));
    }
    if !facts.timestamped {
        return Err("the signature has no timestamp".to_owned());
    }
    if !facts.chain_built {
        return Err("the certificate chain is not valid".to_owned());
    }
    if !facts
        .root_thumbprint
        .eq_ignore_ascii_case(RELEASE_ROOT_THUMBPRINT)
    {
        return Err("the certificate does not chain to the release root".to_owned());
    }
    for usage in [SIGNING_SERVICE_EKU, RELEASE_PROFILE_EKU] {
        if !facts.key_usages.iter().any(|have| have == usage) {
            return Err("the certificate is not from the release signing profile".to_owned());
        }
    }
    match common_name(&facts.subject) {
        Some(name) if name == expected_common_name => Ok(()),
        Some(name) => Err(format!("signed by {name}, not by {expected_common_name}")),
        None => Err("the certificate names no signer".to_owned()),
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
    fn signer_facts(&self, _file: &Path) -> Result<SignerFacts, String> {
        Err("this platform cannot verify Authenticode signatures".to_owned())
    }
}

#[cfg(windows)]
mod windows {
    use super::{SignatureVerifier, SignerFacts};
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const LIMIT: Duration = Duration::from_secs(120);
    // The security module is imported by its full path in the system folder
    // and the cmdlet is called by its module-qualified name, so a module of
    // the same name elsewhere cannot answer in its place.
    const SCRIPT: &str = "$ErrorActionPreference='Stop';\
        Import-Module -Name ($env:SystemRoot+'\\System32\\WindowsPowerShell\\v1.0\\Modules\\Microsoft.PowerShell.Security\\Microsoft.PowerShell.Security.psd1');\
        $p=$env:CA_UPDATE_FILE;\
        $s=Microsoft.PowerShell.Security\\Get-AuthenticodeSignature -LiteralPath $p;\
        $o=[Console]::Out;\
        $o.WriteLine('STATUS='+$s.Status);\
        $o.WriteLine('TYPE='+$s.SignatureType);\
        $o.WriteLine('TIMESTAMP='+[int]($null -ne $s.TimeStamperCertificate));\
        $c=$s.SignerCertificate;\
        if($null -ne $c){\
        $o.WriteLine('SUBJECT='+$c.Subject);\
        foreach($x in $c.Extensions){if($x -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]){foreach($u in $x.EnhancedKeyUsages){$o.WriteLine('EKU='+$u.Value)}}};\
        $k=New-Object Security.Cryptography.X509Certificates.X509Certificate2Collection;\
        $k.Import($p);\
        $h=New-Object Security.Cryptography.X509Certificates.X509Chain;\
        $h.ChainPolicy.RevocationMode='NoCheck';\
        $h.ChainPolicy.VerificationFlags='IgnoreNotTimeValid';\
        $h.ChainPolicy.ExtraStore.AddRange($k);\
        $o.WriteLine('CHAIN='+$h.Build($c));\
        $e=$h.ChainElements;\
        if($e.Count -gt 0){$o.WriteLine('ROOT='+$e[$e.Count-1].Certificate.Thumbprint)}};\
        $o.WriteLine('END')";

    pub struct PowerShellVerifier;

    impl SignatureVerifier for PowerShellVerifier {
        fn signer_facts(&self, file: &Path) -> Result<SignerFacts, String> {
            let root = crate::system::system_root()?;
            let base = root.join("System32").join("WindowsPowerShell").join("v1.0");
            let mut child = ca_io::host_command::host_command(base.join("powershell.exe"))
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-NoLogo",
                    "-Command",
                    SCRIPT,
                ])
                .env_clear()
                .env("SystemRoot", &root)
                .env("PSModulePath", base.join("Modules"))
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
            if let Some(out) = child.stdout.take() {
                let _ = out.take(64 * 1024).read_to_string(&mut text);
            }
            if !status.success() {
                return Err("the signature check failed".to_owned());
            }
            SignerFacts::parse(&text)
        }
    }
}

#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used)]
mod system_tests {
    use std::path::Path;

    const PROBE: &str = "CA_SIGNATURE_PROBE";

    /// Runs the real verifier only inside the child process started below,
    /// or by hand with `CA_SIGNATURE_PROBE` naming a signed release file.
    #[test]
    fn probe_child() {
        let Some(file) = std::env::var_os(PROBE) else {
            return;
        };
        let answer = super::verify_signer(
            super::system_verifier().as_ref(),
            Path::new(&file),
            super::RELEASE_SIGNER,
        );
        println!(
            "PROBE-RESULT:{}",
            match answer {
                Ok(()) => "accepted".to_owned(),
                Err(reason) => format!("refused {reason}"),
            }
        );
    }

    #[test]
    fn a_module_planted_on_the_module_path_cannot_change_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        let module = dir
            .path()
            .join("Modules")
            .join("Microsoft.PowerShell.Security");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::write(
            module.join("Microsoft.PowerShell.Security.psm1"),
            "function Get-AuthenticodeSignature { param($LiteralPath) \
             [pscustomobject]@{ Status = 'Valid'; SignatureType = 'Authenticode'; \
             TimeStamperCertificate = 1; \
             SignerCertificate = [pscustomobject]@{ Subject = 'CN=Jason Ulbright, C=US'; Extensions = @() } } }",
        )
        .unwrap();
        let file = dir.path().join("unsigned.exe");
        std::fs::write(&file, b"MZ not signed").unwrap();
        let output = ca_io::host_command::host_command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "signature::system_tests::probe_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("PSModulePath", dir.path().join("Modules"))
            .env(PROBE, &file)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("PROBE-RESULT:refused"), "{text}");
        // The planted function answers `Valid`; any other status shows that
        // the system cmdlet answered.
        assert!(text.contains("the signature is not valid ("), "{text}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    type Tamper = fn(&mut SignerFacts);

    struct Fixed(SignerFacts);
    impl SignatureVerifier for Fixed {
        fn signer_facts(&self, _: &Path) -> Result<SignerFacts, String> {
            Ok(self.0.clone())
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
        let release = |subject| Fixed(SignerFacts::release(subject));
        assert!(verify_signer(&release("CN=Jason Ulbright, C=US"), file, RELEASE_SIGNER).is_ok());
        assert!(verify_signer(&release("CN=Someone Else"), file, RELEASE_SIGNER).is_err());
        assert!(verify_signer(&release("CN=Jason Ulbright Ltd"), file, RELEASE_SIGNER).is_err());
    }

    #[test]
    fn a_release_name_without_the_release_chain_and_profile_is_refused() {
        let good = SignerFacts::release("CN=Jason Ulbright, O=Jason Ulbright, C=US");
        assert!(check_facts(&good, RELEASE_SIGNER).is_ok());
        let cases: [(&str, Tamper); 9] = [
            ("untrusted", |f| f.status = "UnknownError".to_owned()),
            ("catalog", |f| f.signature_type = "Catalog".to_owned()),
            ("no timestamp", |f| f.timestamped = false),
            ("broken chain", |f| f.chain_built = false),
            ("organization root", |f| {
                f.root_thumbprint = "0123456789ABCDEF0123456789ABCDEF01234567".to_owned();
            }),
            ("no root", |f| f.root_thumbprint.clear()),
            ("no service usage", |f| {
                f.key_usages.retain(|u| u != SIGNING_SERVICE_EKU);
            }),
            ("another profile", |f| {
                f.key_usages.retain(|u| u != RELEASE_PROFILE_EKU);
                f.key_usages.push("1.3.6.1.4.1.311.97.1.2.3.4".to_owned());
            }),
            ("no subject", |f| f.subject.clear()),
        ];
        for (label, tamper) in cases {
            let mut facts = good.clone();
            tamper(&mut facts);
            assert!(check_facts(&facts, RELEASE_SIGNER).is_err(), "{label}");
        }
        let mut lower = good;
        lower.root_thumbprint = RELEASE_ROOT_THUMBPRINT.to_ascii_lowercase();
        assert!(check_facts(&lower, RELEASE_SIGNER).is_ok());
    }

    #[test]
    fn the_verifier_answer_is_read_only_when_complete() {
        let text = format!(
            "STATUS=Valid\nTYPE=Authenticode\nTIMESTAMP=1\nSUBJECT=CN=Jason Ulbright, C=US\n\
             EKU={SIGNING_SERVICE_EKU}\nEKU=1.3.6.1.5.5.7.3.3\nEKU={RELEASE_PROFILE_EKU}\n\
             CHAIN=True\nROOT={RELEASE_ROOT_THUMBPRINT}\nEND\n"
        );
        let facts = SignerFacts::parse(&text).unwrap();
        assert!(check_facts(&facts, RELEASE_SIGNER).is_ok());
        assert!(SignerFacts::parse(&text.replace("END\n", "")).is_err());
        assert!(SignerFacts::parse(&format!("{text}STATUS=Valid\n")).is_err());
        assert!(SignerFacts::parse(&format!("STATUS=Valid\n{text}")).is_err());
        assert!(SignerFacts::parse(&text.replace("CHAIN=True", "OTHER=1")).is_err());
        let unsigned = SignerFacts::parse("STATUS=NotSigned\nTYPE=None\nTIMESTAMP=0\nEND").unwrap();
        assert!(check_facts(&unsigned, RELEASE_SIGNER).is_err());
    }
}

//! The console program carries its build version where the updater reads it.

#![allow(clippy::unwrap_used)]

#[cfg(windows)]
#[test]
fn the_program_file_carries_its_version_for_the_updater() {
    let bytes = std::fs::read(env!("CARGO_BIN_EXE_ca")).unwrap();
    assert_eq!(
        ca_update::verify::embedded_version(&bytes).unwrap(),
        ca_update::Version::parse(env!("CA_VERSION")).unwrap()
    );
}

/// The version information as Windows reads it from the file.
#[cfg(all(windows, target_env = "msvc"))]
#[test]
fn the_program_file_names_the_product_and_its_version() {
    let root = ca_update::system::system_root().unwrap();
    let output = ca_io::host_command::host_command(
        root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    )
    .args(["-NoProfile", "-NonInteractive", "-Command"])
    .arg(
        "$v=(Get-Item -LiteralPath $env:CA_TARGET).VersionInfo;\
         $v.FileDescription;$v.ProductName;$v.FileVersion;$v.ProductVersion;\
         $v.OriginalFilename",
    )
    .env("CA_TARGET", env!("CARGO_BIN_EXE_ca"))
    .output()
    .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let version = env!("CA_VERSION");
    assert_eq!(
        lines,
        [
            "Compare All command line",
            "Compare All",
            version,
            version,
            "ca.exe"
        ]
    );
}

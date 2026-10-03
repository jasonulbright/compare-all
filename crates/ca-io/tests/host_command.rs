//! What a real host program sees when it is started from an image.
//!
//! Each case runs this test binary again with an image environment, so the
//! test process itself never changes a variable; the child starts the host
//! `env`. A copy placed inside the image folder runs as the application does;
//! the binary at its own place runs as a program outside the image.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const PROBE: &str = "COMPARE_ALL_HOST_COMMAND_PROBE";
const MARKER: &str = "child environment:";

#[test]
fn host_command_probe() {
    if std::env::var_os(PROBE).is_none() {
        return;
    }
    let output = ca_io::host_command::host_command("env").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    println!("{MARKER}");
    print!("{}", String::from_utf8_lossy(&output.stdout));
}

fn write_program(path: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    std::fs::write(path, text)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// A copy of this test binary inside `image`, so that the copy runs from the
/// image as the application does.
fn copy_into(image: &Path) -> Result<std::path::PathBuf> {
    let current = std::env::current_exe()?;
    let copy = image
        .join("shared/bin")
        .join(current.file_name().ok_or("no file name")?);
    std::fs::create_dir_all(copy.parent().ok_or("no parent")?)?;
    std::fs::copy(&current, &copy)?;
    Ok(copy)
}

#[test]
fn a_program_outside_the_image_keeps_the_environment() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let image = folder.path().join(".mount_TermTest");
    let root = image.display().to_string();
    write_program(
        &image.join("bin/env"),
        "#!/bin/sh\necho the terminal image copy of env ran\n",
    )?;
    #[allow(
        clippy::disallowed_methods,
        reason = "the test binary is started with an exact environment"
    )]
    let mut command = std::process::Command::new(std::env::current_exe()?);
    let output = command
        .args(["--exact", "host_command_probe", "--nocapture"])
        .env_clear()
        .env(PROBE, "1")
        .env("PATH", format!("{root}/bin:/usr/bin:/bin"))
        .env("APPDIR", &image)
        .env("APPIMAGE", folder.path().join("Term.AppImage"))
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("the terminal image copy of env ran"),
        "{stdout}"
    );
    Ok(())
}

#[test]
fn a_host_program_started_from_an_image_sees_no_image_path() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let image = folder.path().join(".mount_comparTest");
    let root = image.display().to_string();
    write_program(
        &image.join("bin/env"),
        "#!/bin/sh\necho the image copy of env ran\n",
    )?;
    let image_file = folder.path().join("compare-all.AppImage");
    let home = folder.path().join("home");
    let program = copy_into(&image)?;
    #[allow(
        clippy::disallowed_methods,
        reason = "the test binary is started with an exact environment"
    )]
    let mut command = std::process::Command::new(program);
    let output = command
        .args(["--exact", "host_command_probe", "--nocapture"])
        .env_clear()
        .env(PROBE, "1")
        .env("PATH", format!("{root}/bin:/usr/bin:/bin"))
        .env("APPDIR", &image)
        .env("SHARUN_DIR", &image)
        .env("APPIMAGE", &image_file)
        .env("ARGV0", &image_file)
        .env("OWD", folder.path())
        .env("CROSS_LIBC_DLOPEN_ROOT", &image)
        .env("GCONV_PATH", format!("{root}/lib/gconv"))
        .env("LIBGL_DRIVERS_PATH", format!("{root}/lib/dri"))
        .env("TERMINFO", format!("{root}/share/terminfo"))
        .env(
            "XDG_DATA_DIRS",
            format!("{root}/share:/usr/local/share:/usr/share"),
        )
        .env("HOME", &home)
        .env("LANG", "C.UTF-8")
        .env("TZ", "UTC")
        .env("COMPARE_ALL_USER_SETTING", "kept:as:is")
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert!(output.status.success(), "{stdout}");
    let seen = stdout
        .split_once(MARKER)
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("the probe did not run: {stdout}"))?;
    assert!(!seen.contains("the image copy"), "{seen}");
    let lines: Vec<&str> = seen.lines().filter(|line| line.contains('=')).collect();
    let leaked: Vec<&&str> = lines.iter().filter(|line| line.contains(&root)).collect();
    assert!(leaked.is_empty(), "{leaked:?}");
    for gone in ["APPDIR=", "SHARUN_DIR=", "APPIMAGE=", "ARGV0=", "OWD="] {
        assert!(!lines.iter().any(|line| line.starts_with(gone)), "{gone}");
    }
    for kept in [
        "PATH=/usr/bin:/bin".to_owned(),
        "XDG_DATA_DIRS=/usr/local/share:/usr/share".to_owned(),
        format!("HOME={}", home.display()),
        "LANG=C.UTF-8".to_owned(),
        "TZ=UTC".to_owned(),
        "COMPARE_ALL_USER_SETTING=kept:as:is".to_owned(),
    ] {
        assert!(lines.contains(&kept.as_str()), "{kept} in {lines:?}");
    }
    Ok(())
}

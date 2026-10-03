//! The time zone read started from an image.
//!
//! The case runs a copy of this test binary placed inside an image folder, with
//! an image environment and a `date` on `PATH` that records its environment
//! before it runs the real `date`, so the test process itself never changes a
//! variable.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const PROBE: &str = "COMPARE_ALL_ZONE_PROBE";

#[test]
fn zone_probe() {
    if std::env::var_os(PROBE).is_some() {
        println!("offset={}", ca_fs::zone::local_offset_seconds());
    }
}

fn write_program(path: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    std::fs::write(path, text)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[test]
fn the_date_child_gets_no_image_variable_and_still_reads_the_zone() -> Result<()> {
    let folder = tempfile::tempdir()?;
    let image = folder.path().join(".mount_comparZone");
    let root = image.display().to_string();
    let spy = folder.path().join("spy");
    let record = folder.path().join("date.env");
    write_program(
        &spy.join("date"),
        &format!(
            "#!/bin/sh\nenv > '{}'\nfor real in /usr/bin/date /bin/date; do\n  [ -x \"$real\" ] && exec \"$real\" \"$@\"\ndone\nexit 127\n",
            record.display()
        ),
    )?;
    let current = std::env::current_exe()?;
    let program = image
        .join("shared/bin")
        .join(current.file_name().ok_or("no file name")?);
    std::fs::create_dir_all(program.parent().ok_or("no parent")?)?;
    std::fs::copy(&current, &program)?;
    #[allow(
        clippy::disallowed_methods,
        reason = "a copy of the test binary inside the image is started with an exact environment"
    )]
    let mut command = std::process::Command::new(program);
    let output = command
        .args(["--exact", "zone_probe", "--nocapture"])
        .env_clear()
        .env(PROBE, "1")
        .env(
            "PATH",
            format!("{root}/bin:{}:/usr/bin:/bin", spy.display()),
        )
        .env("APPDIR", &image)
        .env("SHARUN_DIR", &image)
        .env("APPIMAGE", folder.path().join("compare-all.AppImage"))
        .env("CROSS_LIBC_DLOPEN_ROOT", &image)
        .env("GCONV_PATH", format!("{root}/lib/gconv"))
        .env("LIBGL_DRIVERS_PATH", format!("{root}/lib/dri"))
        .env("TERMINFO", format!("{root}/share/terminfo"))
        .env("LANG", "C")
        .env("TZ", "IST-5:30")
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert!(output.status.success(), "{stdout}");
    let seen = std::fs::read_to_string(&record)?;
    let leaked: Vec<&str> = seen.lines().filter(|line| line.contains(&root)).collect();
    assert!(leaked.is_empty(), "{leaked:?}");
    assert!(seen.lines().any(|line| line == "TZ=IST-5:30"), "{seen}");
    assert!(
        stdout.lines().any(|line| line == "offset=19800"),
        "{stdout}"
    );
    Ok(())
}

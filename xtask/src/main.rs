//! `cargo xtask <cmd>`. `bump` increments `BUILD_NUMBER`; `version` prints the
//! version the next build embeds; `icon` regenerates the application icon files
//! from their SVG; `package` builds the Windows installer from the release
//! binaries.

mod icon;
mod notices;
mod package;
mod version;

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| anyhow::anyhow!("xtask has no parent directory"))?;
    let file = root.join("BUILD_NUMBER");
    match std::env::args().nth(1).as_deref() {
        Some("bump") => {
            let n: u32 = std::fs::read_to_string(&file)?.trim().parse()?;
            std::fs::write(&file, format!("{:04}\n", n + 1))?;
            println!("{}", version::compute());
        }
        Some("version") => println!("{}", version::compute()),
        Some("build") => build(root)?,
        Some("icon") => icon::run(root)?,
        Some("package") => package::run(root)?,
        Some("notices") => notices::run(root)?,
        _ => anyhow::bail!("usage: cargo xtask <bump|version|build|icon|package|notices>"),
    }
    Ok(())
}

/// Runs `cargo build` with the build date supplied to the build scripts.
///
/// Cargo cannot see the calendar, so a build script left to its own devices
/// stamps the date of the last compile. Passing the date as a tracked
/// environment variable makes it an input: the day it changes, cargo rebuilds
/// what carries the version; within one day nothing is rebuilt. Arguments after
/// `build` are passed to cargo unchanged.
fn build(root: &Path) -> anyhow::Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = std::process::Command::new(cargo)
        .current_dir(root)
        .arg("build")
        .args(std::env::args().skip(2))
        .env(version::BUILD_DATE_VAR, version::today_iso())
        .status()?;
    if !status.success() {
        anyhow::bail!("cargo build failed");
    }
    Ok(())
}

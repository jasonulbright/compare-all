//! `cargo xtask <cmd>`. `bump` increments `BUILD_NUMBER`; `version` prints the
//! version the next build embeds; `icon` regenerates the application icon files
//! from their SVG; `package` builds the Windows installer from the release
//! binaries.

#![allow(
    clippy::disallowed_methods,
    reason = "build tasks run the toolchain directly on the build machine"
)]

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
/// `build` select the packages and profile; workspace builds exclude this
/// already-running helper so Windows never has to replace its executable.
fn build(root: &Path) -> anyhow::Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = std::process::Command::new(cargo)
        .current_dir(root)
        .arg("build")
        .args(build_arguments(std::env::args().skip(2).collect()))
        .env(version::BUILD_DATE_VAR, version::today_iso())
        .status()?;
    if !status.success() {
        anyhow::bail!("cargo build failed");
    }
    Ok(())
}

fn build_arguments(mut arguments: Vec<String>) -> Vec<String> {
    let workspace = arguments
        .iter()
        .any(|arg| arg == "--workspace" || arg == "--all");
    let selected = arguments
        .iter()
        .any(|arg| arg == "--package" || arg.starts_with("--package=") || arg.starts_with("-p"));
    if workspace || !selected {
        if !workspace {
            arguments.push("--workspace".to_owned());
        }
        arguments.extend(["--exclude".to_owned(), "xtask".to_owned()]);
    }
    arguments
}

#[cfg(test)]
mod build_tests {
    #[test]
    fn workspace_build_excludes_the_running_helper() {
        for arguments in [
            vec![],
            vec!["--workspace"],
            vec!["--workspace", "--release"],
        ] {
            let args = super::build_arguments(arguments.into_iter().map(str::to_owned).collect());
            assert!(args.windows(2).any(|pair| pair == ["--exclude", "xtask"]));
            assert!(args.iter().any(|arg| arg == "--workspace"));
        }
    }
    #[test]
    fn selected_packages_keep_their_build_options() {
        let arguments = vec!["-p".to_owned(), "ca-app".to_owned(), "--release".to_owned()];
        assert_eq!(super::build_arguments(arguments.clone()), arguments);
    }
}

//! `cargo xtask package`: builds `target/package/compare-all-<version>-x64.msi`
//! from the release binaries with the `wix` .NET tool.
//!
//! The version is `CA_VERSION` when set, else the version `ca.exe` reports,
//! so the installer always carries the version the binaries embed.

use anyhow::{bail, Context};
use std::path::Path;
use std::process::Command;

/// Environment variable that names the version instead of `ca.exe`.
const VERSION_VAR: &str = "CA_VERSION";

/// Writes the MSI and prints its path.
pub fn run(root: &Path) -> anyhow::Result<()> {
    let bin = root.join("target").join("release");
    for name in ["compare-all.exe", "ca.exe"] {
        if !bin.join(name).is_file() {
            bail!(
                "{} is missing; run `cargo xtask build --release -p ca-app -p ca-cli` first",
                bin.join(name).display()
            );
        }
    }
    let full = match std::env::var(VERSION_VAR) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
        _ => reported_version(&bin.join("ca.exe"))?,
    };
    let product = msi_version(&full)?;
    let out = root.join("target").join("package");
    std::fs::create_dir_all(&out).context("cannot create target/package")?;
    let msi = out.join(format!("compare-all-{full}-x64.msi"));
    let mut wix = Command::new("wix");
    wix.current_dir(root)
        .arg("build")
        .arg(root.join("installer").join("compare-all.wxs"))
        .args(["-arch", "x64"])
        .arg("-d")
        .arg(format!("ProductVersion={product}"))
        .arg("-d")
        .arg(format!("FullVersion={full}"))
        .arg("-d")
        .arg(format!("BinDir={}", bin.display()))
        .arg("-d")
        .arg(format!("DocDir={}", root.display()))
        .arg("-d")
        .arg(format!(
            "IconFile={}",
            root.join("assets")
                .join("icon")
                .join("compare-all.ico")
                .display()
        ));
    let license = root.join("LICENSE");
    if !license.is_file() {
        bail!("LICENSE is missing");
    }
    wix.arg("-d")
        .arg(format!("LicenseMit={}", license.display()));
    wix.arg("-o").arg(&msi);
    let status = wix
        .status()
        .context("cannot start `wix`; install it with `dotnet tool install --global wix`")?;
    if !status.success() {
        bail!("wix build failed");
    }
    // ICE57 reads a shortcut folder as per-user data. In a package whose scope
    // is decided at install time the folder and the HKMU key path resolve to
    // the same scope, so the check reports a mismatch that cannot occur.
    let status = Command::new("wix")
        .current_dir(root)
        .args(["msi", "validate", "-sice", "ICE57"])
        .arg(&msi)
        .status()
        .context("cannot start `wix msi validate`")?;
    if !status.success() {
        bail!("the MSI failed validation");
    }
    println!("{}", msi.display());
    Ok(())
}

/// The last word `ca --version` prints.
fn reported_version(ca: &Path) -> anyhow::Result<String> {
    let output = Command::new(ca)
        .arg("--version")
        .output()
        .with_context(|| format!("cannot run {}", ca.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .last()
        .map(str::to_owned)
        .context("ca --version printed nothing")
}

/// Maps `YYYY.MM.DD.NNNN` to the MSI `ProductVersion` `YY.MM.NNNN`.
///
/// Windows Installer compares three fields: the first and the second at most
/// 255, the third at most 65535. The day has no room. The build number never
/// decreases and never repeats, so the three fields keep the order of the full
/// versions through the year 2255 and build number 65535.
pub fn msi_version(full: &str) -> anyhow::Result<String> {
    let parts: Vec<&str> = full.split('.').collect();
    let [year, month, day, build] = parts.as_slice() else {
        bail!("{full:?} is not of the form YYYY.MM.DD.NNNN");
    };
    let number = |text: &str| -> anyhow::Result<u32> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("{full:?} is not of the form YYYY.MM.DD.NNNN");
        }
        Ok(text.parse()?)
    };
    let (year, month, day, build) = (number(year)?, number(month)?, number(day)?, number(build)?);
    if !(2000..=2255).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        bail!("{full:?} holds a date outside the range an MSI version can carry");
    }
    if build > 65_535 {
        bail!("{full:?} holds a build number above 65535");
    }
    Ok(format!("{}.{}.{}", year - 2000, month, build))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::msi_version;

    #[test]
    fn the_msi_version_is_year_month_build() {
        assert_eq!(msi_version("2026.09.25.0031").unwrap(), "26.9.31");
        assert_eq!(msi_version("2026.12.31.12345").unwrap(), "26.12.12345");
    }

    #[test]
    fn the_msi_version_keeps_the_order_of_the_full_versions() {
        let ordered = [
            "2026.09.25.0031",
            "2026.09.25.0032",
            "2026.09.26.0033",
            "2026.10.01.0034",
            "2027.01.01.0035",
        ];
        let fields = |text: &str| -> Vec<u32> {
            msi_version(text)
                .unwrap()
                .split('.')
                .map(|part| part.parse().unwrap())
                .collect()
        };
        for pair in ordered.windows(2) {
            assert!(fields(pair[0]) < fields(pair[1]), "{pair:?}");
        }
    }

    #[test]
    fn a_version_an_msi_cannot_carry_is_refused() {
        for bad in [
            "",
            "2026.09.25",
            "2026.09.25.0031.1",
            "2026.13.01.0001",
            "2256.01.01.0001",
            "1999.01.01.0001",
            "2026.09.25.65536",
            "2026.09.x5.0001",
        ] {
            assert!(msi_version(bad).is_err(), "{bad:?} must be refused");
        }
    }
}

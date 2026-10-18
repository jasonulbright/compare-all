//! Embeds the build version as `CA_VERSION` and, on Windows, the version
//! information as a resource of the `ca` executable.

#[path = "../../xtask/src/version.rs"]
mod version;

#[path = "../../xtask/src/resource.rs"]
mod resource;

use std::path::PathBuf;

fn main() {
    // The build date is an input cargo cannot see, so it is supplied as an
    // environment variable and tracked as one. Nothing else makes the date
    // reach the binary without recompiling the crate on every invocation.
    println!("cargo:rerun-if-env-changed=CA_BUILD_DATE");
    println!("cargo:rerun-if-changed=../../BUILD_NUMBER");
    let version = version::compute();
    println!("cargo:rustc-env=CA_VERSION={version}");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        if let Err(error) = embed_version(&version) {
            println!("cargo:warning=the version information is not embedded: {error}");
        }
    }
}

/// Writes the version information as a compiled resource file and hands it
/// to the linker, which converts a `.res` file itself.
fn embed_version(version: &str) -> Result<(), String> {
    let mut res = resource::new_resource_file();
    resource::append_version(
        &mut res,
        &resource::VersionInfo {
            version,
            description: "Compare All command line",
            product: "Compare All",
            company: "Jason Ulbright",
            file_name: "ca.exe",
        },
    )?;
    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(|error| error.to_string())?);
    let path = out.join("ca.res");
    std::fs::write(&path, res).map_err(|error| error.to_string())?;
    println!("cargo:rustc-link-arg-bin=ca={}", path.display());
    Ok(())
}

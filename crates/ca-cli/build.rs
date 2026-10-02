//! Embeds the build version as `CA_VERSION`.

#[path = "../../xtask/src/version.rs"]
mod version;

fn main() {
    // The build date is an input cargo cannot see, so it is supplied as an
    // environment variable and tracked as one. Nothing else makes the date
    // reach the binary without recompiling the crate on every invocation.
    println!("cargo:rerun-if-env-changed=CA_BUILD_DATE");
    println!("cargo:rerun-if-changed=../../BUILD_NUMBER");
    println!("cargo:rustc-env=CA_VERSION={}", version::compute());
}

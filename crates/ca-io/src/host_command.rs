//! Commands for programs of the host, with the environment a host program expects.
//!
//! A self-contained Linux image (an AppImage deployed with sharun) runs this
//! process on a bundled C library, loader and data, and the image's runtime
//! points loader, preload and resource variables (`GCONV_PATH`,
//! `LIBGL_DRIVERS_PATH`, `TERMINFO`, `PATH`, `XDG_DATA_DIRS`, ...) into the
//! image. A host program that inherits them loads the image's modules against
//! the host C library and fails, or reads image files that stop existing when
//! the image unmounts while the program still runs. [`host_command`] builds a
//! [`Command`] whose environment has those variables removed or cleaned, and
//! leaves every other variable exactly as it is.
//!
//! The image is recognised by the variables its launchers set: `SHARUN_DIR`
//! (sharun, every entry point) and `APPDIR` together with `APPIMAGE` or
//! `SHARUN_DIR` (the AppImage runtime). Without them, and always on Windows and
//! macOS, the environment passes through unchanged.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::process::Command;

/// An environment, by variable name.
pub type Environment = BTreeMap<OsString, OsString>;

/// Bookkeeping of the AppImage runtime and of sharun. A host program has no use
/// for them, and another AppImage started with them takes this image for its own.
const LAUNCHER_VARIABLES: &[&str] = &[
    "APPDIR",
    "APPIMAGE",
    "APPIMAGE_ARCH",
    "APPIMAGE_UID",
    "ARGV0",
    "OWD",
    "SHARUN_DIR",
    "CROSS_LIBC_DLOPEN_ROOT",
    "HOSTPATH",
    "HOST_HOME",
    "HOST_KERNEL_VERSION",
    "HOST_XDG_CACHE_HOME",
    "HOST_XDG_CONFIG_HOME",
    "HOST_XDG_DATA_HOME",
    "HOST_XDG_STATE_HOME",
    "REAL_HOME",
    "REAL_XDG_CACHE_HOME",
    "REAL_XDG_CONFIG_HOME",
    "REAL_XDG_DATA_HOME",
    "REAL_XDG_STATE_HOME",
];

/// Separators of the loader's library path; glibc reads both.
const LIBRARY_PATH_SEPARATORS: &[u8] = b":;";

/// Separators of the loader's preload list; glibc reads both.
const PRELOAD_SEPARATORS: &[u8] = b": ";

/// Separator of every other list variable.
const LIST_SEPARATORS: &[u8] = b":";

/// Preload libraries of sharun that a preload list can name without a folder.
/// The host loader cannot find them and reports each one at every start.
const IMAGE_PRELOADS: &[&str] = &[
    "anylinux.so",
    "cross-libc-dlopen.so",
    "glycin-fix.so",
    "gtk-fix-nonsense.so",
    "path-mapping.so",
];

/// Resource overrides that sharun and its preload set for the bundled
/// libraries. Each one is removed whole when any of its entries lies inside the
/// image, because the host entries sharun adds beside the image entry are
/// defaults the user did not set. A value that names no image path is the
/// user's own and is kept.
const RESOURCE_VARIABLES: &[&str] = &[
    "ALSA_CONFIG_PATH",
    "AMDGPU_ASIC_ID_TABLE_PATHS",
    "BABL_PATH",
    "FOLKS_BACKEND_PATH",
    "FREI0R_PATH",
    "GBM_BACKENDS_PATH",
    "GCONV_PATH",
    "GDK_PIXBUF_MODULEDIR",
    "GDK_PIXBUF_MODULE_FILE",
    "GEGL_PATH",
    "GIO_MODULE_DIR",
    "GI_TYPELIB_PATH",
    "GSETTINGS_SCHEMA_DIR",
    "GS_LIB",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_SCANNER",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GTK_PATH",
    "IMLIB2_FILTER_PATH",
    "IMLIB2_LOADER_PATH",
    "JACK_DRIVER_DIR",
    "LADSPA_PATH",
    "LIBDECOR_PLUGIN_DIR",
    "LIBGL_DRIVERS_PATH",
    "LIBHEIF_PLUGIN_PATH",
    "LIBVA_DRIVERS_PATH",
    "LOCPATH",
    "MAGIC",
    "MAGICK_CODER_FILTER_PATH",
    "MAGICK_CODER_MODULE_PATH",
    "MAGICK_CONFIGURE_PATH",
    "MAGICK_HOME",
    "MLT_PRESETS_PATH",
    "MLT_PROFILES_PATH",
    "MLT_REPOSITORY",
    "OPENSSL_CONF",
    "PATH_MAPPING",
    "PATH_MAPPING_EXCLUDE",
    "PEAS_PLUGIN_LOADERS_DIR",
    "PERLLIB",
    "PIPEWIRE_CONFIG_DIR",
    "PIPEWIRE_MODULE_DIR",
    "PYTHONHOME",
    "QT_PLUGIN_PATH",
    "QT_XKB_CONFIG_ROOT",
    "SPA_PLUGIN_DIR",
    "TCL_LIBRARY",
    "TERMINFO",
    "TERMINFO_DIRS",
    "TEXTDOMAINDIR",
    "TK_LIBRARY",
    "VK_DRIVER_FILES",
    "WEBKIT_EXEC_PATH",
    "WEBKIT_INJECTED_BUNDLE_PATH",
    "XKB_CONFIG_ROOT",
    "XTABLES_LIBDIR",
    "__EGL_VENDOR_LIBRARY_DIRS",
    "__EGL_VENDOR_LIBRARY_FILENAMES",
];

/// Folders the image's launchers redirect for the image's own data, with the
/// variable that keeps the user's value while the redirection is in force.
const SAVED_ORIGINALS: &[(&str, &str)] = &[
    ("HOME", "REAL_HOME"),
    ("XDG_CONFIG_HOME", "REAL_XDG_CONFIG_HOME"),
    ("XDG_DATA_HOME", "REAL_XDG_DATA_HOME"),
    ("XDG_CACHE_HOME", "REAL_XDG_CACHE_HOME"),
    ("XDG_STATE_HOME", "REAL_XDG_STATE_HOME"),
];

/// The image this process runs from, as its environment describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The folders the image is mounted or extracted at, without a trailing
    /// separator.
    roots: Vec<Vec<u8>>,
    /// The user's home folder from the account database, used when portable
    /// mode replaced `HOME` and no launcher kept the original.
    account_home: Option<OsString>,
}

impl Image {
    /// The image `environment` names, or `None` when it names none.
    #[must_use]
    pub fn detect(environment: &Environment) -> Option<Self> {
        let sharun = value(environment, "SHARUN_DIR").and_then(root_of);
        let appdir = value(environment, "APPDIR").and_then(root_of);
        let from_runtime = sharun.is_some() || value(environment, "APPIMAGE").is_some();
        let mut roots = Vec::new();
        roots.extend(sharun);
        if from_runtime {
            roots.extend(appdir);
        }
        roots.dedup();
        if roots.is_empty() {
            return None;
        }
        Some(Self {
            roots,
            account_home: None,
        })
    }

    /// The same image with the account database's home folder known.
    #[must_use]
    pub fn with_account_home(mut self, home: Option<OsString>) -> Self {
        self.account_home = home;
        self
    }

    /// Whether portable mode replaced `HOME` with no launcher keeping the
    /// original, so only the account database can tell the user's home.
    #[must_use]
    pub fn needs_account_home(&self, environment: &Environment) -> bool {
        value(environment, "REAL_HOME").is_none() && portable_folder(environment, "HOME", ".home")
    }

    fn contains(&self, entry: &[u8]) -> bool {
        self.roots.iter().any(|root| {
            entry.starts_with(root)
                && entry
                    .get(root.len())
                    .is_none_or(|separator| *separator == b'/')
        })
    }
}

/// The environment a host program started from `image` gets.
///
/// - The launcher bookkeeping is removed.
/// - The loader's library path (entries split at `:` or `;`) and preload list
///   (split at `:` or a space) lose the entries inside the image and the
///   sharun preload libraries named without a folder; the user's entries stay.
/// - A resource override that names an image path is removed whole.
/// - A folder the launchers redirected for the image's own data is set back to
///   the value they kept. Portable mode (`<image>.home`, `<image>.config`
///   beside the image file) holds the image application's data only, so a host
///   program gets the account's home folder and the default configuration
///   folder; with no account home known, `HOME` stays as it is.
/// - In every other variable, each `:`-separated entry that lies inside the
///   image is dropped, the other entries keep their order, and a variable left
///   with no entry is removed. A variable that names no image path is kept
///   byte for byte.
#[must_use]
pub fn clean_environment(environment: &Environment, image: &Image) -> Environment {
    let mut cleaned = Environment::new();
    for (name, value) in environment {
        let Some(name_text) = name.to_str() else {
            insert_filtered(&mut cleaned, image, name, value);
            continue;
        };
        if LAUNCHER_VARIABLES.contains(&name_text) {
            continue;
        }
        if name_text == "LD_LIBRARY_PATH" {
            insert_entries(
                &mut cleaned,
                name,
                value,
                LIBRARY_PATH_SEPARATORS,
                |entry| image.contains(entry),
            );
            continue;
        }
        if name_text == "LD_PRELOAD" {
            insert_entries(&mut cleaned, name, value, PRELOAD_SEPARATORS, |entry| {
                image.contains(entry)
                    || std::str::from_utf8(entry).is_ok_and(|text| IMAGE_PRELOADS.contains(&text))
            });
            continue;
        }
        if RESOURCE_VARIABLES.contains(&name_text) && names_image(image, value) {
            continue;
        }
        insert_filtered(&mut cleaned, image, name, value);
    }
    for (name, saved) in SAVED_ORIGINALS {
        if let Some(original) = value(environment, saved) {
            cleaned.insert((*name).into(), original.to_owned());
        }
    }
    if value(environment, "REAL_HOME").is_none() && portable_folder(environment, "HOME", ".home") {
        if let Some(home) = &image.account_home {
            cleaned.insert("HOME".into(), home.clone());
        }
    }
    if value(environment, "REAL_XDG_CONFIG_HOME").is_none()
        && portable_folder(environment, "XDG_CONFIG_HOME", ".config")
    {
        cleaned.remove(OsStr::new("XDG_CONFIG_HOME"));
    }
    cleaned
}

fn names_image(image: &Image, value: &OsStr) -> bool {
    value
        .as_encoded_bytes()
        .split(|byte| *byte == b':')
        .any(|entry| image.contains(entry))
}

/// Insert `value` without its image entries, or nothing when only image
/// entries remain.
fn insert_filtered(cleaned: &mut Environment, image: &Image, name: &OsStr, value: &OsStr) {
    insert_entries(cleaned, name, value, LIST_SEPARATORS, |entry| {
        image.contains(entry)
    });
}

/// Insert `value` without the entries `drop` selects. A kept entry keeps the
/// separator that preceded it, and the variable is left out when no non-empty
/// entry remains, because an empty `PATH` entry searches the working folder.
fn insert_entries(
    cleaned: &mut Environment,
    name: &OsStr,
    value: &OsStr,
    separators: &[u8],
    drop: impl Fn(&[u8]) -> bool,
) {
    let bytes = value.as_encoded_bytes();
    let mut entries: Vec<(Option<u8>, &[u8])> = Vec::new();
    let mut start = 0;
    let mut separator = None;
    for (index, byte) in bytes.iter().enumerate() {
        if separators.contains(byte) {
            entries.push((separator, bytes.get(start..index).unwrap_or_default()));
            separator = Some(*byte);
            start = index + 1;
        }
    }
    entries.push((separator, bytes.get(start..).unwrap_or_default()));
    if !entries.iter().any(|(_, entry)| drop(entry)) {
        cleaned.insert(name.to_owned(), value.to_owned());
        return;
    }
    entries.retain(|(_, entry)| !drop(entry));
    if entries.iter().all(|(_, entry)| entry.is_empty()) {
        return;
    }
    let mut joined = Vec::with_capacity(bytes.len());
    for (index, (separator, entry)) in entries.iter().enumerate() {
        if index > 0 {
            joined.extend(separator.or_else(|| separators.first().copied()));
        }
        joined.extend_from_slice(entry);
    }
    if let Some(joined) = from_bytes(&joined) {
        cleaned.insert(name.to_owned(), joined);
    }
}

/// Bytes split from an [`OsStr`] at an ASCII separator, back as an
/// [`OsString`].
#[cfg(unix)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature matches the variant for platforms where the rebuild can fail"
)]
fn from_bytes(bytes: &[u8]) -> Option<OsString> {
    use std::os::unix::ffi::OsStrExt;
    Some(OsStr::from_bytes(bytes).to_owned())
}

/// Bytes split from an [`OsStr`] at an ASCII separator, back as an
/// [`OsString`]; a value that is not text is dropped, since the platform
/// offers no safe rebuild of its encoding.
#[cfg(not(unix))]
fn from_bytes(bytes: &[u8]) -> Option<OsString> {
    std::str::from_utf8(bytes).ok().map(OsString::from)
}

/// A [`Command`] for `program`, a program of the host, with the environment
/// [`host_environment`] gives.
///
/// Every program this workspace starts that is not part of the image goes
/// through this function. Off the image the command is the plain
/// [`Command::new`].
pub fn host_command(program: impl AsRef<OsStr>) -> Command {
    #[allow(
        clippy::disallowed_methods,
        reason = "the one place a host command is created"
    )]
    let mut command = Command::new(program);
    if let Some(environment) = host_environment() {
        command.env_clear().envs(environment);
    }
    command
}

/// The environment a host program started now gets, or `None` when it
/// inherits this process's environment unchanged.
///
/// On Linux inside an image this reads the process environment and, in
/// portable mode, the account database, so it belongs where the program is
/// started, never on the frame thread.
#[must_use]
pub fn host_environment() -> Option<Environment> {
    if cfg!(any(windows, target_os = "macos")) {
        return None;
    }
    let current: Environment = std::env::vars_os().collect();
    let mut image = Image::detect(&current)?;
    if image.needs_account_home(&current) {
        image = image.with_account_home(account_home());
    }
    Some(clean_environment(&current, &image))
}

/// The home folder the local account database gives the real user.
#[cfg(unix)]
fn account_home() -> Option<OsString> {
    let database = std::fs::read_to_string("/etc/passwd").ok()?;
    home_of(&database, rustix::process::getuid().as_raw())
}

#[cfg(not(unix))]
fn account_home() -> Option<OsString> {
    None
}

/// The value of `name` that a host program started now sees.
#[must_use]
pub fn host_variable(name: &str) -> Option<OsString> {
    match host_environment() {
        Some(environment) => environment.get(OsStr::new(name)).cloned(),
        None => std::env::var_os(name),
    }
}

fn value<'a>(environment: &'a Environment, name: &str) -> Option<&'a OsStr> {
    environment
        .get(OsStr::new(name))
        .map(OsString::as_os_str)
        .filter(|value| !value.is_empty())
}

/// A launcher folder as a root, or `None` for a relative path or the file
/// system root, which would place every host path inside the image.
fn root_of(value: &OsStr) -> Option<Vec<u8>> {
    let bytes = value.as_encoded_bytes();
    if bytes.first() != Some(&b'/') {
        return None;
    }
    let trimmed = trim_separators(bytes);
    (!trimmed.is_empty()).then(|| trimmed.to_vec())
}

fn trim_separators(mut bytes: &[u8]) -> &[u8] {
    while let Some(rest) = bytes.strip_suffix(b"/") {
        bytes = rest;
    }
    bytes
}

/// Whether `name` holds the portable folder `<APPIMAGE><suffix>`.
fn portable_folder(environment: &Environment, name: &str, suffix: &str) -> bool {
    let (Some(image_file), Some(folder)) =
        (value(environment, "APPIMAGE"), value(environment, name))
    else {
        return false;
    };
    let mut expected = image_file.as_encoded_bytes().to_vec();
    expected.extend_from_slice(suffix.as_bytes());
    trim_separators(folder.as_encoded_bytes()) == expected.as_slice()
}

/// The home folder of `uid` in a password database (`name:password:uid:gid:
/// comment:home:shell` per line).
#[cfg_attr(not(unix), allow(dead_code))]
fn home_of(database: &str, uid: u32) -> Option<OsString> {
    database.lines().find_map(|line| {
        let mut fields = line.split(':');
        let id = fields.nth(2)?.parse::<u32>().ok()?;
        let home = fields.nth(2)?;
        (id == uid && home.starts_with('/')).then(|| OsString::from(home))
    })
}

#[cfg(test)]
mod tests;

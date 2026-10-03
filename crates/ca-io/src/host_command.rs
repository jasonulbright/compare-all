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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The folders a program search walks when `PATH` is unset: the value glibc's
/// `execvp` takes from `confstr(_CS_PATH)`. A host program whose cleaned
/// environment has no `PATH` is searched for here.
pub const DEFAULT_SEARCH_PATH: &str = "/bin:/usr/bin";

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
/// libraries. They hold one path or a `:`-separated list of paths whatever
/// their entries look like, and lose their image entries like `PATH`.
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

/// Variables that hold `:`-separated paths whatever their entries look like.
const PATH_LIST_VARIABLES: &[&str] = &["PATH", "XDG_CONFIG_DIRS", "XDG_DATA_DIRS"];

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
    /// Whether the AppImage runtime started this image, so `APPIMAGE` and the
    /// portable folders beside it belong to it.
    runtime: bool,
    /// The user's home folder from the account database, used when portable
    /// mode replaced `HOME` and no launcher kept the original.
    account_home: Option<OsString>,
}

impl Image {
    /// The image `environment` names that `executable`, this process's own
    /// program, runs from, or `None` when there is none.
    ///
    /// A root counts only when `executable` lies inside it, so a program that
    /// inherited the variables of another image (a terminal started from an
    /// AppImage) does not take that image for its own. With `executable`
    /// unknown every named root counts: a host program given the image's
    /// variables fails, while a variable of another image that is cleaned in
    /// error names paths that are not this process's anyway.
    ///
    /// `resolve` gives the canonical form of a folder, or `None`. Each folder
    /// matches in the form the variable gives and in its canonical form, so a
    /// folder named through a link or relative to the working folder matches
    /// the resolved paths that the executable and the variables hold.
    #[must_use]
    pub fn detect(
        environment: &Environment,
        executable: Option<&Path>,
        resolve: &dyn Fn(&Path) -> Option<PathBuf>,
    ) -> Option<Self> {
        let forms = |name: &str| -> Vec<Vec<u8>> {
            let Some(given) = value(environment, name) else {
                return Vec::new();
            };
            let resolved = resolve(Path::new(given));
            let mut forms: Vec<Vec<u8>> = [Some(given), resolved.as_deref().map(Path::as_os_str)]
                .into_iter()
                .flatten()
                .filter_map(root_of)
                .collect();
            forms.dedup();
            let runs_from = executable.is_none_or(|program| {
                let program = program.as_os_str().as_encoded_bytes();
                forms.iter().any(|root| inside(root, program))
            });
            if runs_from {
                forms
            } else {
                Vec::new()
            }
        };
        let from_runtime =
            value(environment, "SHARUN_DIR").is_some() || value(environment, "APPIMAGE").is_some();
        let sharun = forms("SHARUN_DIR");
        let appdir = if from_runtime {
            forms("APPDIR")
        } else {
            Vec::new()
        };
        let runtime = !appdir.is_empty() && value(environment, "APPIMAGE").is_some();
        let mut roots = sharun;
        for root in appdir {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        if roots.is_empty() {
            return None;
        }
        Some(Self {
            roots,
            runtime,
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
        self.runtime
            && value(environment, "REAL_HOME").is_none()
            && portable_folder(environment, "HOME", ".home")
    }

    fn contains(&self, entry: &[u8]) -> bool {
        self.roots.iter().any(|root| inside(root, entry))
    }
}

/// Whether `path` is `root` or lies below it.
fn inside(root: &[u8], path: &[u8]) -> bool {
    path.starts_with(root)
        && path
            .get(root.len())
            .is_none_or(|separator| *separator == b'/')
}

/// The environment a host program started from `image` gets.
///
/// - The launcher bookkeeping is removed.
/// - The loader's library path (entries split at `:` or `;`) and preload list
///   (split at `:` or a space) lose the entries inside the image and the
///   sharun preload libraries named without a folder; the user's entries stay.
/// - A folder the launchers redirected for the image's own data is set back to
///   the value they kept. Portable mode (`<image>.home`, `<image>.config`
///   beside the image file) holds the image application's data only, so a host
///   program gets the account's home folder and the default configuration
///   folder; with no account home known, `HOME` stays as it is.
/// - In `PATH`, `XDG_DATA_DIRS`, `XDG_CONFIG_DIRS`, the resource overrides
///   (`GCONV_PATH`, `LIBGL_DRIVERS_PATH`, `TERMINFO`, ...) and in every value
///   whose non-empty `:`-separated entries are all absolute paths, each entry
///   inside the image is dropped, the other entries keep their order, and a
///   variable left with no non-empty entry is removed. Any other value is kept
///   whole, also when an image path occurs inside it. A variable that names no
///   image path is kept byte for byte.
#[must_use]
pub fn clean_environment(environment: &Environment, image: &Image) -> Environment {
    let mut cleaned = Environment::new();
    for (name, value) in environment {
        let Some(name_text) = name.to_str() else {
            insert_other(&mut cleaned, image, name, value);
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
        if PATH_LIST_VARIABLES.contains(&name_text) || RESOURCE_VARIABLES.contains(&name_text) {
            insert_filtered(&mut cleaned, image, name, value);
            continue;
        }
        insert_other(&mut cleaned, image, name, value);
    }
    for (name, saved) in SAVED_ORIGINALS {
        if let Some(original) = value(environment, saved) {
            cleaned.insert((*name).into(), original.to_owned());
        }
    }
    if image.needs_account_home(environment) {
        if let Some(home) = &image.account_home {
            cleaned.insert("HOME".into(), home.clone());
        }
    }
    if image.runtime
        && value(environment, "REAL_XDG_CONFIG_HOME").is_none()
        && portable_folder(environment, "XDG_CONFIG_HOME", ".config")
    {
        cleaned.remove(OsStr::new("XDG_CONFIG_HOME"));
    }
    cleaned
}

/// Insert `value` without its image entries, or nothing when only image
/// entries remain.
fn insert_filtered(cleaned: &mut Environment, image: &Image, name: &OsStr, value: &OsStr) {
    insert_entries(cleaned, name, value, LIST_SEPARATORS, |entry| {
        image.contains(entry)
    });
}

/// Insert a variable of no known kind: a list of absolute paths loses its image
/// entries; any other value is kept whole, since a value such as
/// `file:<path>` or `ssh -i <path>` cannot be repaired by dropping a part.
fn insert_other(cleaned: &mut Environment, image: &Image, name: &OsStr, value: &OsStr) {
    let is_path_list = value
        .as_encoded_bytes()
        .split(|byte| *byte == b':')
        .all(|entry| entry.is_empty() || entry.first() == Some(&b'/'));
    if is_path_list {
        insert_filtered(cleaned, image, name, value);
    } else {
        cleaned.insert(name.to_owned(), value.to_owned());
    }
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
    static IMAGE: OnceLock<Option<Image>> = OnceLock::new();
    if cfg!(any(windows, target_os = "macos")) {
        return None;
    }
    let current: Environment = std::env::vars_os().collect();
    let mut image = IMAGE
        .get_or_init(|| {
            Image::detect(&current, executable(), &|folder| {
                std::fs::canonicalize(folder).ok()
            })
        })
        .clone()?;
    if image.needs_account_home(&current) {
        image = image.with_account_home(account_home());
    }
    Some(clean_environment(&current, &image))
}

/// This process's own program as the kernel names it (`/proc/self/exe` on
/// Linux), read once; `None` when it cannot be read.
fn executable() -> Option<&'static Path> {
    static EXECUTABLE: OnceLock<Option<PathBuf>> = OnceLock::new();
    EXECUTABLE
        .get_or_init(|| std::env::current_exe().ok())
        .as_deref()
}

/// The home folder the local account database gives the real user, read at
/// most once per process.
fn account_home() -> Option<OsString> {
    static HOME: OnceLock<Option<OsString>> = OnceLock::new();
    HOME.get_or_init(read_account_home).clone()
}

/// How many times the password database was read.
#[cfg(test)]
static ACCOUNT_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(unix)]
fn read_account_home() -> Option<OsString> {
    use std::io::Read;
    #[cfg(test)]
    ACCOUNT_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut database = Vec::new();
    std::fs::File::open("/etc/passwd")
        .ok()?
        .take(MAX_PASSWORD_BYTES)
        .read_to_end(&mut database)
        .ok()?;
    home_of(&database, rustix::process::getuid().as_raw())
}

/// Most bytes read from the password database.
#[cfg(unix)]
const MAX_PASSWORD_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(not(unix))]
fn read_account_home() -> Option<OsString> {
    #[cfg(test)]
    ACCOUNT_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
/// The fields are bytes: a comment or a name in a legacy encoding is not text.
#[cfg_attr(not(unix), allow(dead_code))]
fn home_of(database: &[u8], uid: u32) -> Option<OsString> {
    database.split(|byte| *byte == b'\n').find_map(|line| {
        let mut fields = line.split(|byte| *byte == b':');
        let id = std::str::from_utf8(fields.nth(2)?)
            .ok()?
            .parse::<u32>()
            .ok()?;
        let home = fields.nth(2)?;
        if id != uid || home.first() != Some(&b'/') {
            return None;
        }
        from_bytes(home.strip_suffix(b"\r").unwrap_or(home))
    })
}

#[cfg(test)]
mod tests;

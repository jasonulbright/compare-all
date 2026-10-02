//! Locators naming one side of a session.
//!
//! A side is not always a path on the local file system: it can be a folder
//! inside an archive, a recorded snapshot of a folder tree, the clipboard, a
//! live registry hive, or a folder reached through a named remote profile.
//!
//! Two forms exist and they are not the same thing. The textual spec parsed by
//! [`FromStr`] and rendered by [`fmt::Display`] is for people: what a user
//! types into a side box and what the command line accepts. The stored form is
//! tagged JSON, so reading a document never depends on re-parsing a string and
//! a path containing the container separator cannot be split in the wrong
//! place.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Prefix marking a spec that names a saved folder-tree snapshot.
pub const SNAPSHOT_PREFIX: &str = "snapshot:";
/// Prefix marking a spec that names a path inside an archive file.
pub const ARCHIVE_PREFIX: &str = "archive:";
/// Prefix marking a spec that names a saved remote profile.
pub const PROFILE_PREFIX: &str = "profile:";
/// Spec naming the clipboard as a comparison side.
pub const CLIPBOARD_SPEC: &str = "clipboard://";
/// Prefix marking a spec that names a registry hive and key.
pub const REGISTRY_PREFIX: &str = "reg:\\\\";
/// Separator between a container and a path inside it.
pub const CONTAINER_SEPARATOR: char = '?';
/// Prefix that turns off path parsing in the Windows file APIs. It contains the
/// container separator, so it is skipped when a container spec is split.
pub const EXTENDED_LENGTH_PREFIX: &str = r"\\?\";
/// Text rendered in place of a password.
pub const REDACTED: &str = "***";

/// Encoding tag of a stored path whose text is not valid UTF-8 on the platform
/// that wrote it.
const WIDE_ENCODING: &str = "windows-utf16";
/// Encoding tag of a stored path made of raw bytes.
const BYTE_ENCODING: &str = "unix-bytes";

/// A file system path in a form that survives storage on every platform.
///
/// A path whose text is valid UTF-8 is stored as a plain string. A path that is
/// not — unpaired surrogates on Windows, non-UTF-8 bytes elsewhere — is stored
/// as a tagged object listing the platform's own units, so saving never fails
/// and nothing is replaced by a substitution character. An object written by
/// another platform is kept verbatim and written back unchanged.
#[derive(Clone)]
pub struct StoredPath {
    path: PathBuf,
    foreign: Option<Value>,
}

impl StoredPath {
    /// The path as this platform sees it.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// The path, consuming the locator.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.path
    }

    /// True when the stored form came from another platform and is carried
    /// through unchanged rather than re-encoded.
    #[must_use]
    pub fn is_foreign(&self) -> bool {
        self.foreign.is_some()
    }
}

impl<T: Into<PathBuf>> From<T> for StoredPath {
    fn from(value: T) -> Self {
        Self {
            path: value.into(),
            foreign: None,
        }
    }
}

impl Deref for StoredPath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl PartialEq for StoredPath {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.foreign == other.foreign
    }
}

impl fmt::Debug for StoredPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.path, f)
    }
}

impl fmt::Display for StoredPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())
    }
}

#[cfg(windows)]
fn encode_units(path: &Path) -> Value {
    use std::os::windows::ffi::OsStrExt;
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    serde_json::json!({ "encoding": WIDE_ENCODING, "units": units })
}

#[cfg(not(windows))]
fn encode_units(path: &Path) -> Value {
    use std::os::unix::ffi::OsStrExt;
    let bytes: Vec<u8> = path.as_os_str().as_bytes().to_vec();
    serde_json::json!({ "encoding": BYTE_ENCODING, "bytes": bytes })
}

#[cfg(windows)]
fn decode_units(value: &Value) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let units: Vec<u16> = value
        .get("units")?
        .as_array()?
        .iter()
        .map(|unit| u16::try_from(unit.as_u64()?).ok())
        .collect::<Option<_>>()?;
    Some(PathBuf::from(std::ffi::OsString::from_wide(&units)))
}

#[cfg(not(windows))]
fn decode_units(value: &Value) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let bytes: Vec<u8> = value
        .get("bytes")?
        .as_array()?
        .iter()
        .map(|byte| u8::try_from(byte.as_u64()?).ok())
        .collect::<Option<_>>()?;
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Best effort rendering of a foreign encoding, used only so the path has a
/// usable value in this process; the original object is stored alongside and
/// written back in place of anything derived here.
fn decode_foreign(value: &Value) -> PathBuf {
    if let Some(units) = value.get("units").and_then(Value::as_array) {
        let units: Vec<u16> = units
            .iter()
            .filter_map(|unit| u16::try_from(unit.as_u64()?).ok())
            .collect();
        return PathBuf::from(String::from_utf16_lossy(&units));
    }
    if let Some(bytes) = value.get("bytes").and_then(Value::as_array) {
        let bytes: Vec<u8> = bytes
            .iter()
            .filter_map(|byte| u8::try_from(byte.as_u64()?).ok())
            .collect();
        return PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
    }
    PathBuf::new()
}

fn native_encoding() -> &'static str {
    if cfg!(windows) {
        WIDE_ENCODING
    } else {
        BYTE_ENCODING
    }
}

impl Serialize for StoredPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(foreign) = &self.foreign {
            return foreign.serialize(serializer);
        }
        match self.path.to_str() {
            Some(text) => serializer.serialize_str(text),
            None => encode_units(&self.path).serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for StoredPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if let Some(text) = value.as_str() {
            return Ok(StoredPath {
                path: PathBuf::from(text),
                foreign: None,
            });
        }
        let encoding = value
            .get("encoding")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                serde::de::Error::custom("path is neither text nor an encoded object")
            })?;
        if encoding == native_encoding() {
            if let Some(path) = decode_units(&value) {
                return Ok(StoredPath {
                    path,
                    foreign: None,
                });
            }
        }
        Ok(StoredPath {
            path: decode_foreign(&value),
            foreign: Some(value),
        })
    }
}

/// A password held for the lifetime of the process only.
///
/// The type carries no serde implementation and every field holding one is
/// marked `skip`, so no stored document can contain a password whatever the
/// `DisableSavedPasswords` policy is set to. Both renderings replace the value
/// with [`REDACTED`], so a password cannot reach a log or an error message.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Password(String);

impl Password {
    /// Wraps a password.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Password(value.into())
    }

    /// The password itself, for handing to a connection.
    #[must_use]
    pub fn reveal(&self) -> &str {
        &self.0
    }

    /// True when the password is the empty string.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// A folder reached through a named remote profile.
///
/// Credentials belong to the stored profile, not to the side, so nothing here
/// carries one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProfileLocation {
    /// Name of the stored profile supplying host and credentials.
    #[serde(default)]
    pub profile: String,
    /// Path below the profile root, empty for the profile root itself.
    #[serde(default)]
    pub path: String,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, Value>,
}

/// One side of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[non_exhaustive]
pub enum SideLocation {
    /// A path on a mounted file system, including UNC server paths.
    Local {
        /// The path as typed; expansion of `~` happens in the file layer.
        path: StoredPath,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// A path inside an archive file.
    Archive {
        /// Location of the archive file itself.
        archive: StoredPath,
        /// Path inside the archive, empty for the archive root.
        #[serde(default)]
        inner: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// A recorded state of a folder tree held in a snapshot file.
    Snapshot {
        /// Location of the snapshot file.
        path: StoredPath,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// The clipboard.
    ///
    /// A tagged variant with no data of its own still serializes as an object,
    /// so it carries the flattened `unknown` map: without one, a field a newer
    /// build writes beside the tag is dropped on load.
    Clipboard {
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// A registry hive and key, optionally on a named machine.
    Registry {
        /// Hive and key path, including any leading machine name.
        key: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// A folder reached through a named remote profile.
    Profile(ProfileLocation),
    /// A remote location addressed by URL, such as `ftp` or `sftp`.
    Remote {
        /// URL scheme, lower case and without the separator.
        scheme: String,
        /// User name taken from the URL, without any password.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        /// Password taken from the URL. Never serialized.
        #[serde(skip)]
        password: Option<Password>,
        /// Host, port and path: everything after the userinfo.
        rest: String,
        /// Fields written by another build, preserved verbatim.
        #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, Value>,
    },
    /// A side this build does not understand, carried through unchanged.
    #[serde(untagged)]
    Unknown(Value),
}

impl SideLocation {
    /// Builds a local file system side.
    pub fn local(path: impl Into<PathBuf>) -> Self {
        SideLocation::Local {
            path: StoredPath::from(path.into()),
            unknown: BTreeMap::new(),
        }
    }

    /// Builds an archive-internal side.
    pub fn archive(archive: impl Into<PathBuf>, inner: impl Into<String>) -> Self {
        SideLocation::Archive {
            archive: StoredPath::from(archive.into()),
            inner: inner.into(),
            unknown: BTreeMap::new(),
        }
    }

    /// Builds a snapshot side.
    pub fn snapshot(path: impl Into<PathBuf>) -> Self {
        SideLocation::Snapshot {
            path: StoredPath::from(path.into()),
            unknown: BTreeMap::new(),
        }
    }

    /// Builds a clipboard side.
    #[must_use]
    pub fn clipboard() -> Self {
        SideLocation::Clipboard {
            unknown: BTreeMap::new(),
        }
    }

    /// Builds a registry side.
    pub fn registry(key: impl Into<String>) -> Self {
        SideLocation::Registry {
            key: key.into(),
            unknown: BTreeMap::new(),
        }
    }

    /// Builds a profile side.
    pub fn profile(profile: impl Into<String>, path: impl Into<String>) -> Self {
        SideLocation::Profile(ProfileLocation {
            profile: profile.into(),
            path: path.into(),
            unknown: BTreeMap::new(),
        })
    }

    /// Builds a remote side with no credentials.
    pub fn remote(scheme: impl Into<String>, rest: impl Into<String>) -> Self {
        SideLocation::Remote {
            scheme: scheme.into(),
            user: None,
            password: None,
            rest: rest.into(),
            unknown: BTreeMap::new(),
        }
    }

    /// True when reading the side needs a network connection or credentials.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self, SideLocation::Profile(_) | SideLocation::Remote { .. })
    }

    /// The password parsed out of a remote URL, if one was given.
    #[must_use]
    pub fn password(&self) -> Option<&Password> {
        match self {
            SideLocation::Remote { password, .. } => password.as_ref(),
            _ => None,
        }
    }

    /// The same side with any in-memory password dropped.
    #[must_use]
    pub fn without_password(mut self) -> Self {
        if let SideLocation::Remote { password, .. } = &mut self {
            *password = None;
        }
        self
    }

    /// The spec text, identical to the [`fmt::Display`] rendering. A password
    /// is replaced by [`REDACTED`], so a spec carrying one does not round trip.
    #[must_use]
    pub fn to_spec(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for SideLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SideLocation::Local { path, .. } => write!(f, "{path}"),
            SideLocation::Archive { archive, inner, .. } => {
                write!(f, "{ARCHIVE_PREFIX}{archive}")?;
                if !inner.is_empty() {
                    write!(f, "{CONTAINER_SEPARATOR}{inner}")?;
                }
                Ok(())
            }
            SideLocation::Snapshot { path, .. } => write!(f, "{SNAPSHOT_PREFIX}{path}"),
            SideLocation::Clipboard { .. } => f.write_str(CLIPBOARD_SPEC),
            SideLocation::Registry { key, .. } => write!(f, "{REGISTRY_PREFIX}{key}"),
            SideLocation::Profile(profile) => {
                write!(f, "{PROFILE_PREFIX}{}", profile.profile)?;
                if !profile.path.is_empty() {
                    write!(f, "{CONTAINER_SEPARATOR}{}", profile.path)?;
                }
                Ok(())
            }
            SideLocation::Remote {
                scheme,
                user,
                password,
                rest,
                ..
            } => {
                write!(f, "{scheme}://")?;
                if let Some(user) = user {
                    f.write_str(user)?;
                    if password.is_some() {
                        write!(f, ":{REDACTED}")?;
                    }
                    f.write_str("@")?;
                }
                f.write_str(rest)
            }
            SideLocation::Unknown(value) => write!(f, "unknown:{value}"),
        }
    }
}

/// Failure to read a [`SideLocation`] from a spec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocationParseError {
    /// The spec was empty or only whitespace.
    #[error("empty location spec")]
    Empty,
    /// A prefixed spec carried no body after its prefix.
    #[error("missing {0} after prefix")]
    MissingBody(&'static str),
    /// An unescaped path separator appeared before a credential delimiter.
    #[error("a URL password containing '/' must encode it as %2F")]
    UnescapedUserInfoSeparator,
}

/// Splits a container spec into the container and the optional path inside it.
///
/// The Windows extended-length prefix itself contains the separator, so the
/// search starts after it.
fn split_container(body: &str) -> (&str, &str) {
    let start = if body.starts_with(EXTENDED_LENGTH_PREFIX) {
        EXTENDED_LENGTH_PREFIX.len()
    } else {
        0
    };
    match body[start..].find(CONTAINER_SEPARATOR) {
        Some(offset) => (&body[..start + offset], &body[start + offset + 1..]),
        None => (body, ""),
    }
}

/// A single character before a colon is a drive letter, not a URL scheme.
fn scheme_of(spec: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = spec.split_once("://")?;
    if scheme.len() < 2
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+')
    {
        return None;
    }
    Some((scheme, rest))
}

/// Splits `user:password@` off the front of a URL authority.
fn split_userinfo(
    rest: &str,
) -> Result<(Option<String>, Option<Password>, String), LocationParseError> {
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let Some(at) = rest[..authority_end].rfind('@') else {
        let authority = &rest[..authority_end];
        if authority.contains(':')
            && !authority_has_host_port_shape(authority)
            && rest[authority_end..].contains('@')
        {
            return Err(LocationParseError::UnescapedUserInfoSeparator);
        }
        return Ok((None, None, rest.to_owned()));
    };
    let (userinfo, tail) = rest.split_at(at);
    let tail = tail[1..].to_owned();
    Ok(match userinfo.split_once(':') {
        Some((user, password)) => (Some(user.to_owned()), Some(Password::new(password)), tail),
        None => (Some(userinfo.to_owned()), None, tail),
    })
}

/// Whether a colon in the authority has the shape of a host/port separator,
/// or belongs to a bracketed IPv6 host, rather than an unescaped password.
fn authority_has_host_port_shape(authority: &str) -> bool {
    fn decimal_port(value: &str) -> bool {
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
    }

    if let Some(bracketed) = authority.strip_prefix('[') {
        return bracketed.split_once(']').is_some_and(|(host, suffix)| {
            host.contains(':')
                && (suffix.is_empty() || suffix.strip_prefix(':').is_some_and(decimal_port))
        });
    }

    authority
        .rsplit_once(':')
        .is_some_and(|(host, port)| !host.is_empty() && decimal_port(port))
}

impl FromStr for SideLocation {
    type Err = LocationParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let spec = s.trim();
        if spec.is_empty() {
            return Err(LocationParseError::Empty);
        }
        if spec.eq_ignore_ascii_case(CLIPBOARD_SPEC) {
            return Ok(SideLocation::clipboard());
        }
        if let Some(body) = spec.strip_prefix(SNAPSHOT_PREFIX) {
            if body.is_empty() {
                return Err(LocationParseError::MissingBody("snapshot path"));
            }
            return Ok(SideLocation::snapshot(body));
        }
        if let Some(body) = spec.strip_prefix(ARCHIVE_PREFIX) {
            let (archive, inner) = split_container(body);
            if archive.is_empty() {
                return Err(LocationParseError::MissingBody("archive path"));
            }
            return Ok(SideLocation::archive(archive, inner));
        }
        if let Some(body) = spec.strip_prefix(PROFILE_PREFIX) {
            let (profile, path) = split_container(body);
            if profile.is_empty() {
                return Err(LocationParseError::MissingBody("profile name"));
            }
            return Ok(SideLocation::profile(profile, path));
        }
        if let Some(body) = spec.strip_prefix(REGISTRY_PREFIX) {
            if body.is_empty() {
                return Err(LocationParseError::MissingBody("registry key"));
            }
            return Ok(SideLocation::registry(body));
        }
        if let Some((scheme, rest)) = scheme_of(spec) {
            let (user, password, rest) = split_userinfo(rest)?;
            return Ok(SideLocation::Remote {
                scheme: scheme.to_ascii_lowercase(),
                user,
                password,
                rest,
                unknown: BTreeMap::new(),
            });
        }
        Ok(SideLocation::local(spec))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod tests {
    use super::*;

    const SPECS: &[&str] = &[
        r"C:\Projects\left",
        r"\\server\share\folder",
        "/home/user/left",
        "~/left",
        "archive:/tmp/bundle.zip",
        "archive:/tmp/bundle.zip?docs/readme.txt",
        r"archive:\\?\C:\very\long\bundle.zip",
        r"archive:\\?\C:\very\long\bundle.zip?docs/a.txt",
        "snapshot:/tmp/tree.snapshot",
        "clipboard://",
        r"reg:\\HKEY_LOCAL_MACHINE\Software",
        r"reg:\\MyComputer\HKEY_USERS\S-1-5",
        "profile:Nightly",
        "profile:Nightly?releases/current",
        "ftp://user@host/pub",
        "sftp://user@host//absolute",
        "smb://server/share",
        "mtp://Device/DCIM",
        "svn+ssh://host/trunk",
    ];

    #[test]
    fn every_spec_round_trips() {
        for spec in SPECS {
            let parsed = SideLocation::from_str(spec).unwrap();
            assert_eq!(&parsed.to_spec(), spec, "spec {spec} did not round trip");
            assert_eq!(SideLocation::from_str(&parsed.to_spec()).unwrap(), parsed);
        }
    }

    #[test]
    fn drive_letter_is_not_a_scheme() {
        let parsed = SideLocation::from_str(r"C:\Projects").unwrap();
        assert_eq!(parsed, SideLocation::local(r"C:\Projects"));
    }

    #[test]
    fn archive_inner_path_is_separated() {
        let parsed = SideLocation::from_str("archive:/tmp/b.zip?a/b.txt").unwrap();
        assert_eq!(parsed, SideLocation::archive("/tmp/b.zip", "a/b.txt"));
    }

    #[test]
    fn an_extended_length_archive_keeps_its_prefix() {
        let parsed =
            SideLocation::from_str(r"archive:\\?\C:\very\long\bundle.zip?docs/a.txt").unwrap();
        assert_eq!(
            parsed,
            SideLocation::archive(r"\\?\C:\very\long\bundle.zip", "docs/a.txt")
        );
    }

    #[test]
    fn profile_root_has_empty_path() {
        let parsed = SideLocation::from_str("profile:Nightly").unwrap();
        assert_eq!(parsed, SideLocation::profile("Nightly", ""));
        assert!(parsed.is_remote());
    }

    #[test]
    fn empty_and_headless_specs_are_rejected() {
        assert_eq!(
            SideLocation::from_str("   ").unwrap_err(),
            LocationParseError::Empty
        );
        assert!(SideLocation::from_str("profile:").is_err());
        assert!(SideLocation::from_str("snapshot:").is_err());
        assert!(SideLocation::from_str("archive:").is_err());
    }

    #[test]
    fn json_round_trips() {
        for spec in SPECS {
            let parsed = SideLocation::from_str(spec).unwrap();
            let json = serde_json::to_string(&parsed).unwrap();
            let back: SideLocation = serde_json::from_str(&json).unwrap();
            assert_eq!(back, parsed);
        }
    }

    #[test]
    fn a_password_reaches_neither_the_document_nor_any_rendering() {
        let parsed = SideLocation::from_str("ftp://alice:hunter2@example.com/pub").unwrap();
        assert_eq!(
            parsed.password().map(Password::reveal),
            Some("hunter2"),
            "the password is available in memory"
        );

        let json = serde_json::to_string(&parsed).unwrap();
        let debug = format!("{parsed:?}");
        let display = parsed.to_string();
        for rendering in [&json, &debug, &display] {
            assert!(
                !rendering.contains("hunter2"),
                "password leaked into {rendering}"
            );
        }
        assert_eq!(display, "ftp://alice:***@example.com/pub");
        assert!(json.contains("alice"), "the user name is kept: {json}");

        let back: SideLocation = serde_json::from_str(&json).unwrap();
        assert!(back.password().is_none(), "no stored document carries one");
    }

    #[test]
    fn an_unescaped_slash_in_a_password_is_refused_without_leaking_it() {
        let result = SideLocation::from_str("ftp://alice:pa/ss@files.example.test/pub");

        assert_eq!(result, Err(LocationParseError::UnescapedUserInfoSeparator));
        assert!(!format!("{result:?}").contains("pa/ss"));
    }

    #[test]
    fn an_at_sign_in_a_path_after_a_port_is_not_mistaken_for_a_password() {
        for (spec, rest) in [
            (
                "http://example.test:8443/repo/path@name",
                "example.test:8443/repo/path@name",
            ),
            (
                "http://[2001:db8::1]:8443/repo/path@name",
                "[2001:db8::1]:8443/repo/path@name",
            ),
        ] {
            let parsed = SideLocation::from_str(spec).unwrap();
            assert_eq!(parsed, SideLocation::remote("http", rest));
        }
    }

    #[test]
    fn a_remote_url_without_credentials_keeps_its_authority() {
        let parsed = SideLocation::from_str("sftp://host//absolute").unwrap();
        assert_eq!(parsed, SideLocation::remote("sftp", "host//absolute"));
    }

    #[test]
    fn dropping_the_password_leaves_the_rest_intact() {
        let parsed = SideLocation::from_str("ftp://alice:hunter2@example.com/pub").unwrap();
        let dropped = parsed.clone().without_password();
        assert!(dropped.password().is_none());
        assert_eq!(dropped.to_string(), "ftp://alice@example.com/pub");
    }

    #[test]
    fn an_unknown_side_survives_a_round_trip() {
        let json = r#"{"type":"cloud-bucket","bucket":"releases","region":"eu"}"#;
        let parsed: SideLocation = serde_json::from_str(json).unwrap();
        assert!(matches!(parsed, SideLocation::Unknown(_)));
        let written: Value =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(written, serde_json::from_str::<Value>(json).unwrap());
    }

    #[test]
    fn a_text_path_is_stored_as_text() {
        let parsed = SideLocation::local(r"C:\Projects\left");
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(json.contains(r#""path":"C:\\Projects\\left""#), "{json}");
    }

    #[test]
    fn a_path_that_is_not_valid_unicode_round_trips_losslessly() {
        let path = non_unicode_path();
        let side = SideLocation::local(path.clone());
        let json = serde_json::to_string(&side).unwrap();
        let back: SideLocation = serde_json::from_str(&json).unwrap();
        let SideLocation::Local { path: stored, .. } = &back else {
            panic!("kind changed");
        };
        assert!(!stored.is_foreign());
        assert_eq!(stored.as_path(), path.as_path());
        assert_eq!(back, side);
    }

    #[test]
    fn a_path_encoded_by_another_platform_is_written_back_unchanged() {
        let foreign = if cfg!(windows) {
            r#"{"type":"local","path":{"encoding":"unix-bytes","bytes":[47,116,109,112,255]}}"#
        } else {
            r#"{"type":"local","path":{"encoding":"windows-utf16","units":[47,116,55296]}}"#
        };
        let parsed: SideLocation = serde_json::from_str(foreign).unwrap();
        let SideLocation::Local { path, .. } = &parsed else {
            panic!("kind changed");
        };
        assert!(path.is_foreign());
        let written: Value =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(written, serde_json::from_str::<Value>(foreign).unwrap());
    }

    #[cfg(windows)]
    fn non_unicode_path() -> PathBuf {
        use std::os::windows::ffi::OsStringExt;
        // A lone high surrogate has no UTF-8 encoding, so the text form cannot
        // carry it.
        PathBuf::from(std::ffi::OsString::from_wide(&[
            0x0043, 0x003a, 0x005c, 0xd800, 0x005c, 0x0061,
        ]))
    }

    #[cfg(not(windows))]
    fn non_unicode_path() -> PathBuf {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(vec![
            0x2f, 0x74, 0x6d, 0x70, 0xff, 0x2f, 0x61,
        ]))
    }
}

//! Connection profiles.
//!
//! A profile is the stored description of one remote service: where it is,
//! how to log in, and how its listings and transfers behave. It holds no
//! secret. Where a password, a passphrase or an access key is needed the
//! profile carries a [`SecretRef`] and a [`SecretStore`](super::SecretStore)
//! supplies the value at connect time.
//!
//! # Forward compatibility
//!
//! A profile written by a later build stays readable here, under the same
//! three rules the snapshot format follows:
//!
//! 1. Every struct collects fields this build does not know into an `unknown`
//!    map and writes them back out unchanged.
//! 2. Every enum has an `Unknown` arm, so a service or an option added later
//!    reads as unknown instead of failing the whole document.
//! 3. Every field is `snake_case`, including the fields of a struct variant,
//!    which needs `rename_all_fields` as well as `rename_all`.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::secret::SecretRef;

/// Most bytes one stored profile document may hold.
pub const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

/// Fields a build does not know, kept so a round trip does not drop them.
pub type Unknown = BTreeMap<String, serde_json::Value>;

macro_rules! forward_string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let value = match self {
                    $(Self::$variant => $value,)+
                    Self::Unknown(value) => value,
                };
                serializer.serialize_str(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($value => Self::$variant,)+
                    _ => Self::Unknown(value),
                })
            }
        }
    };
}

/// True for a value equal to the type's default, used to keep the stored
/// document small.
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// One named remote service.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct RemoteProfile {
    /// Name the profile is addressed by.
    pub name: String,
    /// Free text identifying the profile.
    pub description: String,
    /// Which service this profile describes, and its settings.
    pub service: ServiceProfile,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// The service a profile describes.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "a profile is stored once per service, never in a hot collection"
)]
pub enum ServiceProfile {
    /// File transfer protocol, with or without transport security, and the
    /// secure shell transfer protocol, which shares the same profile shape.
    Ftp(FtpProfile),
    /// Web distributed authoring and versioning over HTTP.
    WebDav(WebDavProfile),
    /// Amazon S3 and the servers that speak the same interface.
    S3(S3Profile),
    /// Dropbox.
    Dropbox(CloudProfile),
    /// The Microsoft `OneDrive` service.
    OneDrive(CloudProfile),
    /// A Subversion repository, read only.
    Subversion(SubversionProfile),
    /// The complete document for a service this build does not know.
    Unknown(Unknown),
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "snake_case"
)]
enum KnownServiceProfile {
    Ftp(Box<FtpProfile>),
    WebDav(WebDavProfile),
    S3(S3Profile),
    Dropbox(CloudProfile),
    OneDrive(CloudProfile),
    Subversion(SubversionProfile),
}

impl Serialize for ServiceProfile {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Ftp(profile) => {
                KnownServiceProfile::Ftp(Box::new(profile.clone())).serialize(serializer)
            }
            Self::WebDav(profile) => {
                KnownServiceProfile::WebDav(profile.clone()).serialize(serializer)
            }
            Self::S3(profile) => KnownServiceProfile::S3(profile.clone()).serialize(serializer),
            Self::Dropbox(profile) => {
                KnownServiceProfile::Dropbox(profile.clone()).serialize(serializer)
            }
            Self::OneDrive(profile) => {
                KnownServiceProfile::OneDrive(profile.clone()).serialize(serializer)
            }
            Self::Subversion(profile) => {
                KnownServiceProfile::Subversion(profile.clone()).serialize(serializer)
            }
            Self::Unknown(fields) => serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ServiceProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let Some(fields) = value.as_object() else {
            return Err(serde::de::Error::custom("expected a service object"));
        };
        let Some(kind) = fields.get("kind").and_then(serde_json::Value::as_str) else {
            return Err(serde::de::Error::custom(
                "service object has no string kind",
            ));
        };
        match kind {
            "ftp" | "web_dav" | "s3" | "dropbox" | "one_drive" | "subversion" => {
                let known = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(match known {
                    KnownServiceProfile::Ftp(profile) => Self::Ftp(*profile),
                    KnownServiceProfile::WebDav(profile) => Self::WebDav(profile),
                    KnownServiceProfile::S3(profile) => Self::S3(profile),
                    KnownServiceProfile::Dropbox(profile) => Self::Dropbox(profile),
                    KnownServiceProfile::OneDrive(profile) => Self::OneDrive(profile),
                    KnownServiceProfile::Subversion(profile) => Self::Subversion(profile),
                })
            }
            _ => Ok(Self::Unknown(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )),
        }
    }
}

impl Default for ServiceProfile {
    fn default() -> Self {
        Self::Ftp(FtpProfile::default())
    }
}

// ---------------------------------------------------------------- file transfer

/// Everything one file transfer profile stores.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct FtpProfile {
    /// Protocol, host, port and account.
    pub login: FtpLogin,
    /// How the server behaves.
    pub server: FtpServer,
    /// How the connection is made and secured.
    pub connection: FtpConnection,
    /// Firewall or proxy in front of the server.
    pub proxy: FtpProxy,
    /// What the listing command asks for.
    pub listing: FtpListing,
    /// How content moves.
    pub transfer: FtpTransfer,
    /// Settings that apply to every profile of this kind.
    pub global: FtpGlobal,
    /// Folder on the server the profile opens at.
    pub root_path: String,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Protocol, host, port and account.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct FtpLogin {
    /// Which protocol the profile speaks.
    pub protocol: FtpProtocol,
    /// Server host name or address.
    pub host: String,
    /// Port, when it is not the protocol's standard one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Account name.
    pub username: String,
    /// Where the account password is stored.
    #[serde(skip_serializing_if = "SecretRef::is_empty")]
    pub password: SecretRef,
    /// Whether the password is kept for the next session.
    pub save_password: bool,
    /// Whether the account is the anonymous account.
    pub anonymous: bool,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Which protocol a file transfer profile speaks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FtpProtocol {
    /// Plain file transfer protocol.
    #[default]
    Ftp,
    /// File transfer protocol that upgrades the control channel with the
    /// explicit authentication command.
    FtpsExplicit,
    /// File transfer protocol whose control channel is secured from the first
    /// byte.
    FtpsImplicit,
    /// Secure shell file transfer protocol.
    Sftp,
    /// A protocol this build does not know.
    Unknown(String),
}

forward_string_enum!(FtpProtocol {
    Ftp => "ftp",
    FtpsExplicit => "ftps_explicit",
    FtpsImplicit => "ftps_implicit",
    Sftp => "sftp",
});

impl FtpProtocol {
    /// The port the protocol uses when the profile names none.
    #[must_use]
    pub fn default_port(&self) -> u16 {
        match self {
            Self::Ftp | Self::FtpsExplicit | Self::Unknown(_) => 21,
            Self::FtpsImplicit => 990,
            Self::Sftp => 22,
        }
    }

    /// True when the control channel carries transport security.
    #[must_use]
    pub const fn uses_tls(&self) -> bool {
        matches!(self, Self::FtpsExplicit | Self::FtpsImplicit)
    }
}

/// How the server behaves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct FtpServer {
    /// Names must match in case to align in a comparison.
    pub case_sensitive_names: bool,
    /// Character encoding of listings and commands.
    pub encoding: ListingEncoding,
    /// Name of the server's time zone, as the interface shows it.
    pub time_zone: String,
    /// Offset of the server's clock from UTC, in minutes, applied to listing
    /// times that carry no zone.
    pub time_zone_offset_minutes: i32,
    /// Commands sent after the login completes. The secure shell protocol
    /// carries no such command channel and ignores these.
    pub custom_login_commands: Vec<String>,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Default for FtpServer {
    fn default() -> Self {
        Self {
            case_sensitive_names: true,
            encoding: ListingEncoding::default(),
            time_zone: String::new(),
            time_zone_offset_minutes: 0,
            custom_login_commands: Vec::new(),
            unknown: Unknown::new(),
        }
    }
}

/// Character encoding of listings and commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ListingEncoding {
    /// Read listings as UTF-8, replacing anything that does not decode.
    #[default]
    Utf8,
    /// Read listings as UTF-8 where they decode and as single-byte text
    /// otherwise.
    Detect,
    /// Read listings as single-byte Latin-1 text.
    Latin1,
    /// An encoding this build does not know.
    Unknown(String),
}

forward_string_enum!(ListingEncoding {
    Utf8 => "utf8",
    Detect => "detect",
    Latin1 => "latin1",
});

/// How the connection is made and secured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a record of independent profile options"
)]
pub struct FtpConnection {
    /// Most connections opened to this server at once, from one to ten.
    pub max_connections: u8,
    /// A transfer fails when no byte arrives within this many seconds.
    pub read_timeout_seconds: u32,
    /// Ask the server for a data port instead of listening for one.
    pub passive: bool,
    /// Open the data connection to the address the passive reply names, rather
    /// than to the peer of the control connection.
    ///
    /// A server that names a third party makes this client connect there, so
    /// the setting stays off unless an operator states that the server's own
    /// address is the reachable one.
    pub trust_passive_address: bool,
    /// Ports the client listens on when the transfer is not passive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_port_range: Option<PortRange>,
    /// Name the virtual host before the account name.
    pub use_host_before_login: bool,
    /// Drop transport security on the control channel after the login.
    pub clear_control_channel: bool,
    /// Drop transport security on the data channel after the login.
    pub clear_data_channel: bool,
    /// Lowest transport security version to negotiate.
    pub min_tls_version: TlsVersionSetting,
    /// Highest transport security version to negotiate.
    pub max_tls_version: TlsVersionSetting,
    /// Certificate fingerprints accepted in addition to the trusted roots.
    pub pinned_certificate_fingerprints: Vec<String>,
    /// Accept any certificate. This removes the only defence against a
    /// machine in the middle of the connection.
    pub accept_any_certificate: bool,
    /// Seconds between keep-alive commands on an idle control channel; zero
    /// sends none.
    pub keep_alive_seconds: u32,
    /// Which resolved addresses to use.
    pub address_family: AddressFamily,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Default for FtpConnection {
    fn default() -> Self {
        Self {
            max_connections: 1,
            read_timeout_seconds: 60,
            passive: true,
            trust_passive_address: false,
            active_port_range: None,
            use_host_before_login: false,
            clear_control_channel: false,
            clear_data_channel: false,
            min_tls_version: TlsVersionSetting::default(),
            max_tls_version: TlsVersionSetting::default(),
            pinned_certificate_fingerprints: Vec::new(),
            accept_any_certificate: false,
            keep_alive_seconds: 0,
            address_family: AddressFamily::default(),
            unknown: Unknown::new(),
        }
    }
}

/// A range of local ports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct PortRange {
    /// Lowest port in the range.
    pub first: u16,
    /// Highest port in the range.
    pub last: u16,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// A transport security version a profile names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TlsVersionSetting {
    /// Let the library choose.
    #[default]
    Any,
    /// Transport security version 1.2.
    Tls12,
    /// Transport security version 1.3.
    Tls13,
    /// A version this build does not know.
    Unknown(String),
}

forward_string_enum!(TlsVersionSetting {
    Any => "any",
    Tls12 => "tls12",
    Tls13 => "tls13",
});

#[cfg(feature = "tls")]
impl TryFrom<TlsVersionSetting> for super::tls::TlsVersion {
    type Error = crate::error::VfsError;

    fn try_from(value: TlsVersionSetting) -> Result<Self, Self::Error> {
        match value {
            TlsVersionSetting::Any => Ok(Self::Any),
            TlsVersionSetting::Tls12 => Ok(Self::Tls12),
            TlsVersionSetting::Tls13 => Ok(Self::Tls13),
            TlsVersionSetting::Unknown(_) => Err(crate::error::VfsError::tls(
                "the profile uses an unsupported TLS version setting",
            )),
        }
    }
}

/// Which resolved addresses a connection uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AddressFamily {
    /// Try every address in the order the resolver returned them.
    #[default]
    Any,
    /// Prefer version 6 addresses.
    Ipv6First,
    /// Use version 4 addresses only.
    Ipv4Only,
    /// A setting this build does not know.
    Unknown(String),
}

forward_string_enum!(AddressFamily {
    Any => "any",
    Ipv6First => "ipv6_first",
    Ipv4Only => "ipv4_only",
});

impl From<AddressFamily> for super::net::AddressPreference {
    fn from(value: AddressFamily) -> Self {
        match value {
            AddressFamily::Ipv6First => Self::Ipv6First,
            AddressFamily::Ipv4Only => Self::Ipv4Only,
            AddressFamily::Any | AddressFamily::Unknown(_) => Self::Resolver,
        }
    }
}

/// Firewall or proxy in front of the server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct FtpProxy {
    /// Route the connection through the proxy.
    pub enabled: bool,
    /// Which proxy protocol the firewall speaks.
    pub kind: ProxyKind,
    /// Proxy host name or address.
    pub host: String,
    /// Proxy port.
    pub port: u16,
    /// Account name at the proxy.
    pub username: String,
    /// Where the proxy password is stored.
    #[serde(skip_serializing_if = "SecretRef::is_empty")]
    pub password: SecretRef,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Which proxy protocol a firewall speaks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProxyKind {
    /// No proxy.
    #[default]
    None,
    /// Send the account name as `user@host`.
    UserAtHost,
    /// Log in at the proxy, then send `SITE host`.
    SiteHost,
    /// Log in at the proxy, then send `OPEN host`.
    OpenHost,
    /// SOCKS version 4.
    Socks4,
    /// SOCKS version 5.
    Socks5,
    /// A tunnel opened with an HTTP connect request.
    HttpConnect,
    /// A proxy protocol this build does not know.
    Unknown(String),
}

forward_string_enum!(ProxyKind {
    None => "none",
    UserAtHost => "user_at_host",
    SiteHost => "site_host",
    OpenHost => "open_host",
    Socks4 => "socks4",
    Socks5 => "socks5",
    HttpConnect => "http_connect",
});

/// What the listing command asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a record of independent profile options"
)]
pub struct FtpListing {
    /// How a link's kind is decided.
    pub link_resolution: LinkResolution,
    /// Ask for the machine-readable listing first.
    pub use_mlsd: bool,
    /// Ask the server to include entries it normally omits.
    pub show_hidden: bool,
    /// Ask for the long listing format.
    pub force_long_format: bool,
    /// Ask for a full date and time on every line.
    pub complete_timestamps: bool,
    /// Ask the server to list a link's target rather than the link.
    pub resolve_links: bool,
    /// Ask one listing to cover the whole tree.
    pub recursive: bool,
    /// List only the newest version of a file on a server that keeps
    /// versions.
    pub hide_vms_versions: bool,
    /// Ask for the modification time of a file whose listing line carries
    /// only a date.
    pub fetch_incomplete_timestamps: bool,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Default for FtpListing {
    fn default() -> Self {
        Self {
            link_resolution: LinkResolution::default(),
            use_mlsd: true,
            show_hidden: false,
            force_long_format: false,
            complete_timestamps: false,
            resolve_links: false,
            recursive: false,
            hide_vms_versions: true,
            fetch_incomplete_timestamps: false,
            unknown: Unknown::new(),
        }
    }
}

/// How a link's kind is decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LinkResolution {
    /// A name with an extension is a file, anything else is a folder. No
    /// extra request is made.
    #[default]
    Fast,
    /// Try to change directory into the link; success means a folder.
    Simple,
    /// A setting this build does not know.
    Unknown(String),
}

forward_string_enum!(LinkResolution {
    Fast => "fast",
    Simple => "simple",
});

/// How content moves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a record of independent profile options"
)]
pub struct FtpTransfer {
    /// Byte-exact, line-ending adjusted, or chosen per file extension.
    pub transfer_type: TransferType,
    /// Set the time of an uploaded file to match the source.
    pub copy_timestamps: bool,
    /// Set the permissions of an uploaded file to match the source.
    pub copy_unix_permissions: bool,
    /// Ask the server to compress the data channel.
    pub compress_transfers: bool,
    /// Cap on download rate in kilobits per second; zero means no cap.
    pub download_limit_kbps: u32,
    /// Cap on upload rate in kilobits per second; zero means no cap.
    pub upload_limit_kbps: u32,
    /// Use larger buffers and pipelining against a secure shell server that
    /// accepts them.
    pub aggressive_uploads: bool,
    /// Continue an interrupted transfer from where it stopped.
    pub resume_interrupted: bool,
    /// Write to a temporary name and rename it into place, so an interrupted
    /// upload leaves the old content untouched.
    pub upload_through_temporary_name: bool,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Default for FtpTransfer {
    fn default() -> Self {
        Self {
            transfer_type: TransferType::default(),
            copy_timestamps: true,
            copy_unix_permissions: true,
            compress_transfers: false,
            download_limit_kbps: 0,
            upload_limit_kbps: 0,
            aggressive_uploads: false,
            resume_interrupted: true,
            upload_through_temporary_name: true,
            unknown: Unknown::new(),
        }
    }
}

/// Byte-exact, line-ending adjusted, or chosen per file extension.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TransferType {
    /// Byte-exact.
    #[default]
    Binary,
    /// The server adjusts line endings.
    Ascii,
    /// Chosen per file extension from the global list.
    Auto,
    /// A setting this build does not know.
    Unknown(String),
}

forward_string_enum!(TransferType {
    Binary => "binary",
    Ascii => "ascii",
    Auto => "auto",
});

/// Settings shared by every file transfer profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct FtpGlobal {
    /// Address sent as the password for an anonymous login.
    pub anonymous_login_email: String,
    /// Offer keys held by a running agent.
    pub ssh_agent: bool,
    /// Path to a secure shell private key file.
    pub ssh_private_key_file: String,
    /// Where the passphrase of that key is stored.
    #[serde(skip_serializing_if = "SecretRef::is_empty")]
    pub ssh_private_key_passphrase: SecretRef,
    /// Offer answers to keyboard-interactive prompts.
    pub ssh_keyboard_interactive: bool,
    /// Path to the client certificate offered to a server that asks for one.
    pub ssl_client_certificate_file: String,
    /// Extensions transferred as text when the transfer type is chosen per
    /// file.
    pub ascii_types: Vec<String>,
    /// Path to the store of accepted host keys. The caller supplies it; this
    /// crate never picks a location of its own.
    pub known_hosts_file: String,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl Default for FtpGlobal {
    fn default() -> Self {
        Self {
            anonymous_login_email: "anonymous@example.invalid".to_owned(),
            ssh_agent: false,
            ssh_private_key_file: String::new(),
            ssh_private_key_passphrase: SecretRef::default(),
            ssh_keyboard_interactive: true,
            ssl_client_certificate_file: String::new(),
            ascii_types: Vec::new(),
            known_hosts_file: String::new(),
            unknown: Unknown::new(),
        }
    }
}

// ---------------------------------------------------------------------- webdav

/// A profile for a share reached over HTTP.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct WebDavProfile {
    /// Address of the share, including the scheme and any base folder.
    pub url: String,
    /// Account name.
    pub username: String,
    /// Where the account password is stored.
    #[serde(skip_serializing_if = "SecretRef::is_empty")]
    pub password: SecretRef,
    /// Which authentication scheme to offer.
    pub auth: HttpAuthScheme,
    /// Ask one request to cover the whole tree.
    ///
    /// The listing request is still made one level at a time. A request that
    /// covers a whole tree makes a server walk it in full for every folder
    /// opened, so nothing sends one.
    pub recursive_listings: bool,
    /// Send the account name and password over a plain connection.
    ///
    /// Basic authentication carries the password in a reversible encoding, so
    /// over plain HTTP anything on the path can read it.
    pub allow_plaintext_credentials: bool,
    /// How the connection treats the server's certificate.
    #[serde(skip_serializing_if = "is_default")]
    pub tls: TlsSettings,
    /// Seconds a request may take before it fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Which authentication scheme an HTTP request offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HttpAuthScheme {
    /// Answer whatever the server asks for.
    #[default]
    Negotiate,
    /// Send the account name and password encoded, never over plain HTTP.
    Basic,
    /// Answer the server's challenge with a digest.
    Digest,
    /// Send no credentials.
    None,
    /// A scheme this build does not know.
    Unknown(String),
}

forward_string_enum!(HttpAuthScheme {
    Negotiate => "negotiate",
    Basic => "basic",
    Digest => "digest",
    None => "none",
});

/// How a connection treats the server's certificate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct TlsSettings {
    /// Lowest transport security version to negotiate.
    pub min_version: TlsVersionSetting,
    /// Highest transport security version to negotiate.
    pub max_version: TlsVersionSetting,
    /// Certificate fingerprints accepted in addition to the trusted roots.
    pub pinned_certificate_fingerprints: Vec<String>,
    /// Accept any certificate. This removes the only defence against a
    /// machine in the middle of the connection.
    pub accept_any_certificate: bool,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[cfg(feature = "tls")]
impl TryFrom<&TlsSettings> for super::tls::TlsOptions {
    type Error = crate::error::VfsError;

    fn try_from(value: &TlsSettings) -> Result<Self, Self::Error> {
        Ok(Self {
            min_version: value.min_version.clone().try_into()?,
            max_version: value.max_version.clone().try_into()?,
            pinned_fingerprints: value.pinned_certificate_fingerprints.clone(),
            accept_any_certificate: value.accept_any_certificate,
        })
    }
}

// -------------------------------------------------------------------- object store

/// A profile for an object store.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct S3Profile {
    /// Where the credentials come from.
    pub auth: S3Auth,
    /// Bucket the profile opens at. An account that may not list buckets
    /// needs one named here.
    pub bucket: String,
    /// Region the bucket is in.
    pub region: String,
    /// Address of the service, for a server that is not the public one.
    pub endpoint: String,
    /// Put the bucket in the path rather than in the host name.
    pub path_style: bool,
    /// How the connection treats the server's certificate.
    #[serde(skip_serializing_if = "is_default")]
    pub tls: TlsSettings,
    /// Seconds a request may take before it fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Where the credentials of an object store profile come from.
#[derive(Debug, Clone, PartialEq)]
pub enum S3Auth {
    /// Keys stored with the profile.
    Saved {
        /// Access key identifier.
        access_key_id: String,
        /// Where the secret access key is stored.
        secret_access_key: SecretRef,
        /// Where a session token is stored, for time-limited credentials.
        session_token: SecretRef,
        /// Fields this build does not know.
        unknown: Unknown,
    },
    /// Keys read from a credentials file.
    CredentialsFile {
        /// Path to the file. Empty means the platform's default path.
        path: String,
        /// Name of the section in that file. Empty means the default section.
        profile_name: String,
        /// Fields this build does not know.
        unknown: Unknown,
    },
    /// Keys read from the process environment, falling back to the default
    /// credentials file.
    Environment {
        /// Name of the section to read when the fallback applies.
        profile_name: String,
        /// Fields this build does not know.
        unknown: Unknown,
    },
    /// No credentials; the service is reached anonymously.
    Anonymous {
        /// Fields this build does not know.
        unknown: Unknown,
    },
    /// The complete credential source object when this build does not know it.
    Unknown(Unknown),
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "source",
    rename_all = "snake_case",
    rename_all_fields = "snake_case"
)]
enum KnownS3Auth {
    Saved {
        access_key_id: String,
        secret_access_key: SecretRef,
        #[serde(default, skip_serializing_if = "SecretRef::is_empty")]
        session_token: SecretRef,
        #[serde(default, flatten)]
        unknown: Unknown,
    },
    CredentialsFile {
        #[serde(default)]
        path: String,
        #[serde(default)]
        profile_name: String,
        #[serde(default, flatten)]
        unknown: Unknown,
    },
    Environment {
        #[serde(default)]
        profile_name: String,
        #[serde(default, flatten)]
        unknown: Unknown,
    },
    Anonymous {
        #[serde(default, flatten)]
        unknown: Unknown,
    },
}

impl Serialize for S3Auth {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Saved {
                access_key_id,
                secret_access_key,
                session_token,
                unknown,
            } => KnownS3Auth::Saved {
                access_key_id: access_key_id.clone(),
                secret_access_key: secret_access_key.clone(),
                session_token: session_token.clone(),
                unknown: unknown.clone(),
            }
            .serialize(serializer),
            Self::CredentialsFile {
                path,
                profile_name,
                unknown,
            } => KnownS3Auth::CredentialsFile {
                path: path.clone(),
                profile_name: profile_name.clone(),
                unknown: unknown.clone(),
            }
            .serialize(serializer),
            Self::Environment {
                profile_name,
                unknown,
            } => KnownS3Auth::Environment {
                profile_name: profile_name.clone(),
                unknown: unknown.clone(),
            }
            .serialize(serializer),
            Self::Anonymous { unknown } => KnownS3Auth::Anonymous {
                unknown: unknown.clone(),
            }
            .serialize(serializer),
            Self::Unknown(fields) => serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for S3Auth {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let Some(fields) = value.as_object() else {
            return Err(serde::de::Error::custom(
                "expected an S3 credential source object",
            ));
        };
        let Some(source) = fields.get("source").and_then(serde_json::Value::as_str) else {
            return Err(serde::de::Error::custom(
                "S3 credential source object has no string source",
            ));
        };
        match source {
            "saved" | "credentials_file" | "environment" | "anonymous" => {
                let known = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(match known {
                    KnownS3Auth::Saved {
                        access_key_id,
                        secret_access_key,
                        session_token,
                        unknown,
                    } => Self::Saved {
                        access_key_id,
                        secret_access_key,
                        session_token,
                        unknown,
                    },
                    KnownS3Auth::CredentialsFile {
                        path,
                        profile_name,
                        unknown,
                    } => Self::CredentialsFile {
                        path,
                        profile_name,
                        unknown,
                    },
                    KnownS3Auth::Environment {
                        profile_name,
                        unknown,
                    } => Self::Environment {
                        profile_name,
                        unknown,
                    },
                    KnownS3Auth::Anonymous { unknown } => Self::Anonymous { unknown },
                })
            }
            _ => Ok(Self::Unknown(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )),
        }
    }
}

impl Default for S3Auth {
    fn default() -> Self {
        Self::Environment {
            profile_name: String::new(),
            unknown: Unknown::new(),
        }
    }
}

// -------------------------------------------------------------------- other services

/// A profile whose authorization happens outside the settings document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct CloudProfile {
    /// Identifier of the account the authorization belongs to.
    pub account: String,
    /// Where the refresh token is stored.
    #[serde(skip_serializing_if = "SecretRef::is_empty")]
    pub refresh_token: SecretRef,
    /// Identifier this program registered with the service.
    pub client_id: String,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// A profile for a revision repository, read only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct SubversionProfile {
    /// Address of the repository.
    pub url: String,
    /// Revision the profile is pinned to; none means the newest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// Account name.
    pub username: String,
    /// Where the account password is stored.
    #[serde(skip_serializing_if = "SecretRef::is_empty")]
    pub password: SecretRef,
    /// Fields this build does not know.
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_profile_round_trips() {
        let profile = RemoteProfile {
            name: "example".to_owned(),
            description: "a server".to_owned(),
            service: ServiceProfile::Ftp(FtpProfile::default()),
            unknown: Unknown::new(),
        };
        let text = serde_json::to_string(&profile).unwrap();
        let back: RemoteProfile = serde_json::from_str(&text).unwrap();
        assert_eq!(back, profile);
        assert!(!text.contains("secret_value"));
    }

    #[test]
    fn a_service_this_build_does_not_know_reads_as_unknown() {
        let text = r#"{"name":"x","description":"","service":{"kind":"quantum_drive","future":{"value":42}}}"#;
        let back: RemoteProfile = serde_json::from_str(text).unwrap();
        let encoded = serde_json::to_value(back.service).unwrap();
        let original = serde_json::from_str::<serde_json::Value>(text).unwrap();
        assert_eq!(encoded, original["service"]);
    }

    #[test]
    fn an_option_this_build_does_not_know_reads_as_unknown() {
        let text = r#"{"kind":"ftp","login":{"protocol":"telepathy"}}"#;
        let back: ServiceProfile = serde_json::from_str(text).unwrap();
        let ServiceProfile::Ftp(ftp) = back else {
            panic!("expected a file transfer profile");
        };
        assert_eq!(
            ftp.login.protocol,
            FtpProtocol::Unknown("telepathy".to_owned())
        );
        let encoded = serde_json::to_value(ftp.login.protocol).unwrap();
        assert_eq!(encoded, serde_json::json!("telepathy"));
    }

    #[test]
    fn anonymous_s3_auth_preserves_fields_added_by_a_later_build() {
        let text = r#"{"source":"anonymous","future_option":{"enabled":true}}"#;
        let auth: S3Auth = serde_json::from_str(text).unwrap();
        let encoded = serde_json::to_value(auth).unwrap();
        let original = serde_json::from_str::<serde_json::Value>(text).unwrap();
        assert_eq!(encoded, original);
    }

    #[test]
    fn unknown_profile_numbers_keep_their_precision() {
        let source = r#"{"name":"x","future_integer":123456789012345678901234567890,"future_fraction":0.10000000000000000555111512312578270211815834045,"future_exponent":1e2,"service":{"kind":"quantum_drive","future_integer":18446744073709551616}}"#;
        let profile: RemoteProfile = serde_json::from_str(source).unwrap();
        let written = serde_json::to_string(&profile).unwrap();

        for token in [
            "123456789012345678901234567890",
            "0.10000000000000000555111512312578270211815834045",
            "18446744073709551616",
        ] {
            assert!(
                written.contains(token),
                "lost numeric spelling {token}: {written}"
            );
        }
        let written: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(
            written["future_exponent"].as_number().unwrap().to_string(),
            "1e+2"
        );
    }

    #[cfg(feature = "tls")]
    #[test]
    fn unknown_tls_version_settings_are_refused() {
        use crate::remote::tls::{TlsOptions, TlsVersion};

        assert!(TlsVersion::try_from(TlsVersionSetting::Unknown("tls14".to_owned())).is_err());
        for settings in [
            TlsSettings {
                min_version: TlsVersionSetting::Unknown("tls14".to_owned()),
                ..TlsSettings::default()
            },
            TlsSettings {
                max_version: TlsVersionSetting::Unknown("tls14".to_owned()),
                ..TlsSettings::default()
            },
        ] {
            assert!(TlsOptions::try_from(&settings).is_err());
        }
    }
}

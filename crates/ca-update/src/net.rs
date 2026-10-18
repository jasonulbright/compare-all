//! Network access: the operating system proxy and certificate store.
//!
//! TLS verification goes through the platform verifier, so a certificate
//! chain that ends in a root the organization installed is trusted exactly as
//! it is in the browser. The proxy comes from the Windows registry settings
//! of the current user (and the proxy environment variables elsewhere).
//! Automatic configuration scripts (PAC, WPAD) and proxy authentication are not
//! supported.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Content types a release file may arrive with.
const FILE_CONTENT_TYPES: &[&str] = &[
    "application/octet-stream",
    "binary/octet-stream",
    "application/zip",
    "application/x-zip",
    "application/x-zip-compressed",
    "application/x-compressed",
    "application/x-msi",
    "application/x-msdownload",
    "application/x-ole-storage",
    "application/vnd.ms-cab-compressed",
];

/// True when a response of `content_type` may be a release file.
///
/// A response without a type passes here; the size, the format check and the
/// checksum decide.
#[must_use]
pub fn is_file_content_type(content_type: Option<&str>) -> bool {
    let Some(raw) = content_type else {
        return true;
    };
    let essence = raw
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    FILE_CONTENT_TYPES.contains(&essence.as_str())
}

/// What a download delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The `Content-Type` of the final response, if it sent one.
    pub content_type: Option<String>,
    /// Bytes written to the destination.
    pub bytes: u64,
}

/// Reads documents and files over HTTP.
pub trait Transport: Send + Sync {
    /// Read a small document into memory.
    ///
    /// # Errors
    /// Returns why the document could not be read.
    fn get_document(
        &self,
        url: &str,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<u8>, String>;

    /// Write a file to `destination`. The caller verifies the result: a
    /// success here proves only that bytes arrived.
    ///
    /// # Errors
    /// Returns why the transfer failed.
    fn download(
        &self,
        url: &str,
        destination: &Path,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Fetched, String>;
}

/// The proxy settings of the machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyConfig {
    /// `host:port` of the proxy for secure requests.
    pub server: Option<String>,
    /// Host patterns that skip the proxy. `*` matches any run of characters.
    pub bypass: Vec<String>,
    /// Skip the proxy for names without a dot (`<local>`).
    pub bypass_local: bool,
}

impl ProxyConfig {
    /// No proxy.
    #[must_use]
    pub fn direct() -> Self {
        Self::default()
    }

    /// Read the `ProxyServer` and `ProxyOverride` values of the Windows
    /// Internet settings.
    #[must_use]
    pub fn from_windows_settings(enabled: bool, server: &str, override_list: &str) -> Self {
        if !enabled {
            return Self::direct();
        }
        let mut chosen: Option<String> = None;
        let mut fallback: Option<String> = None;
        for part in server.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            match part.split_once('=') {
                Some((scheme, value)) => {
                    if scheme.eq_ignore_ascii_case("https") {
                        chosen = Some(strip_scheme(value));
                    } else if scheme.eq_ignore_ascii_case("http") && fallback.is_none() {
                        fallback = Some(strip_scheme(value));
                    }
                }
                None => chosen = Some(strip_scheme(part)),
            }
        }
        let mut bypass = Vec::new();
        let mut bypass_local = false;
        for item in override_list
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            if item.eq_ignore_ascii_case("<local>") {
                bypass_local = true;
            } else {
                bypass.push(item.to_ascii_lowercase());
            }
        }
        Self {
            server: chosen.or(fallback).filter(|value| !value.is_empty()),
            bypass,
            bypass_local,
        }
    }

    /// The settings of this machine.
    #[must_use]
    pub fn system() -> Self {
        #[cfg(windows)]
        {
            windows_proxy()
        }
        #[cfg(not(windows))]
        {
            let value = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
                .iter()
                .find_map(|name| std::env::var(name).ok())
                .filter(|value| !value.is_empty());
            Self {
                server: value.map(|value| strip_scheme(&value)),
                ..Self::default()
            }
        }
    }

    /// The proxy address to use for `host`, if any.
    #[must_use]
    pub fn for_host(&self, host: &str) -> Option<&str> {
        let server = self.server.as_deref()?;
        let host = host.to_ascii_lowercase();
        if self.bypass_local && !host.contains('.') {
            return None;
        }
        if self
            .bypass
            .iter()
            .any(|pattern| wildcard_matches(pattern, &host))
        {
            return None;
        }
        Some(server)
    }
}

fn strip_scheme(value: &str) -> String {
    let value = value.trim();
    value
        .split_once("://")
        .map_or(value, |(_, rest)| rest)
        .trim_end_matches('/')
        .to_owned()
}

fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(windows)]
fn windows_proxy() -> ProxyConfig {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
    else {
        return ProxyConfig::direct();
    };
    let enabled = key.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0;
    let server: String = key.get_value("ProxyServer").unwrap_or_default();
    let list: String = key.get_value("ProxyOverride").unwrap_or_default();
    ProxyConfig::from_windows_settings(enabled, &server, &list)
}

/// HTTP over `ureq`, with the platform certificate verifier.
#[derive(Debug, Clone)]
pub struct HttpTransport {
    user_agent: String,
    proxy: ProxyConfig,
    timeout: Duration,
    allow_plain_http: bool,
}

impl HttpTransport {
    /// A transport that refuses any address that is not HTTPS.
    #[must_use]
    pub fn new(user_agent: String, proxy: ProxyConfig, timeout: Duration) -> Self {
        Self {
            user_agent,
            proxy,
            timeout,
            allow_plain_http: false,
        }
    }

    /// Accept `http://` addresses. For tests against a local server only.
    #[must_use]
    pub fn allowing_plain_http(mut self) -> Self {
        self.allow_plain_http = true;
        self
    }

    fn agent_for(&self, url: &str) -> Result<ureq::Agent, String> {
        let host = url
            .split_once("://")
            .map_or("", |(_, rest)| rest)
            .split(['/', ':', '?'])
            .next()
            .unwrap_or_default();
        let proxy = match self.proxy.for_host(host) {
            Some(server) => Some(
                ureq::Proxy::new(&format!("http://{server}"))
                    .map_err(|error| format!("the proxy setting is not usable: {error}"))?,
            ),
            None => None,
        };
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .proxy(proxy)
            .tls_config(tls)
            .https_only(!self.allow_plain_http)
            .max_redirects(5)
            .timeout_global(Some(self.timeout))
            .user_agent(self.user_agent.clone())
            .build();
        Ok(ureq::Agent::new_with_config(config))
    }

    fn open(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        self.agent_for(url)?
            .get(url)
            .call()
            .map_err(|error| error.to_string())
    }
}

impl Transport for HttpTransport {
    fn get_document(
        &self,
        url: &str,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<u8>, String> {
        let this = self.clone();
        let url = url.to_owned();
        cancellable(cancel, move |flag| {
            let response = this.open(&url)?;
            let mut reader = response.into_body().into_reader();
            let mut out = Vec::new();
            copy_limited(&mut reader, &mut out, max_bytes, &flag)?;
            Ok(out)
        })
    }

    fn download(
        &self,
        url: &str,
        destination: &Path,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Fetched, String> {
        let this = self.clone();
        let url = url.to_owned();
        let destination = destination.to_path_buf();
        cancellable(cancel, move |flag| {
            let response = this.open(&url)?;
            let content_type = response.body().mime_type().map(str::to_owned);
            if response
                .body()
                .content_length()
                .is_some_and(|declared| declared > max_bytes)
            {
                return Err("the server announces a file larger than any release".to_owned());
            }
            let mut reader = response.into_body().into_reader();
            let mut file = std::fs::File::create(&destination)
                .map_err(|error| format!("cannot write {}: {error}", destination.display()))?;
            let bytes = copy_limited(&mut reader, &mut file, max_bytes, &flag)?;
            file.flush().map_err(|error| error.to_string())?;
            Ok(Fetched {
                content_type,
                bytes,
            })
        })
    }
}

/// Run `work` on its own thread and give up on it when `cancel` is set. A
/// blocked connect or a silent server cannot be interrupted from outside, so
/// the abandoned thread ends on its own timeout.
fn cancellable<T: Send + 'static>(
    cancel: &Arc<AtomicBool>,
    work: impl FnOnce(Arc<AtomicBool>) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let flag = Arc::clone(cancel);
    std::thread::spawn(move || {
        let _ = sender.send(work(flag));
    });
    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::Relaxed) {
                    return Err("the transfer was cancelled".to_owned());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("the transfer ended without a result".to_owned());
            }
        }
    }
}

fn copy_limited(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    max_bytes: u64,
    cancel: &Arc<AtomicBool>,
) -> Result<u64, String> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("the transfer was cancelled".to_owned());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("the transfer failed: {error}"))?;
        if read == 0 {
            return Ok(total);
        }
        total += read as u64;
        if total > max_bytes {
            return Err("the response is larger than allowed".to_owned());
        }
        writer
            .write_all(&buffer[..read])
            .map_err(|error| format!("cannot write the download: {error}"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_proxy_text_is_read() {
        let config = ProxyConfig::from_windows_settings(
            true,
            "http=a:80;https=secure.example:8443",
            "*.corp.example;<local>;10.*",
        );
        assert_eq!(config.server.as_deref(), Some("secure.example:8443"));
        assert_eq!(config.for_host("github.com"), Some("secure.example:8443"));
        assert_eq!(config.for_host("tools.corp.example"), None);
        assert_eq!(config.for_host("intranet"), None);
        assert_eq!(config.for_host("10.1.2.3"), None);
        let single = ProxyConfig::from_windows_settings(true, "http://p:3128", "");
        assert_eq!(single.server.as_deref(), Some("p:3128"));
        assert_eq!(
            ProxyConfig::from_windows_settings(false, "p:1", ""),
            ProxyConfig::direct()
        );
    }

    #[test]
    fn content_types_of_release_files_are_listed() {
        assert!(is_file_content_type(Some("application/octet-stream")));
        assert!(is_file_content_type(Some("application/x-msi; charset=x")));
        assert!(is_file_content_type(None));
        assert!(!is_file_content_type(Some("text/html; charset=utf-8")));
        assert!(!is_file_content_type(Some("application/json")));
    }
}

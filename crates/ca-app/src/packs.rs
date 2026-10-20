//! The network behind the pack browser: the updater's HTTP transport, with the
//! same proxy and certificate settings and the same file content-type check.

use ca_ui::pack::remote::{FetchError, PackLocations, PackServices, PackTransport};
use ca_update::flow::Endpoints;
use ca_update::net::{is_file_content_type, HttpTransport, ProxyConfig, Transport};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Pack transfers over HTTPS.
pub struct HttpPackTransport {
    inner: HttpTransport,
}

impl HttpPackTransport {
    /// A transport that identifies itself as the running version.
    #[must_use]
    pub fn new(version: &str) -> Self {
        Self {
            inner: HttpTransport::new(
                crate::update::user_agent(version),
                ProxyConfig::system(),
                crate::update::TIMEOUT,
            ),
        }
    }
}

fn classify(reason: String) -> FetchError {
    if reason.contains("status: 404") {
        FetchError::NotFound
    } else {
        FetchError::Failed(reason)
    }
}

impl PackTransport for HttpPackTransport {
    fn document(
        &self,
        url: &str,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<u8>, FetchError> {
        self.inner
            .get_document(url, max_bytes, cancel)
            .map_err(classify)
    }

    fn file(
        &self,
        url: &str,
        destination: &Path,
        max_bytes: u64,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(), FetchError> {
        let fetched = self
            .inner
            .download(url, destination, max_bytes, cancel)
            .map_err(classify)?;
        if is_file_content_type(fetched.content_type.as_deref()) {
            Ok(())
        } else {
            Err(FetchError::Failed(format!(
                "the server answered with {}, not a file",
                fetched.content_type.as_deref().unwrap_or("no type")
            )))
        }
    }
}

/// The pack browser services for the release whose tag is `version`.
#[must_use]
pub fn services(settings: &Path, endpoints: &Endpoints, version: &str) -> Arc<PackServices> {
    Arc::new(PackServices {
        transport: Arc::new(HttpPackTransport::new(version)),
        locations: PackLocations::for_release(
            &endpoints.download_base,
            &endpoints.page_base,
            version,
        ),
        directory: settings.join("packs"),
    })
}

//! The network behind the pack browser: the updater's HTTP transport, with the
//! same proxy and certificate settings and the same file content-type check.

use ca_ui::pack::remote::{FetchError, PackLocations, PackServices, PackTransport};
use ca_update::flow::Endpoints;
use ca_update::net::{is_file_content_type, HttpTransport, ProxyConfig, TransferError};
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

fn classify(error: TransferError) -> FetchError {
    if error.status == Some(404) {
        FetchError::NotFound
    } else {
        FetchError::Failed(error.message)
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
            .fetch_document(url, max_bytes, cancel)
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
            .fetch_file(url, destination, max_bytes, cancel)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_status_404_means_no_list() {
        let error = |status, message: &str| TransferError {
            status,
            message: message.to_owned(),
        };
        assert_eq!(classify(error(Some(404), "x")), FetchError::NotFound);
        assert_eq!(
            classify(error(Some(500), "status 404 in the text")),
            FetchError::Failed("status 404 in the text".to_owned())
        );
        assert_eq!(
            classify(error(None, "http status: 404")),
            FetchError::Failed("http status: 404".to_owned())
        );
    }
}

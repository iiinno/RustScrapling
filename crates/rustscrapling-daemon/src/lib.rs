//! Authenticated localhost daemon for shared RustScrapling fetching.
//!
//! Daemon-owned fetch coordination works with any [`FetchBackend`]. Enable
//! `fetch-integration` to bind the real `rustscrapling_fetch::Fetcher` once
//! its HTTP dependency is available on the integration branch.

mod persistence;
mod protocol;
mod service;

pub use protocol::{
    DaemonError, DaemonRequest, DaemonResponse, FetchRequest, FetchResult, FetchTier, Status,
};
pub use service::{DaemonClient, DaemonConfig, DaemonServer, FetchBackend, FetchFuture};

#[cfg(feature = "fetch-integration")]
mod integration;

#[cfg(feature = "fetch-integration")]
pub use integration::FetcherBackend;

#[cfg(feature = "fetch-integration")]
impl DaemonServer {
    /// Construct a daemon using the real HTTP/browser acquisition policy.
    pub async fn start_with_fetcher(
        config: DaemonConfig,
        fetcher: rustscrapling_fetch::Fetcher,
    ) -> Result<Self, DaemonError> {
        Self::start(config, std::sync::Arc::new(FetcherBackend(fetcher))).await
    }
}

//! Adapter to the shared acquisition policy; enabled after HTTP integration.
use crate::{DaemonError, FetchBackend, FetchFuture, FetchRequest, FetchResult, FetchTier};
use rustscrapling_fetch::{FetchErrorKind, FetchTier as PolicyTier, Fetcher};
use rustscrapling_http::HttpRequest;

pub struct FetcherBackend(pub Fetcher);

impl FetchBackend for FetcherBackend {
    fn fetch<'a>(&'a self, request: FetchRequest) -> FetchFuture<'a> {
        Box::pin(async move {
            let mut http = HttpRequest::new(request.method, request.url);
            http.headers = request.headers;
            http.body = request.body;
            let policy_tier = match request.tier {
                FetchTier::Auto => PolicyTier::Auto,
                FetchTier::Http => PolicyTier::Http,
                FetchTier::Browser => PolicyTier::Browser,
                FetchTier::Headed => PolicyTier::Headed,
            };
            let response = self
                .0
                .fetch(http, policy_tier)
                .await
                .map_err(|e| DaemonError {
                    kind: e.kind.as_str().into(),
                    status: e.status,
                    // Never relay backend/URL/proxy diagnostic text over IPC:
                    // it may contain userinfo or proxy credentials.
                    message: "acquisition backend request failed".into(),
                    hint: match e.kind {
                        FetchErrorKind::Timeout => "Increase timeout or retry later",
                        FetchErrorKind::Tls => "Check CA trust",
                        FetchErrorKind::Dns => "Check DNS and hostname",
                        FetchErrorKind::Blocked => "Respect source access restrictions",
                        FetchErrorKind::BrowserUnavailable => "Configure a compatible browser",
                        FetchErrorKind::Proxy => "Check proxy configuration",
                        _ => "Inspect request parameters and retry",
                    }
                    .into(),
                })?;
            let tier = match response.tier {
                PolicyTier::Auto => FetchTier::Auto,
                PolicyTier::Http => FetchTier::Http,
                PolicyTier::Browser => FetchTier::Browser,
                PolicyTier::Headed => FetchTier::Headed,
            };
            Ok(FetchResult {
                status: response.status,
                final_url: response.final_url,
                headers: response.headers,
                body: response.body,
                tier,
                protocol: response.protocol,
            })
        })
    }
}

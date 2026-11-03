//! Per-page browser settings matching upstream `StealthySession` options:
//! locale, timezone, user agent, cookies, extra headers, blocked domains and
//! disabled resource types. Everything goes through CDP domain commands; no
//! script is injected into the page's main world.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::{BrowserError, BrowserPage};

/// Upstream `DEFAULT_ARGS` + `STEALTH_ARGS` (scrapling/engines/constants.py at the
/// pinned commit), used for every browser RustScrapling launches itself. All
/// launches use a fresh throwaway profile, so `--password-store=basic` never
/// weakens a persistent user profile.
pub const UPSTREAM_LAUNCH_ARGS: &[&str] = &[
    "--no-pings",
    "--no-first-run",
    "--disable-infobars",
    "--disable-breakpad",
    "--no-service-autorun",
    "--homepage=about:blank",
    "--password-store=basic",
    "--disable-hang-monitor",
    "--no-default-browser-check",
    "--disable-session-crashed-bubble",
    "--disable-search-engine-choice-screen",
    "--test-type",
    "--mute-audio",
    "--disable-sync",
    "--hide-scrollbars",
    "--disable-logging",
    "--start-maximized",
    "--enable-async-dns",
    "--use-mock-keychain",
    "--do-not-de-elevate",
    "--disable-translate",
    "--disable-voice-input",
    "--window-position=0,0",
    "--disable-wake-on-wifi",
    "--ignore-gpu-blocklist",
    "--enable-tcp-fast-open",
    "--enable-web-bluetooth",
    "--disable-cloud-import",
    "--disable-print-preview",
    "--disable-dev-shm-usage",
    "--metrics-recording-only",
    "--disable-crash-reporter",
    "--disable-partial-raster",
    "--disable-gesture-typing",
    "--disable-checker-imaging",
    "--disable-prompt-on-repost",
    "--force-color-profile=srgb",
    "--font-render-hinting=none",
    "--aggressive-cache-discard",
    "--disable-cookie-encryption",
    "--disable-domain-reliability",
    "--disable-threaded-animation",
    "--disable-threaded-scrolling",
    "--enable-simple-cache-backend",
    "--disable-background-networking",
    "--enable-surface-synchronization",
    "--disable-image-animation-resync",
    "--disable-renderer-backgrounding",
    "--disable-ipc-flooding-protection",
    "--prerender-from-omnibox=disabled",
    "--safebrowsing-disable-auto-update",
    "--disable-offer-upload-credit-cards",
    "--disable-background-timer-throttling",
    "--disable-new-content-rendering-timeout",
    "--run-all-compositor-stages-before-draw",
    "--disable-client-side-phishing-detection",
    "--disable-backgrounding-occluded-windows",
    "--disable-layer-tree-host-memory-pressure",
    "--autoplay-policy=user-gesture-required",
    "--disable-offer-store-unmasked-wallet-cards",
    "--disable-blink-features=AutomationControlled",
    "--disable-component-extensions-with-background-pages",
    "--enable-features=NetworkService,NetworkServiceInProcess,TrustTokens,TrustTokensAlwaysAllowIssuance",
    "--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4",
    "--disable-features=AudioServiceOutOfProcess,TranslateUI,BlinkGenPropertyTrees",
];

/// Upstream `disable_resources` types (font, image, media, beacon, object,
/// imageset, texttrack, websocket, csp_report, stylesheet) as CDP
/// `Network.ResourceType` names.
pub const DISABLED_RESOURCE_TYPES: [&str; 8] = [
    "Font",
    "Image",
    "Media",
    "Ping",
    "TextTrack",
    "WebSocket",
    "CSPViolationReport",
    "Stylesheet",
];

/// Session-level settings applied once to a fresh page.
#[derive(Clone, Debug, Default)]
pub struct PageSettings {
    pub locale: Option<String>,
    pub timezone_id: Option<String>,
    /// Explicit User-Agent; otherwise headless Chrome's UA has its
    /// `HeadlessChrome` token replaced, as upstream never exposes it.
    pub user_agent: Option<String>,
    pub headless: bool,
    /// Playwright-style cookie objects (`name`, `value`, `url` or
    /// `domain`/`path`, `expires`, `httpOnly`, `secure`, `sameSite`).
    pub cookies: Vec<Value>,
    /// Viewport and screen size (upstream uses 1920x1080).
    pub viewport: Option<(u32, u32)>,
}

impl BrowserPage {
    /// Apply locale/timezone/user-agent emulation and initial cookies.
    pub async fn apply_settings(&self, settings: &PageSettings) -> Result<(), BrowserError> {
        if let Some((width, height)) = settings.viewport {
            self.raw(
                "Emulation.setDeviceMetricsOverride",
                json!({"width": width, "height": height, "deviceScaleFactor": 1,
                    "mobile": false, "screenWidth": width, "screenHeight": height}),
            )
            .await?;
        }
        if let Some(timezone) = &settings.timezone_id {
            self.raw(
                "Emulation.setTimezoneOverride",
                json!({ "timezoneId": timezone }),
            )
            .await
            .map_err(|error| {
                BrowserError::InvalidArgument(format!("Invalid timezone ID '{timezone}': {error}"))
            })?;
        }
        if let Some(locale) = &settings.locale {
            self.raw("Emulation.setLocaleOverride", json!({ "locale": locale }))
                .await
                .map_err(|error| {
                    BrowserError::InvalidArgument(format!("Invalid locale '{locale}': {error}"))
                })?;
        }
        let user_agent = match &settings.user_agent {
            Some(agent) => Some(agent.clone()),
            None if settings.headless => {
                let version = self
                    .connection
                    .send("Browser.getVersion", json!({}), None)
                    .await?;
                version
                    .get("userAgent")
                    .and_then(Value::as_str)
                    .map(|agent| agent.replace("HeadlessChrome/", "Chrome/"))
            }
            None => None,
        };
        if user_agent.is_some() || settings.locale.is_some() {
            let agent = match user_agent {
                Some(agent) => agent,
                None => self
                    .connection
                    .send("Browser.getVersion", json!({}), None)
                    .await?
                    .get("userAgent")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            };
            let mut params = json!({ "userAgent": agent });
            if let Some(locale) = &settings.locale {
                params["acceptLanguage"] = locale.clone().into();
            }
            self.raw("Emulation.setUserAgentOverride", params).await?;
        }
        if !settings.cookies.is_empty() {
            self.set_cookies(&settings.cookies).await?;
        }
        Ok(())
    }

    /// Add cookies given as Playwright cookie dictionaries.
    pub async fn set_cookies(&self, cookies: &[Value]) -> Result<(), BrowserError> {
        let mut converted = Vec::with_capacity(cookies.len());
        for cookie in cookies {
            let object = cookie
                .as_object()
                .ok_or_else(|| BrowserError::InvalidArgument("cookies must be objects".into()))?;
            let text = |key: &str| object.get(key).and_then(Value::as_str);
            let (Some(name), Some(value)) = (text("name"), text("value")) else {
                return Err(BrowserError::InvalidArgument(
                    "Cookie should have a name and a value".into(),
                ));
            };
            if text("url").is_none() && text("domain").is_none() {
                return Err(BrowserError::InvalidArgument(
                    "Cookie should have a url or a domain/path pair".into(),
                ));
            }
            let mut entry = json!({ "name": name, "value": value });
            for key in ["url", "domain", "path"] {
                if let Some(value) = text(key) {
                    entry[key] = value.into();
                }
            }
            if let Some(expires) = object.get("expires").and_then(Value::as_f64) {
                if expires >= 0.0 {
                    entry["expires"] = json!(expires);
                }
            }
            for (from, to) in [("httpOnly", "httpOnly"), ("secure", "secure")] {
                if let Some(flag) = object.get(from).and_then(Value::as_bool) {
                    entry[to] = flag.into();
                }
            }
            if let Some(same_site) = text("sameSite") {
                if !matches!(same_site, "Strict" | "Lax" | "None") {
                    return Err(BrowserError::InvalidArgument(format!(
                        "Invalid sameSite '{same_site}'"
                    )));
                }
                entry["sameSite"] = same_site.into();
            }
            if let Some(key) = text("partitionKey") {
                entry["partitionKey"] = json!({"topLevelSite": key, "hasCrossSiteAncestor": false});
            }
            converted.push(entry);
        }
        self.raw("Network.setCookies", json!({ "cookies": converted }))
            .await?;
        Ok(())
    }

    /// Replace the extra request headers sent by this page (empty clears).
    pub async fn set_extra_headers(
        &self,
        headers: &BTreeMap<String, String>,
    ) -> Result<(), BrowserError> {
        for (name, value) in headers {
            if name.is_empty()
                || name.bytes().any(|b| b <= b' ' || b == b':' || b >= 0x7f)
                || value.bytes().any(|b| matches!(b, b'\r' | b'\n' | b'\0'))
            {
                return Err(BrowserError::InvalidArgument(format!(
                    "invalid extra header '{name}'"
                )));
            }
        }
        self.raw("Network.enable", json!({})).await?;
        self.raw("Network.setExtraHTTPHeaders", json!({ "headers": headers }))
            .await?;
        Ok(())
    }

    /// Block requests to these domains and their subdomains (and nothing
    /// else); an empty list removes the block.
    pub async fn set_blocked_domains(&self, domains: &[String]) -> Result<(), BrowserError> {
        let mut patterns = Vec::with_capacity(domains.len() * 2);
        for domain in domains {
            let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
            if domain.is_empty()
                || !domain
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            {
                return Err(BrowserError::InvalidArgument(format!(
                    "invalid blocked domain '{domain}'"
                )));
            }
            patterns.push(format!("*://{domain}/*"));
            patterns.push(format!("*://{domain}:*/*"));
            patterns.push(format!("*://*.{domain}/*"));
            patterns.push(format!("*://*.{domain}:*/*"));
        }
        self.raw("Network.enable", json!({})).await?;
        self.raw("Network.setBlockedURLs", json!({ "urls": patterns }))
            .await?;
        Ok(())
    }

    /// Fail requests of the given CDP resource types (upstream
    /// `disable_resources`) through `Fetch` interception of only those
    /// types; `false` stops intercepting.
    pub async fn set_resources_disabled(&self, disabled: bool) -> Result<(), BrowserError> {
        if disabled {
            // Pause every request and decide by its resource type in the page
            // monitor; type-specific Fetch filters differ between versions.
            self.monitor.lock().block_paused_requests = true;
            self.raw(
                "Fetch.enable",
                json!({"patterns": [{"urlPattern": "*", "requestStage": "Request"}]}),
            )
            .await?;
        } else if std::mem::take(&mut self.monitor.lock().block_paused_requests) {
            self.raw("Fetch.disable", json!({})).await?;
        }
        Ok(())
    }
}

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

/// Playwright `chromiumSwitches()` (patchright-core 1.64, the driver used by
/// the pinned upstream): upstream's browser process gets these *before*
/// Scrapling's own arguments. `--no-sandbox` (Playwright's default unless
/// `chromiumSandbox`) is deliberately not copied; see DECISIONS D-011.
pub const PLAYWRIGHT_SWITCHES: &[&str] = &[
    "--disable-field-trial-config",
    "--disable-background-networking",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-breakpad",
    "--no-default-browser-check",
    "--disable-dev-shm-usage",
    "--disable-edgeupdater",
    "--disable-features=AvoidUnnecessaryBeforeUnloadCheckSync,DestroyProfileOnBrowserClose,DialMediaRouteProvider,GlobalMediaControls,HttpsUpgrades,LensOverlay,PaintHolding,ThirdPartyStoragePartitioning,BlockOriginHeaderModificationOnRedirect,AvoidCorsURLLoaderRestartOnRedirect,Translate,AutoDeElevate,OptimizationHints,NetworkTimeServiceQuerying,AimEnabled,msForceBrowserSignIn,msEdgeUpdateLaunchServicesPreferredVersion",
    "--enable-features=CDPScreenshotNewSurface",
    "--disable-hang-monitor",
    "--disable-prompt-on-repost",
    "--disable-renderer-backgrounding",
    "--disable-updater-scheduler",
    "--force-color-profile=srgb",
    "--no-first-run",
    "--password-store=basic",
    "--use-mock-keychain",
    "--no-service-autorun",
    "--export-tagged-pdf",
    "--disable-search-engine-choice-screen",
    "--edge-skip-compat-layer-relaunch",
    "--disable-infobars",
    "--disable-sync",
    "--disable-blink-features=AutomationControlled",
];

/// Extra switches Playwright adds for headless launches.
pub const PLAYWRIGHT_HEADLESS_SWITCHES: &[&str] = &[
    "--hide-scrollbars",
    "--mute-audio",
    "--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4",
];

/// The argument list upstream's browser process receives: Playwright
/// switches, headless extras, then Scrapling's `DEFAULT_ARGS` +
/// `STEALTH_ARGS`. Feature lists are merged by [`merge_feature_switches`]
/// when the browser is launched.
pub fn upstream_launch_args(headless: bool) -> Vec<String> {
    let mut args: Vec<String> = PLAYWRIGHT_SWITCHES
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
    if headless {
        args.extend(PLAYWRIGHT_HEADLESS_SWITCHES.iter().map(|a| (*a).to_owned()));
    }
    args.extend(UPSTREAM_LAUNCH_ARGS.iter().map(|a| (*a).to_owned()));
    args
}

/// Chrome keeps only the last `--enable-features`/`--disable-features`
/// switch, so repeated lists (Playwright's and Scrapling's) are merged into
/// one switch each, preserving first-seen order and dropping duplicates.
/// Exact duplicate plain switches are dropped as well.
pub fn merge_feature_switches(args: &[String]) -> Vec<String> {
    const MERGED: [&str; 3] = [
        "--enable-features=",
        "--disable-features=",
        "--disable-blink-features=",
    ];
    let mut lists: Vec<Vec<String>> = vec![Vec::new(); MERGED.len()];
    let mut out: Vec<String> = Vec::new();
    let mut placed = [false; 3];
    for arg in args {
        if let Some(index) = MERGED.iter().position(|prefix| arg.starts_with(prefix)) {
            for feature in arg[MERGED[index].len()..].split(',') {
                let feature = feature.trim();
                if !feature.is_empty() && !lists[index].iter().any(|f| f == feature) {
                    lists[index].push(feature.to_owned());
                }
            }
            if !placed[index] {
                placed[index] = true;
                out.push(MERGED[index].to_owned());
            }
        } else if !out.contains(arg) {
            out.push(arg.clone());
        }
    }
    out.into_iter()
        .filter_map(
            |arg| match MERGED.iter().position(|prefix| arg == *prefix) {
                Some(index) if lists[index].is_empty() => None,
                Some(index) => Some(format!("{}{}", MERGED[index], lists[index].join(","))),
                None => Some(arg),
            },
        )
        .collect()
}

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

/// A browser identity whose parts really co-occur in the wild. The table is
/// fixed rather than assembled from random pieces: a user agent that disagrees
/// with `Sec-CH-UA`, the accept language or the timezone is itself a signal.
///
/// `sec_chua` is empty for engines that do not send Client Hints (Firefox,
/// Safari); `impersonate` names the matching `rustscrapling-http` profile so
/// the HTTP and browser tiers present the same identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fingerprint {
    pub user_agent: &'static str,
    pub sec_chua: &'static str,
    pub sec_chua_mobile: &'static str,
    pub sec_chua_platform: &'static str,
    pub accept_language: &'static str,
    pub locale: &'static str,
    pub timezone_id: &'static str,
    pub impersonate: &'static str,
}

pub const FINGERPRINTS: &[Fingerprint] = &[
    Fingerprint {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36",
        sec_chua: "\"Google Chrome\";v=\"149\", \"Chromium\";v=\"149\", \"Not_A Brand\";v=\"24\"",
        sec_chua_mobile: "?0",
        sec_chua_platform: "\"Windows\"",
        accept_language: "ja-JP,ja;q=0.9,en-US;q=0.8,en;q=0.7",
        locale: "ja-JP",
        timezone_id: "Asia/Tokyo",
        impersonate: "chrome149",
    },
    Fingerprint {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36",
        sec_chua: "\"Google Chrome\";v=\"149\", \"Chromium\";v=\"149\", \"Not_A Brand\";v=\"24\"",
        sec_chua_mobile: "?0",
        sec_chua_platform: "\"Windows\"",
        accept_language: "en-US,en;q=0.9",
        locale: "en-US",
        timezone_id: "America/New_York",
        impersonate: "chrome149",
    },
    Fingerprint {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36",
        sec_chua: "\"Google Chrome\";v=\"149\", \"Chromium\";v=\"149\", \"Not_A Brand\";v=\"24\"",
        sec_chua_mobile: "?0",
        sec_chua_platform: "\"macOS\"",
        accept_language: "en-GB,en;q=0.9",
        locale: "en-GB",
        timezone_id: "Europe/London",
        impersonate: "chrome149",
    },
    Fingerprint {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36",
        sec_chua: "\"Google Chrome\";v=\"149\", \"Chromium\";v=\"149\", \"Not_A Brand\";v=\"24\"",
        sec_chua_mobile: "?0",
        sec_chua_platform: "\"macOS\"",
        accept_language: "ja-JP,ja;q=0.9",
        locale: "ja-JP",
        timezone_id: "Asia/Tokyo",
        impersonate: "chrome149",
    },
    Fingerprint {
        user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36",
        sec_chua: "\"Google Chrome\";v=\"149\", \"Chromium\";v=\"149\", \"Not_A Brand\";v=\"24\"",
        sec_chua_mobile: "?0",
        sec_chua_platform: "\"Linux\"",
        accept_language: "en-US,en;q=0.9",
        locale: "en-US",
        timezone_id: "Etc/UTC",
        impersonate: "chrome149",
    },
    Fingerprint {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:151.0) Gecko/20100101 Firefox/151.0",
        sec_chua: "",
        sec_chua_mobile: "",
        sec_chua_platform: "",
        accept_language: "ja-JP,ja;q=0.8,en-US;q=0.5,en;q=0.3",
        locale: "ja-JP",
        timezone_id: "Asia/Tokyo",
        impersonate: "firefox151",
    },
    Fingerprint {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:151.0) Gecko/20100101 Firefox/151.0",
        sec_chua: "",
        sec_chua_mobile: "",
        sec_chua_platform: "",
        accept_language: "en-US,en;q=0.5",
        locale: "en-US",
        timezone_id: "America/Chicago",
        impersonate: "firefox151",
    },
    Fingerprint {
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
        sec_chua: "",
        sec_chua_mobile: "",
        sec_chua_platform: "",
        accept_language: "en-US,en;q=0.9",
        locale: "en-US",
        timezone_id: "America/Los_Angeles",
        impersonate: "safari26",
    },
];

/// Deterministic selection: the same seed always yields the same identity, so a
/// retry presents the same browser instead of flipping fingerprints mid-session.
pub fn fingerprint_for(seed: u64) -> &'static Fingerprint {
    &FINGERPRINTS[(seed % FINGERPRINTS.len() as u64) as usize]
}

impl Fingerprint {
    /// The HTTP headers this identity must send, so the request tier and the
    /// browser tier never disagree. Empty Client Hints are omitted, matching
    /// engines that do not send them.
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![
            ("user-agent".to_owned(), self.user_agent.to_owned()),
            (
                "accept-language".to_owned(),
                self.accept_language.to_owned(),
            ),
        ];
        for (name, value) in [
            ("sec-ch-ua", self.sec_chua),
            ("sec-ch-ua-mobile", self.sec_chua_mobile),
            ("sec-ch-ua-platform", self.sec_chua_platform),
        ] {
            if !value.is_empty() {
                headers.push((name.to_owned(), value.to_owned()));
            }
        }
        headers
    }

    /// The page settings that make the browser agree with this identity.
    pub fn page_settings(&self) -> PageSettings {
        PageSettings {
            locale: Some(self.locale.to_owned()),
            timezone_id: Some(self.timezone_id.to_owned()),
            user_agent: Some(self.user_agent.to_owned()),
            ..PageSettings::default()
        }
    }
}

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

#[cfg(test)]
mod launch_arg_tests {
    use super::*;

    #[test]
    fn feature_lists_are_merged_into_one_switch_each() {
        let args = upstream_launch_args(true);
        let merged = merge_feature_switches(&args);
        let disable: Vec<&String> = merged
            .iter()
            .filter(|a| a.starts_with("--disable-features="))
            .collect();
        assert_eq!(disable.len(), 1);
        for feature in [
            "PaintHolding",
            "Translate",
            "AudioServiceOutOfProcess",
            "TranslateUI",
        ] {
            assert!(
                disable[0].split(['=', ',']).any(|f| f == feature),
                "{feature} missing: {}",
                disable[0]
            );
        }
        let enable: Vec<&String> = merged
            .iter()
            .filter(|a| a.starts_with("--enable-features="))
            .collect();
        assert_eq!(enable.len(), 1);
        assert!(
            enable[0].contains("CDPScreenshotNewSurface") && enable[0].contains("NetworkService,")
        );
        assert_eq!(
            merged
                .iter()
                .filter(|a| a.as_str() == "--disable-blink-features=AutomationControlled")
                .count(),
            1
        );
        assert_eq!(
            merged
                .iter()
                .filter(|a| a.as_str() == "--no-first-run")
                .count(),
            1
        );
        assert!(!merged.iter().any(|a| a == "--no-sandbox"));
        assert!(!upstream_launch_args(false).contains(&"--blink-settings=primaryHoverType=2,availableHoverTypes=2,primaryPointerType=4,availablePointerTypes=4".to_owned())
            || UPSTREAM_LAUNCH_ARGS.iter().any(|a| a.starts_with("--blink-settings=")));
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::{fingerprint_for, FINGERPRINTS};

    /// Region each locale may legitimately claim, so a user agent is never
    /// paired with a timezone no real user in that locale would have.
    fn allowed_regions(locale: &str) -> &'static [&'static str] {
        match locale {
            "ja-JP" => &["Asia/"],
            "en-GB" => &["Europe/"],
            "en-US" => &["America/", "Etc/UTC"],
            other => panic!("locale {other} has no region rule"),
        }
    }

    #[test]
    fn client_hints_only_appear_for_engines_that_send_them() {
        for entry in FINGERPRINTS {
            let chromium = entry.user_agent.contains("Chrome/");
            assert_eq!(
                !entry.sec_chua.is_empty(),
                chromium,
                "{} carries the wrong Client Hints",
                entry.user_agent
            );
            if chromium {
                let major = entry
                    .user_agent
                    .split("Chrome/")
                    .nth(1)
                    .and_then(|rest| rest.split('.').next())
                    .expect("a Chrome user agent has a version");
                assert!(
                    entry.sec_chua.contains(&format!("v=\"{major}\"")),
                    "Sec-CH-UA disagrees with the user agent version: {}",
                    entry.sec_chua
                );
            }
        }
    }

    #[test]
    fn accept_language_agrees_with_the_locale() {
        for entry in FINGERPRINTS {
            let primary = entry
                .accept_language
                .split(',')
                .next()
                .expect("an accept language has one entry");
            assert_eq!(
                primary, entry.locale,
                "accept language {primary} does not start with locale {}",
                entry.locale
            );
        }
    }

    #[test]
    fn the_timezone_matches_the_locale_region() {
        for entry in FINGERPRINTS {
            assert!(
                allowed_regions(entry.locale)
                    .iter()
                    .any(|prefix| entry.timezone_id.starts_with(prefix)),
                "{} is not a plausible timezone for {}",
                entry.timezone_id,
                entry.locale
            );
        }
    }

    #[test]
    fn page_settings_repeat_the_same_identity() {
        for entry in FINGERPRINTS {
            let settings = entry.page_settings();
            assert_eq!(settings.user_agent.as_deref(), Some(entry.user_agent));
            assert_eq!(settings.locale.as_deref(), Some(entry.locale));
            assert_eq!(settings.timezone_id.as_deref(), Some(entry.timezone_id));
        }
    }

    #[test]
    fn headers_omit_empty_client_hints() {
        let chromium = FINGERPRINTS[0].headers();
        assert!(chromium.iter().any(|(name, _)| name == "sec-ch-ua"));
        let firefox = FINGERPRINTS
            .iter()
            .find(|entry| entry.impersonate == "firefox151")
            .expect("a firefox identity");
        assert!(!firefox
            .headers()
            .iter()
            .any(|(name, _)| name.starts_with("sec-ch-ua")));
    }

    #[test]
    fn selection_is_deterministic_and_wraps() {
        assert_eq!(fingerprint_for(7), fingerprint_for(7));
        assert_eq!(fingerprint_for(0), &FINGERPRINTS[0]);
        assert_eq!(
            fingerprint_for(FINGERPRINTS.len() as u64),
            &FINGERPRINTS[0],
            "the index wraps instead of running past the table"
        );
        let last = fingerprint_for(FINGERPRINTS.len() as u64 - 1);
        assert_eq!(last, &FINGERPRINTS[FINGERPRINTS.len() - 1]);
    }
}

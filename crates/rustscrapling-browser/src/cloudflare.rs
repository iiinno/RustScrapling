//! Cloudflare interstitial and Turnstile solving, ported from upstream
//! `_cloudflare_solver`. Written against `BrowserPage` so both the MCP executor
//! and the CLI acquisition tier run the same solver.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::time::sleep;

use crate::{
    BrowserAction, BrowserError, BrowserPage, LoadState, MouseButton, PointerTarget,
};

/// Upstream `_detect_cloudflare`: the flavour decides which solve path runs, so
/// it is read from the document rather than from the status code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloudflareChallenge {
    NonInteractive,
    Managed,
    Interactive,
    Embedded,
}

impl CloudflareChallenge {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonInteractive => "non-interactive",
            Self::Managed => "managed",
            Self::Interactive => "interactive",
            Self::Embedded => "embedded",
        }
    }
}

const MAX_ATTEMPTS: u32 = 2;

pub fn detect_cloudflare(html: &str) -> Option<CloudflareChallenge> {
    for (needle, challenge) in [
        (
            "cType: 'non-interactive'",
            CloudflareChallenge::NonInteractive,
        ),
        ("cType: 'managed'", CloudflareChallenge::Managed),
        ("cType: 'interactive'", CloudflareChallenge::Interactive),
    ] {
        if html.contains(needle) {
            return Some(challenge);
        }
    }
    // The generic markers must match `detect_block`, or a page is reported as
    // blocked and then judged to have no challenge, so the solver returns
    // immediately and the caller still sees the interstitial.
    let lowered = html.to_ascii_lowercase();
    if lowered.contains("challenges.cloudflare.com/turnstile/v")
        || lowered.contains("challenge-platform")
        || lowered.contains("cf-chl-")
    {
        return Some(CloudflareChallenge::Embedded);
    }
    None
}

/// Solve whatever challenge the page currently shows. `Ok` means no challenge
/// remains; a challenge that survives the retries is a typed error, so a
/// challenge page can never be reported as a successful fetch.
pub async fn solve_cloudflare(page: &BrowserPage, timeout_ms: f64) -> Result<(), BrowserError> {
    let mut attempt = 0;
    loop {
        let html = page.source_with_shadow(false).await?;
        let Some(challenge) = detect_cloudflare(&html) else {
            return Ok(());
        };
        if attempt >= MAX_ATTEMPTS {
            return Err(BrowserError::Blocked(format!(
                "Cloudflare {} challenge was not cleared",
                challenge.as_str()
            )));
        }
        attempt += 1;
        if challenge == CloudflareChallenge::NonInteractive {
            wait_for_challenge_clear(page).await?;
            continue;
        }
        match widget_corner(page, challenge).await? {
            Some((x, y)) => {
                // Upstream presses 26-28 px right and 25-27 px below the corner.
                let point = PointerTarget::Point {
                    x: x + jitter(26.0),
                    y: y + jitter(25.0),
                };
                let _ = page
                    .run_actions(
                        &[BrowserAction::Click {
                            target: point,
                            timeout_ms,
                            button: MouseButton::Left,
                            click_count: 1,
                            delay_ms: 150.0,
                        }],
                        false,
                    )
                    .await;
                let _ = page
                    .run_actions(
                        &[BrowserAction::WaitLoad {
                            state: LoadState::NetworkIdle,
                            timeout_ms: 5_000.0,
                        }],
                        false,
                    )
                    .await;
            }
            None => sleep(Duration::from_millis(1_000)).await,
        }
        wait_for_challenge_clear(page).await?;
    }
}

/// Ten seconds of 100 ms polls, matching upstream's clearance wait.
async fn wait_for_challenge_clear(page: &BrowserPage) -> Result<(), BrowserError> {
    for _ in 0..100 {
        if detect_cloudflare(&page.source_with_shadow(false).await?).is_none() {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

/// Upstream jitters each click by 0-2 px so repeated solves are not identical.
fn jitter(base: f64) -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    base + f64::from(nanos % 3)
}

/// Top-left corner of the clickable widget in viewport pixels. Turnstile lives in
/// a cross-origin iframe, so its box is measured from the top document, exactly
/// as upstream's `frame_element().bounding_box()` does.
async fn widget_corner(
    page: &BrowserPage,
    challenge: CloudflareChallenge,
) -> Result<Option<(f64, f64)>, BrowserError> {
    const SCRIPT: &str = r#"function(options) {
        const corner = (element) => {
            if (!element) return null;
            const rect = element.getBoundingClientRect();
            if (!rect.width || !rect.height) return null;
            return { x: rect.x, y: rect.y };
        };
        const frames = [...document.querySelectorAll('iframe')];
        const turnstile = frames.find((frame) =>
            (frame.src || '').includes('challenges.cloudflare.com/cdn-cgi/challenge-platform/'));
        const measured = corner(turnstile);
        if (measured) return measured;
        if (options.embedded) return null;
        const boxes = document.querySelectorAll('.main-content p+div>div>div');
        return corner(boxes[boxes.length - 1]);
    }"#;
    let argument = json!({ "embedded": challenge == CloudflareChallenge::Embedded });
    let measured = page
        .evaluate_expression(SCRIPT, Some(&argument), true)
        .await?;
    let found = measured
        .get("x")
        .and_then(Value::as_f64)
        .zip(measured.get("y").and_then(Value::as_f64));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::{detect_cloudflare, CloudflareChallenge};

    #[test]
    fn upstream_ctypes_select_the_matching_solve_path() {
        assert_eq!(
            detect_cloudflare("<script>{cType: 'non-interactive'}</script>"),
            Some(CloudflareChallenge::NonInteractive)
        );
        assert_eq!(
            detect_cloudflare("<script>{cType: 'managed'}</script>"),
            Some(CloudflareChallenge::Managed)
        );
        assert_eq!(
            detect_cloudflare("<script>{cType: 'interactive'}</script>"),
            Some(CloudflareChallenge::Interactive)
        );
    }

    #[test]
    fn a_turnstile_script_marks_an_embedded_challenge() {
        assert_eq!(
            detect_cloudflare(
                r#"<script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>"#
            ),
            Some(CloudflareChallenge::Embedded)
        );
    }

    #[test]
    fn generic_cloudflare_markers_are_recognised() {
        // nowsecure.nl ships its own Turnstile page without a cType field; the
        // solver must still treat it as a challenge or it returns immediately.
        assert_eq!(
            detect_cloudflare(
                r#"<script src="https://challenges.cloudflare.com/cdn-cgi/challenge-platform/x.js"></script>"#
            ),
            Some(CloudflareChallenge::Embedded)
        );
        assert_eq!(
            detect_cloudflare(r#"<div id="cf-chl-widget-abc"></div>"#),
            Some(CloudflareChallenge::Embedded)
        );
    }

    #[test]
    fn an_ordinary_page_is_not_a_challenge() {
        assert_eq!(detect_cloudflare("<html><body><h1>article</h1></body></html>"), None);
    }

    #[test]
    fn the_ctypes_win_over_the_turnstile_script() {
        assert_eq!(
            detect_cloudflare(
                r#"<script>{cType: 'interactive'}</script><script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>"#
            ),
            Some(CloudflareChallenge::Interactive)
        );
    }
}

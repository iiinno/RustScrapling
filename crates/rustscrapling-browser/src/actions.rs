//! Browser actions with Playwright-compatible semantics for the pinned
//! Scrapling v0.5 MCP `browser_actions` contract (12 action variants).
//!
//! Element lookup and actionability checks run in a dedicated isolated world
//! (`Page.createIsolatedWorld`), so `Runtime.enable` is never sent and page
//! scripts cannot see the helper functions. Input is delivered through the
//! CDP `Input` domain, i.e. as trusted browser events.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::keys::{self, KeyDescription};
use crate::transport::Connection;
use crate::{BrowserError, BrowserPage};

const INJECTED: &str = include_str!("injected.js");
const WORLD_NAME: &str = "__rustscrapling_utility__";
const OBJECT_GROUP: &str = "rustscrapling-actions";
/// Retry backoff used while waiting for actionability, in milliseconds.
const BACKOFF_MS: [u64; 5] = [0, 20, 100, 100, 500];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    fn cdp(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
        }
    }

    fn mask(self) -> u32 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementState {
    Attached,
    Detached,
    Visible,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    DomContentLoaded,
    Load,
    NetworkIdle,
}

impl LoadState {
    fn lifecycle_name(self) -> &'static str {
        match self {
            Self::DomContentLoaded => "DOMContentLoaded",
            Self::Load => "load",
            Self::NetworkIdle => "networkIdle",
        }
    }
}

/// An element selector (Playwright syntax or `aria-ref=<ref>`) or a
/// viewport point in CSS pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum PointerTarget {
    Selector(String),
    Point { x: f64, y: f64 },
}

/// One upstream `BrowserAction` variant. Timeouts are milliseconds; `0`
/// disables the timeout, matching the upstream schema.
#[derive(Clone, Debug, PartialEq)]
pub enum BrowserAction {
    Move {
        target: PointerTarget,
        timeout_ms: f64,
        steps: u32,
    },
    Click {
        target: PointerTarget,
        timeout_ms: f64,
        button: MouseButton,
        click_count: u32,
        delay_ms: f64,
    },
    Wheel {
        delta_x: f64,
        delta_y: f64,
    },
    Textbox {
        target: String,
        value: String,
        clear: bool,
        timeout_ms: f64,
    },
    Checkbox {
        target: String,
        checked: bool,
        timeout_ms: f64,
    },
    Radio {
        target: String,
        timeout_ms: f64,
    },
    /// Select options by label; an empty list clears the selection.
    Combobox {
        target: String,
        labels: Vec<String>,
        timeout_ms: f64,
    },
    PressKey {
        key: String,
    },
    Dialog {
        accept: bool,
        prompt_text: Option<String>,
    },
    WaitTime {
        milliseconds: f64,
    },
    WaitElement {
        target: String,
        state: ElementState,
        timeout_ms: f64,
    },
    WaitLoad {
        state: LoadState,
        timeout_ms: f64,
    },
}

impl BrowserAction {
    /// Whether the action resolves a page element (and remote objects).
    fn uses_element(&self) -> bool {
        match self {
            Self::Move { target, .. } | Self::Click { target, .. } => {
                matches!(target, PointerTarget::Selector(_))
            }
            Self::Textbox { .. }
            | Self::Checkbox { .. }
            | Self::Radio { .. }
            | Self::Combobox { .. }
            | Self::WaitElement { .. } => true,
            Self::Wheel { .. }
            | Self::PressKey { .. }
            | Self::Dialog { .. }
            | Self::WaitTime { .. }
            | Self::WaitLoad { .. } => false,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Move { .. } => "move",
            Self::Click { .. } => "click",
            Self::Wheel { .. } => "wheel",
            Self::Textbox { .. } => "textbox",
            Self::Checkbox { .. } => "checkbox",
            Self::Radio { .. } => "radio",
            Self::Combobox { .. } => "combobox",
            Self::PressKey { .. } => "press_key",
            Self::Dialog { .. } => "dialog",
            Self::WaitTime { .. } => "wait_time",
            Self::WaitElement { .. } => "wait_element",
            Self::WaitLoad { .. } => "wait_load",
        }
    }
}

/// A failed action in a chain. Effects of earlier actions remain.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionError {
    /// 1-based index into the action list.
    pub index: usize,
    pub kind: &'static str,
    pub message: String,
    pub timed_out: bool,
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Action {} ({}) failed: {}",
            self.index, self.kind, self.message
        )
    }
}

impl std::error::Error for ActionError {}

#[derive(Debug, Clone)]
struct DialogReply {
    index: usize,
    accept: bool,
    prompt_text: Option<String>,
}

#[derive(Default)]
pub(crate) struct MonitorState {
    loader_id: Option<String>,
    lifecycle: HashSet<String>,
    dialog_replies: VecDeque<DialogReply>,
    dialog_error: Option<(usize, String)>,
    mouse: (f64, f64),
    buttons: u32,
    modifiers: u32,
    /// Utility world keyed by the document loader it was created for.
    world: Option<(Option<String>, i64)>,
}

pub(crate) struct PageMonitor {
    pub(crate) state: StdMutex<MonitorState>,
    changed: Notify,
}

impl PageMonitor {
    fn lock(&self) -> std::sync::MutexGuard<'_, MonitorState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Aborts the page monitor when the owning page is dropped.
pub(crate) struct MonitorGuard(tokio::task::AbortHandle);

impl Drop for MonitorGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Track main-frame lifecycle events and answer JavaScript dialogs in the
/// background. A dialog without a queued `dialog` action is dismissed, which
/// is the Playwright default the upstream server inherits; this also keeps a
/// blocking `alert()` from wedging subsequent CDP input commands.
pub(crate) fn spawn_monitor(
    connection: &Arc<Connection>,
    session_id: String,
    main_frame: String,
) -> (Arc<PageMonitor>, MonitorGuard) {
    let monitor = Arc::new(PageMonitor {
        state: StdMutex::new(MonitorState::default()),
        changed: Notify::new(),
    });
    let mut events = connection.subscribe();
    let weak = Arc::downgrade(connection);
    let shared = Arc::clone(&monitor);
    let task = tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            if event.session_id.as_deref() != Some(session_id.as_str()) {
                continue;
            }
            match event.method.as_str() {
                "Page.lifecycleEvent" => {
                    let frame = event.params.get("frameId").and_then(Value::as_str);
                    if frame != Some(main_frame.as_str()) {
                        continue;
                    }
                    let loader = event
                        .params
                        .get("loaderId")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let name = event
                        .params
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let mut state = shared.lock();
                    if name == "init" || state.loader_id != loader {
                        state.loader_id = loader;
                        state.lifecycle.clear();
                    }
                    if name != "init" {
                        state.lifecycle.insert(name.to_owned());
                    }
                    drop(state);
                    shared.changed.notify_waiters();
                }
                "Page.frameNavigated" => {
                    if event.params.pointer("/frame/parentId").is_none() {
                        let mut state = shared.lock();
                        state.world = None;
                        let loader = event
                            .params
                            .pointer("/frame/loaderId")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        if loader.is_some() && state.loader_id != loader {
                            state.loader_id = loader;
                            state.lifecycle.clear();
                        }
                        drop(state);
                        shared.changed.notify_waiters();
                    }
                }
                "Page.javascriptDialogOpening" => {
                    let reply = shared.lock().dialog_replies.pop_front();
                    let Some(connection) = weak.upgrade() else {
                        break;
                    };
                    let session = session_id.clone();
                    let shared = Arc::clone(&shared);
                    // Answer from a separate task: the connection reader must
                    // keep delivering the response while we wait for it.
                    tokio::spawn(async move {
                        let (accept, prompt, index) = match &reply {
                            Some(reply) => (reply.accept, reply.prompt_text.clone(), reply.index),
                            None => (false, None, 0),
                        };
                        let mut params = json!({ "accept": accept });
                        if let Some(prompt) = prompt {
                            params["promptText"] = prompt.into();
                        }
                        let result = connection
                            .send("Page.handleJavaScriptDialog", params, Some(&session))
                            .await;
                        if let (Err(error), true) = (result, index > 0) {
                            let mut state = shared.lock();
                            state.dialog_error.get_or_insert((index, error.to_string()));
                        }
                        shared.changed.notify_waiters();
                    });
                }
                "Inspector.detached" | "Target.detachedFromTarget" => break,
                _ => {}
            }
        }
    });
    (monitor, MonitorGuard(task.abort_handle()))
}

/// Outcome of one attempt in a retry loop.
enum Attempt {
    /// Not actionable yet; wait and retry. The text describes the state.
    Retry(String),
    /// A non-recoverable error.
    Fail(BrowserError),
}

impl From<BrowserError> for Attempt {
    fn from(error: BrowserError) -> Self {
        if is_context_gone(&error) {
            Attempt::Retry("execution context was destroyed".into())
        } else {
            Attempt::Fail(error)
        }
    }
}

fn is_context_gone(error: &BrowserError) -> bool {
    let text = error.to_string();
    text.contains("Cannot find context")
        || text.contains("Execution context was destroyed")
        || text.contains("Cannot find object with id")
        || text.contains("Could not find node with given id")
        || text.contains("No node with given id")
        || text.contains("Node is detached")
}

struct Waiter {
    deadline: Option<Instant>,
    timeout_ms: f64,
    attempt: usize,
    /// The most recent reason the target was not actionable.
    last: Option<String>,
}

impl Waiter {
    fn new(timeout_ms: f64) -> Self {
        Self {
            deadline: deadline(timeout_ms),
            timeout_ms,
            attempt: 0,
            last: None,
        }
    }

    fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    fn timeout_error(&self, reason: &str) -> BrowserError {
        let reason = self.last.as_deref().unwrap_or(reason);
        BrowserError::Timeout(format!(
            "Timeout {}ms exceeded; {reason}",
            format_ms(self.timeout_ms)
        ))
    }

    /// Sleep before the next attempt, or fail when the deadline has passed.
    async fn retry(&mut self, reason: &str) -> Result<(), BrowserError> {
        let delay = Duration::from_millis(BACKOFF_MS[self.attempt.min(BACKOFF_MS.len() - 1)]);
        self.attempt += 1;
        self.last = Some(reason.to_owned());
        if let Some(deadline) = self.deadline {
            let now = Instant::now();
            if now >= deadline {
                return Err(self.timeout_error(reason));
            }
            tokio::time::sleep_until((now + delay).min(deadline)).await;
        } else {
            tokio::time::sleep(delay).await;
        }
        Ok(())
    }

    /// Bound a single CDP step by the action deadline.
    async fn bound<T>(
        &self,
        reason: &str,
        future: impl std::future::Future<Output = Result<T, BrowserError>>,
    ) -> Result<T, BrowserError> {
        match self.remaining() {
            Some(remaining) => tokio::time::timeout(remaining, future)
                .await
                .map_err(|_| self.timeout_error(reason))?,
            None => future.await,
        }
    }
}

fn deadline(timeout_ms: f64) -> Option<Instant> {
    if timeout_ms <= 0.0 || !timeout_ms.is_finite() {
        return None;
    }
    let duration = Duration::try_from_secs_f64(timeout_ms / 1000.0).ok()?;
    Instant::now().checked_add(duration)
}

fn format_ms(ms: f64) -> String {
    if ms.fract() == 0.0 && ms.abs() < 1e15 {
        format!("{}", ms as i64)
    } else {
        format!("{ms}")
    }
}

fn millis(ms: f64) -> Duration {
    Duration::try_from_secs_f64(ms.max(0.0) / 1000.0).unwrap_or(Duration::MAX)
}

/// Small non-cryptographic generator for human-like `slowly` delays.
struct Jitter(u64);

impl Jitter {
    fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        Self(seed | 1)
    }

    fn between(&mut self, low: f64, high: f64) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        low + (high - low) * ((self.0 >> 11) as f64 / (1u64 << 53) as f64)
    }
}

fn js_error(details: &Value) -> String {
    let description = details
        .pointer("/exception/description")
        .and_then(Value::as_str)
        .or_else(|| details.pointer("/exception/value").and_then(Value::as_str))
        .or_else(|| details.get("text").and_then(Value::as_str))
        .unwrap_or("JavaScript error");
    let first = description.lines().next().unwrap_or(description);
    first.strip_prefix("Error: ").unwrap_or(first).to_owned()
}

impl BrowserPage {
    async fn utility_world(&self) -> Result<i64, BrowserError> {
        let loader = {
            let state = self.monitor.lock();
            if let Some((loader, id)) = &state.world {
                if *loader == state.loader_id {
                    return Ok(*id);
                }
            }
            state.loader_id.clone()
        };
        let created = self
            .raw(
                "Page.createIsolatedWorld",
                json!({"frameId": self.main_frame, "worldName": WORLD_NAME,
                    "grantUniveralAccess": true}),
            )
            .await?;
        let id = created
            .get("executionContextId")
            .and_then(Value::as_i64)
            .ok_or_else(|| BrowserError::Protocol("isolated world has no context id".into()))?;
        let installed = self
            .raw(
                "Runtime.evaluate",
                json!({"expression": INJECTED, "contextId": id, "returnByValue": true}),
            )
            .await?;
        if let Some(details) = installed.get("exceptionDetails") {
            return Err(BrowserError::Protocol(format!(
                "utility script failed: {}",
                js_error(details)
            )));
        }
        self.monitor.lock().world = Some((loader, id));
        Ok(id)
    }

    fn forget_world(&self) {
        self.monitor.lock().world = None;
    }

    /// Call `function` in the utility world; `this` is unset.
    async fn world_call(
        &self,
        function: &str,
        args: &[Value],
        by_value: bool,
    ) -> Result<Value, Attempt> {
        let context = self.utility_world().await?;
        let arguments: Vec<Value> = args.iter().map(|value| json!({"value": value})).collect();
        let result = self
            .raw(
                "Runtime.callFunctionOn",
                json!({"functionDeclaration": function, "executionContextId": context,
                    "arguments": arguments, "returnByValue": by_value, "awaitPromise": true,
                    "objectGroup": OBJECT_GROUP}),
            )
            .await;
        self.finish_call(result)
    }

    /// Call `__rsx.<method>(element, ...args)` for a resolved element.
    async fn element_call(
        &self,
        object_id: &str,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Attempt> {
        let arguments: Vec<Value> = args.iter().map(|value| json!({"value": value})).collect();
        let function =
            format!("function(...args) {{ return globalThis.__rsx.{method}(this, ...args); }}");
        let result = self
            .raw(
                "Runtime.callFunctionOn",
                json!({"functionDeclaration": function, "objectId": object_id,
                    "arguments": arguments, "returnByValue": true, "awaitPromise": true}),
            )
            .await;
        self.finish_call(result)
    }

    fn finish_call(&self, result: Result<Value, BrowserError>) -> Result<Value, Attempt> {
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                if is_context_gone(&error) {
                    self.forget_world();
                }
                return Err(error.into());
            }
        };
        if let Some(details) = value.get("exceptionDetails") {
            let message = js_error(details);
            if message.contains("Execution context was destroyed") {
                self.forget_world();
                return Err(Attempt::Retry(message));
            }
            return Err(Attempt::Fail(BrowserError::InvalidArgument(message)));
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn release_objects(&self) {
        let _ = self
            .raw(
                "Runtime.releaseObjectGroup",
                json!({"objectGroup": OBJECT_GROUP}),
            )
            .await;
    }

    /// Resolve a strict single-element selector. `Ok(None)` means no match.
    async fn resolve(&self, selector: &str, retarget: bool) -> Result<Option<String>, Attempt> {
        let function = r#"function(selector, retarget) {
            const found = globalThis.__rsx.query(selector);
            if (found.length > 1) {
                const shown = found.slice(0, 10).map((e, i) => `    ${i + 1}) ${globalThis.__rsx.preview(e)}`).join('\n');
                throw new Error(`strict mode violation: locator(${JSON.stringify(selector)}) resolved to ${found.length} elements:\n${shown}`);
            }
            if (!found.length) return null;
            return retarget ? globalThis.__rsx.retarget(found[0], 'follow-label') : found[0];
        }"#;
        let result = self
            .world_call(function, &[json!(selector), json!(retarget)], false)
            .await?;
        Ok(result
            .get("objectId")
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    async fn count(&self, selector: &str) -> Result<(usize, bool), Attempt> {
        let function = r#"function(selector) {
            const found = globalThis.__rsx.query(selector);
            return [found.length, found.length === 1 && globalThis.__rsx.isVisible(found[0])];
        }"#;
        let result = self.world_call(function, &[json!(selector)], true).await?;
        let pair = result.get("value").and_then(Value::as_array);
        let count = pair
            .and_then(|pair| pair.first())
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let visible = pair
            .and_then(|pair| pair.get(1))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok((count, visible))
    }

    async fn element_states(&self, object_id: &str, states: &[&str]) -> Result<(), Attempt> {
        let result = self
            .element_call(object_id, "check", &[json!(states)])
            .await?;
        match result.get("value").and_then(Value::as_str) {
            Some("ok") => Ok(()),
            Some(reason) => Err(Attempt::Retry(reason.to_owned())),
            None => Err(Attempt::Fail(BrowserError::Protocol(
                "element state unavailable".into(),
            ))),
        }
    }

    /// Scroll an element into view and return a hit-tested click point.
    async fn click_point(&self, object_id: &str) -> Result<(f64, f64), Attempt> {
        let stable = self.element_call(object_id, "stableRect", &[]).await?;
        if stable.get("value") != Some(&Value::Bool(true)) {
            return Err(Attempt::Retry("element is not stable".into()));
        }
        self.raw(
            "DOM.scrollIntoViewIfNeeded",
            json!({ "objectId": object_id }),
        )
        .await?;
        let quads = self
            .raw("DOM.getContentQuads", json!({ "objectId": object_id }))
            .await?;
        let metrics = self.raw("Page.getLayoutMetrics", json!({})).await?;
        let width = metrics
            .pointer("/cssLayoutViewport/clientWidth")
            .and_then(Value::as_f64)
            .unwrap_or(f64::MAX);
        let height = metrics
            .pointer("/cssLayoutViewport/clientHeight")
            .and_then(Value::as_f64)
            .unwrap_or(f64::MAX);
        let mut point = None;
        for quad in quads
            .get("quads")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(values) = quad.as_array().filter(|values| values.len() == 8) else {
                continue;
            };
            let coords: Vec<f64> = values.iter().filter_map(Value::as_f64).collect();
            if coords.len() != 8 || coords.iter().any(|value| !value.is_finite()) {
                continue;
            }
            // Clip the quad's bounding box to the viewport and take its centre.
            let xs = [coords[0], coords[2], coords[4], coords[6]];
            let ys = [coords[1], coords[3], coords[5], coords[7]];
            let left = xs.iter().copied().fold(f64::INFINITY, f64::min).max(0.0);
            let right = xs
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max)
                .min(width);
            let top = ys.iter().copied().fold(f64::INFINITY, f64::min).max(0.0);
            let bottom = ys
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max)
                .min(height);
            if right - left >= 1.0 && bottom - top >= 1.0 {
                point = Some(((left + right) / 2.0, (top + bottom) / 2.0));
                break;
            }
        }
        let Some((x, y)) = point else {
            return Err(Attempt::Retry("element is outside of the viewport".into()));
        };
        let hit = self
            .element_call(object_id, "hitTarget", &[json!(x), json!(y)])
            .await?;
        match hit.get("value").and_then(Value::as_str) {
            Some("ok") => Ok((x, y)),
            Some(reason) => Err(Attempt::Retry(reason.to_owned())),
            None => Err(Attempt::Fail(BrowserError::Protocol(
                "hit target check failed".into(),
            ))),
        }
    }

    /// Wait until `selector` resolves to an element passing `states`, then
    /// return its remote object id. Used by every element action.
    async fn actionable(
        &self,
        waiter: &mut Waiter,
        selector: &str,
        retarget: bool,
        states: &[&str],
    ) -> Result<String, BrowserError> {
        loop {
            let attempt = async {
                let Some(object) = self.resolve(selector, retarget).await? else {
                    return Err(Attempt::Retry(format!("waiting for locator({selector:?})")));
                };
                self.element_states(&object, states).await?;
                Ok(object)
            };
            match waiter
                .bound("waiting for element", attempt_result(attempt))
                .await?
            {
                Ok(object) => return Ok(object),
                Err(reason) => waiter.retry(&reason).await?,
            }
        }
    }

    async fn mouse_event(
        &self,
        kind: &str,
        x: f64,
        y: f64,
        button: Option<MouseButton>,
        click_count: u32,
    ) -> Result<(), BrowserError> {
        let (buttons, modifiers) = {
            let mut state = self.monitor.lock();
            if let Some(button) = button {
                match kind {
                    "mousePressed" => state.buttons |= button.mask(),
                    "mouseReleased" => state.buttons &= !button.mask(),
                    _ => {}
                }
            }
            state.mouse = (x, y);
            (state.buttons, state.modifiers)
        };
        self.raw(
            "Input.dispatchMouseEvent",
            json!({"type": kind, "x": x, "y": y, "modifiers": modifiers,
                "button": button.map_or("none", MouseButton::cdp), "buttons": buttons,
                "clickCount": click_count}),
        )
        .await?;
        Ok(())
    }

    async fn mouse_move(&self, x: f64, y: f64, steps: u32) -> Result<(), BrowserError> {
        let (from_x, from_y) = self.monitor.lock().mouse;
        let steps = steps.max(1);
        for step in 1..=steps {
            let ratio = f64::from(step) / f64::from(steps);
            self.mouse_event(
                "mouseMoved",
                from_x + (x - from_x) * ratio,
                from_y + (y - from_y) * ratio,
                None,
                0,
            )
            .await?;
        }
        Ok(())
    }

    async fn mouse_click(
        &self,
        x: f64,
        y: f64,
        button: MouseButton,
        click_count: u32,
        delay_ms: f64,
    ) -> Result<(), BrowserError> {
        self.mouse_move(x, y, 1).await?;
        for count in 1..=click_count.max(1) {
            self.mouse_event("mousePressed", x, y, Some(button), count)
                .await?;
            if delay_ms > 0.0 {
                tokio::time::sleep(millis(delay_ms)).await;
            }
            self.mouse_event("mouseReleased", x, y, Some(button), count)
                .await?;
            if delay_ms > 0.0 && count < click_count {
                tokio::time::sleep(millis(delay_ms)).await;
            }
        }
        Ok(())
    }

    /// Release a button left pressed by a cancelled or failed click.
    async fn release_button(&self, button: MouseButton) {
        let (pressed, (x, y)) = {
            let state = self.monitor.lock();
            (state.buttons & button.mask() != 0, state.mouse)
        };
        if pressed {
            let _ = self
                .mouse_event("mouseReleased", x, y, Some(button), 1)
                .await;
        }
    }

    async fn element_pointer(
        &self,
        selector: &str,
        timeout_ms: f64,
        enabled: bool,
        retarget: bool,
    ) -> Result<(f64, f64), BrowserError> {
        let mut waiter = Waiter::new(timeout_ms);
        let states: &[&str] = if enabled {
            &["visible", "enabled"]
        } else {
            &["visible"]
        };
        loop {
            let object = self
                .actionable(&mut waiter, selector, retarget, states)
                .await?;
            match waiter
                .bound(
                    "scrolling into view",
                    attempt_result(self.click_point(&object)),
                )
                .await?
            {
                Ok(point) => return Ok(point),
                Err(reason) => waiter.retry(&reason).await?,
            }
        }
    }

    async fn key_event(
        &self,
        kind: &str,
        description: &KeyDescription,
    ) -> Result<(), BrowserError> {
        let modifiers = self.monitor.lock().modifiers;
        let mut params = json!({"type": kind, "modifiers": modifiers,
            "windowsVirtualKeyCode": description.key_code, "nativeVirtualKeyCode": description.key_code,
            "code": description.code, "key": description.key, "location": description.location,
            "isKeypad": description.location == 3, "autoRepeat": false});
        if kind != "keyUp" && !description.text.is_empty() {
            params["text"] = description.text.clone().into();
            params["unmodifiedText"] = description.text.clone().into();
        }
        self.raw("Input.dispatchKeyEvent", params).await?;
        Ok(())
    }

    async fn key_down(&self, key: &str) -> Result<(), BrowserError> {
        let modifiers = self.monitor.lock().modifiers;
        let description = keys::describe(key, modifiers)?;
        if let Some(bit) = keys::modifier_bit(&description.key) {
            self.monitor.lock().modifiers |= bit;
        }
        let kind = if description.text.is_empty() {
            "rawKeyDown"
        } else {
            "keyDown"
        };
        self.key_event(kind, &description).await
    }

    async fn key_up(&self, key: &str) -> Result<(), BrowserError> {
        let modifiers = self.monitor.lock().modifiers;
        let description = keys::describe(key, modifiers)?;
        if let Some(bit) = keys::modifier_bit(&description.key) {
            self.monitor.lock().modifiers &= !bit;
        }
        self.key_event("keyUp", &description).await
    }

    /// Press a key or shortcut such as `Enter`, `a` or `ControlOrMeta+A`.
    pub async fn press_shortcut(&self, shortcut: &str) -> Result<(), BrowserError> {
        let tokens = keys::split_shortcut(shortcut)?;
        let (modifiers, key) = tokens.split_at(tokens.len() - 1);
        for modifier in modifiers {
            self.key_down(modifier).await?;
        }
        let pressed = async {
            self.key_down(&key[0]).await?;
            self.key_up(&key[0]).await
        }
        .await;
        for modifier in modifiers.iter().rev() {
            let _ = self.key_up(modifier).await;
        }
        pressed
    }

    /// Type text at the current focus, one character at a time. Characters
    /// absent from the US layout are inserted with `Input.insertText`.
    async fn type_characters(
        &self,
        text: &str,
        jitter: Option<&mut Jitter>,
    ) -> Result<(), BrowserError> {
        let mut jitter = jitter;
        let mut buffer = [0u8; 4];
        for character in text.chars() {
            let as_str: &str = character.encode_utf8(&mut buffer);
            let key = match as_str {
                "\n" | "\r" => Some("Enter"),
                other if keys::has_key(other) && other.chars().count() == 1 => Some(other),
                _ => None,
            };
            match key {
                Some(key) => {
                    self.key_down(key).await?;
                    if let Some(jitter) = jitter.as_deref_mut() {
                        tokio::time::sleep(millis(jitter.between(50.0, 150.0))).await;
                    }
                    self.key_up(key).await?;
                }
                None => {
                    self.raw("Input.insertText", json!({ "text": as_str }))
                        .await?;
                    if let Some(jitter) = jitter.as_deref_mut() {
                        tokio::time::sleep(millis(jitter.between(50.0, 150.0))).await;
                    }
                }
            }
        }
        Ok(())
    }

    async fn checked_state(
        &self,
        waiter: &mut Waiter,
        selector: &str,
    ) -> Result<bool, BrowserError> {
        loop {
            let attempt = async {
                let Some(object) = self.resolve(selector, true).await? else {
                    return Err(Attempt::Retry(format!("waiting for locator({selector:?})")));
                };
                let state = self.element_call(&object, "checkedState", &[]).await?;
                Ok(state.get("value") == Some(&Value::Bool(true)))
            };
            match waiter
                .bound("reading checked state", attempt_result(attempt))
                .await?
            {
                Ok(state) => return Ok(state),
                Err(reason) => waiter.retry(&reason).await?,
            }
        }
    }

    async fn set_checked(
        &self,
        selector: &str,
        checked: bool,
        timeout_ms: f64,
    ) -> Result<(), BrowserError> {
        let mut waiter = Waiter::new(timeout_ms);
        if self.checked_state(&mut waiter, selector).await? == checked {
            return Ok(());
        }
        let remaining = waiter
            .remaining()
            .map_or(0.0, |left| left.as_secs_f64() * 1000.0)
            .max(if waiter.deadline.is_some() { 1.0 } else { 0.0 });
        let (x, y) = self
            .element_pointer(selector, remaining, true, false)
            .await
            .map_err(|error| match error {
                BrowserError::Timeout(_) => {
                    waiter.timeout_error("waiting for element to be clickable")
                }
                other => other,
            })?;
        if let Err(error) = self.mouse_click(x, y, MouseButton::Left, 1, 0.0).await {
            self.release_button(MouseButton::Left).await;
            return Err(error);
        }
        if self.checked_state(&mut waiter, selector).await? != checked {
            return Err(BrowserError::InvalidArgument(
                "Clicking the checkbox did not change its state".into(),
            ));
        }
        Ok(())
    }

    async fn fill(
        &self,
        selector: &str,
        value: &str,
        timeout_ms: f64,
        jitter: Option<&mut Jitter>,
    ) -> Result<(), BrowserError> {
        let mut waiter = Waiter::new(timeout_ms);
        let slow = jitter.is_some();
        let object = self
            .actionable(
                &mut waiter,
                selector,
                true,
                &["visible", "enabled", "editable"],
            )
            .await?;
        let initial = if slow { "" } else { value };
        let prepared = self
            .element_call(&object, "fillPrepare", &[json!(initial)])
            .await
            .map_err(attempt_error)?;
        if prepared.get("value").and_then(Value::as_str) == Some("type") {
            if initial.is_empty() {
                self.press_shortcut("Delete").await?;
            } else {
                self.raw("Input.insertText", json!({ "text": initial }))
                    .await?;
            }
        }
        if slow && !value.is_empty() {
            self.type_characters(value, jitter).await?;
        }
        Ok(())
    }

    async fn press_sequentially(
        &self,
        selector: &str,
        value: &str,
        timeout_ms: f64,
        jitter: Option<&mut Jitter>,
    ) -> Result<(), BrowserError> {
        let mut waiter = Waiter::new(timeout_ms);
        let object = self.actionable(&mut waiter, selector, true, &[]).await?;
        self.element_call(&object, "focus", &[])
            .await
            .map_err(attempt_error)?;
        self.type_characters(value, jitter).await
    }

    async fn select_labels(
        &self,
        selector: &str,
        labels: &[String],
        timeout_ms: f64,
    ) -> Result<(), BrowserError> {
        let mut waiter = Waiter::new(timeout_ms);
        loop {
            let object = self
                .actionable(&mut waiter, selector, true, &["visible", "enabled"])
                .await?;
            let selected = self
                .element_call(&object, "selectOptions", &[json!(labels)])
                .await;
            match selected {
                Ok(value) if value.get("value").and_then(Value::as_str) == Some("ok") => {
                    return Ok(())
                }
                Ok(_) => waiter.retry("did not find some options").await?,
                Err(Attempt::Retry(reason)) => waiter.retry(&reason).await?,
                Err(Attempt::Fail(error)) => return Err(error),
            }
        }
    }

    async fn wait_element(
        &self,
        selector: &str,
        state: ElementState,
        timeout_ms: f64,
    ) -> Result<(), BrowserError> {
        let mut waiter = Waiter::new(timeout_ms);
        loop {
            let reason = match waiter
                .bound("waiting for element", attempt_result(self.count(selector)))
                .await?
            {
                Ok((count, _)) if count > 1 => {
                    return Err(BrowserError::InvalidArgument(format!(
                        "strict mode violation: locator({selector:?}) resolved to {count} elements"
                    )))
                }
                Ok((count, visible)) => {
                    let done = match state {
                        ElementState::Attached => count == 1,
                        ElementState::Detached => count == 0,
                        ElementState::Visible => visible,
                        ElementState::Hidden => !visible,
                    };
                    if done {
                        return Ok(());
                    }
                    format!("waiting for locator({selector:?}) to be {state:?}").to_lowercase()
                }
                Err(reason) => reason,
            };
            waiter.attempt = waiter.attempt.max(2);
            waiter.retry(&reason).await?;
        }
    }

    async fn wait_load(&self, state: LoadState, timeout_ms: f64) -> Result<(), BrowserError> {
        let waiter = Waiter::new(timeout_ms);
        let wanted = state.lifecycle_name();
        let wait = async {
            loop {
                let notified = self.monitor.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.monitor.lock().lifecycle.contains(wanted) {
                    return Ok(());
                }
                // Lifecycle events can predate this page monitor (e.g. an
                // externally attached page); fall back to readyState.
                if state != LoadState::NetworkIdle {
                    let ready = self
                        .world_call("function() { return document.readyState; }", &[], true)
                        .await;
                    if let Ok(value) = ready {
                        let ready = value.get("value").and_then(Value::as_str).unwrap_or("");
                        if ready == "complete"
                            || (state == LoadState::DomContentLoaded && ready == "interactive")
                        {
                            return Ok(());
                        }
                    }
                }
                let _ = tokio::time::timeout(Duration::from_millis(100), notified).await;
            }
        };
        waiter
            .bound(&format!("waiting for load state {wanted:?}"), wait)
            .await
    }

    async fn run_one(
        &self,
        index: usize,
        action: &BrowserAction,
        jitter: &mut Option<Jitter>,
    ) -> Result<(), BrowserError> {
        match action {
            BrowserAction::Dialog {
                accept,
                prompt_text,
            } => {
                self.monitor.lock().dialog_replies.push_back(DialogReply {
                    index,
                    accept: *accept,
                    prompt_text: prompt_text.clone(),
                });
                Ok(())
            }
            BrowserAction::WaitTime { milliseconds } => {
                tokio::time::sleep(millis(*milliseconds)).await;
                Ok(())
            }
            BrowserAction::WaitElement {
                target,
                state,
                timeout_ms,
            } => self.wait_element(target, *state, *timeout_ms).await,
            BrowserAction::WaitLoad { state, timeout_ms } => {
                self.wait_load(*state, *timeout_ms).await
            }
            BrowserAction::PressKey { key } => self.press_shortcut(key).await,
            BrowserAction::Wheel { delta_x, delta_y } => {
                let ((x, y), buttons, modifiers) = {
                    let state = self.monitor.lock();
                    (state.mouse, state.buttons, state.modifiers)
                };
                self.raw(
                    "Input.dispatchMouseEvent",
                    json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": delta_x,
                        "deltaY": delta_y, "modifiers": modifiers, "buttons": buttons}),
                )
                .await?;
                Ok(())
            }
            BrowserAction::Move {
                target,
                timeout_ms,
                steps,
            } => match target {
                PointerTarget::Point { x, y } => self.mouse_move(*x, *y, *steps).await,
                PointerTarget::Selector(selector) => {
                    let (x, y) = self
                        .element_pointer(selector, *timeout_ms, false, false)
                        .await?;
                    self.mouse_move(x, y, 1).await
                }
            },
            BrowserAction::Click {
                target,
                timeout_ms,
                button,
                click_count,
                delay_ms,
            } => {
                let (x, y) = match target {
                    PointerTarget::Point { x, y } => (*x, *y),
                    PointerTarget::Selector(selector) => {
                        self.element_pointer(selector, *timeout_ms, true, false)
                            .await?
                    }
                };
                let result = self
                    .mouse_click(x, y, *button, *click_count, *delay_ms)
                    .await;
                if result.is_err() {
                    self.release_button(*button).await;
                }
                result
            }
            BrowserAction::Textbox {
                target,
                value,
                clear,
                timeout_ms,
            } => {
                if *clear {
                    self.fill(target, value, *timeout_ms, jitter.as_mut()).await
                } else {
                    self.press_sequentially(target, value, *timeout_ms, jitter.as_mut())
                        .await
                }
            }
            BrowserAction::Checkbox {
                target,
                checked,
                timeout_ms,
            } => self.set_checked(target, *checked, *timeout_ms).await,
            BrowserAction::Radio { target, timeout_ms } => {
                self.set_checked(target, true, *timeout_ms).await
            }
            BrowserAction::Combobox {
                target,
                labels,
                timeout_ms,
            } => self.select_labels(target, labels, *timeout_ms).await,
        }
    }

    /// Run an ordered action chain. Stops at the first failure; effects of
    /// completed actions remain. Queued dialog replies expire when the call
    /// returns. With `slowly`, waits 100–300 ms between actions and 50–150 ms
    /// per typed character.
    pub async fn run_actions(
        &self,
        actions: &[BrowserAction],
        slowly: bool,
    ) -> Result<(), ActionError> {
        {
            let mut state = self.monitor.lock();
            state.dialog_replies.clear();
            state.dialog_error = None;
        }
        let mut jitter = slowly.then(Jitter::new);
        let mut outcome = Ok(());
        for (offset, action) in actions.iter().enumerate() {
            let index = offset + 1;
            if let (Some(jitter), true) = (jitter.as_mut(), index > 1) {
                tokio::time::sleep(millis(jitter.between(100.0, 300.0))).await;
            }
            let result = self.run_one(index, action, &mut jitter).await;
            if action.uses_element() {
                self.release_objects().await;
            }
            if let Err(error) = result {
                outcome = Err(ActionError {
                    index,
                    kind: action.kind(),
                    timed_out: matches!(error, BrowserError::Timeout(_)),
                    message: action_message(error),
                });
                break;
            }
            if let Some((dialog_index, message)) = self.monitor.lock().dialog_error.take() {
                outcome = Err(ActionError {
                    index: dialog_index,
                    kind: "dialog",
                    timed_out: false,
                    message,
                });
                break;
            }
        }
        {
            let mut state = self.monitor.lock();
            state.dialog_replies.clear();
            if outcome.is_ok() {
                if let Some((index, message)) = state.dialog_error.take() {
                    outcome = Err(ActionError {
                        index,
                        kind: "dialog",
                        timed_out: false,
                        message,
                    });
                }
            }
        }
        outcome
    }

    /// Resolve a Playwright-style selector or `aria-ref` to exactly one
    /// element and return its outer HTML (strict mode, waits up to timeout).
    pub async fn element_outer_html(
        &self,
        selector: &str,
        timeout_ms: f64,
    ) -> Result<String, BrowserError> {
        let mut waiter = Waiter::new(timeout_ms);
        let object = self.actionable(&mut waiter, selector, false, &[]).await?;
        let html = self
            .element_call(&object, "outerHTML", &[])
            .await
            .map_err(attempt_error);
        self.release_objects().await;
        Ok(html?
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }
}

async fn attempt_result<T>(
    future: impl std::future::Future<Output = Result<T, Attempt>>,
) -> Result<Result<T, String>, BrowserError> {
    match future.await {
        Ok(value) => Ok(Ok(value)),
        Err(Attempt::Retry(reason)) => Ok(Err(reason)),
        Err(Attempt::Fail(error)) => Err(error),
    }
}

fn attempt_error(attempt: Attempt) -> BrowserError {
    match attempt {
        Attempt::Retry(reason) => BrowserError::Protocol(reason),
        Attempt::Fail(error) => error,
    }
}

fn action_message(error: BrowserError) -> String {
    match error {
        BrowserError::InvalidArgument(message)
        | BrowserError::Timeout(message)
        | BrowserError::Protocol(message)
        | BrowserError::Io(message) => message,
        BrowserError::Disconnected => "Target page, context or browser has been closed".into(),
    }
}

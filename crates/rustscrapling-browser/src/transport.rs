//! Low-level CDP transport. In particular, none of the commands sent here
//! cause chromiumoxide's FrameManager to initialize a page. D-007 prohibits
//! `Runtime.enable` during ordinary navigation, DOM reads and stealth use.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide_types::Command as CdpCommand;
use futures_util::stream::{SplitSink, StreamExt};
use futures_util::SinkExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Writer = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type Pending = HashMap<u64, oneshot::Sender<Result<Value, BrowserError>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserError {
    Io(String),
    Protocol(String),
    Disconnected,
    Timeout(String),
    InvalidArgument(String),
}

impl fmt::Display for BrowserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "browser I/O error: {msg}"),
            Self::Protocol(msg) => write!(f, "CDP error: {msg}"),
            Self::Disconnected => write!(f, "CDP connection closed"),
            Self::Timeout(command) => write!(f, "CDP operation timed out: {command}"),
            Self::InvalidArgument(msg) => write!(f, "invalid browser argument: {msg}"),
        }
    }
}
impl std::error::Error for BrowserError {}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CdpEvent {
    pub method: String,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub params: Value,
}

pub(crate) struct Connection {
    writer: Mutex<Writer>,
    pending: Mutex<Pending>,
    next_id: AtomicU64,
    events: broadcast::Sender<CdpEvent>,
    history: Mutex<Vec<String>>,
}

impl Connection {
    pub async fn connect(endpoint: &str) -> Result<Arc<Self>, BrowserError> {
        // A remote CDP endpoint exposes all browser state, including cookies.
        // Allow direct remote connections only with TLS; a plaintext endpoint
        // must be reached through an explicit loopback tunnel.
        let url =
            url::Url::parse(endpoint).map_err(|e| BrowserError::InvalidArgument(e.to_string()))?;
        if !matches!(url.scheme(), "ws" | "wss") {
            return Err(BrowserError::InvalidArgument(
                "CDP endpoint must be ws(s)".into(),
            ));
        }
        let host = url.host_str().unwrap_or_default();
        if url.scheme() == "ws" && !matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1") {
            return Err(BrowserError::InvalidArgument(
                "remote CDP requires wss:// or a local tunnel".into(),
            ));
        }
        let (websocket, _) = connect_async(endpoint)
            .await
            .map_err(|e| BrowserError::Io(e.to_string()))?;
        let (writer, mut reader) = websocket.split();
        let (events, _) = broadcast::channel(1024);
        let this = Arc::new(Self {
            writer: Mutex::new(writer),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            events,
            history: Mutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&this);
        tokio::spawn(async move {
            while let Some(message) = reader.next().await {
                let Ok(Message::Text(text)) = message else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let Some(this) = weak.upgrade() else {
                    break;
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    if std::env::var_os("RUSTSCRAPLING_TRACE_CDP_METHODS").is_some() {
                        eprintln!("CDP recv id={id}");
                    }
                    if let Some(sender) = this.pending.lock().await.remove(&id) {
                        let outcome = if let Some(error) = value.get("error") {
                            Err(BrowserError::Protocol(error.to_string()))
                        } else {
                            Ok(value.get("result").cloned().unwrap_or_else(|| json!({})))
                        };
                        let _ = sender.send(outcome);
                    }
                } else if let Ok(event) = serde_json::from_value::<CdpEvent>(value) {
                    if std::env::var_os("RUSTSCRAPLING_TRACE_CDP_METHODS").is_some()
                        && (event.method.starts_with("Page.")
                            || event.method.starts_with("Target."))
                    {
                        eprintln!("CDP event {}", event.method);
                    }
                    let _ = this.events.send(event);
                }
            }
            if let Some(this) = weak.upgrade() {
                for (_, sender) in this.pending.lock().await.drain() {
                    let _ = sender.send(Err(BrowserError::Disconnected));
                }
            }
        });
        Ok(this)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events.subscribe()
    }

    pub async fn history(&self) -> Vec<String> {
        self.history.lock().await.clone()
    }

    pub async fn send_typed<C: CdpCommand>(
        &self,
        command: C,
        session_id: Option<&str>,
    ) -> Result<C::Response, BrowserError> {
        let method = command.identifier();
        let params =
            serde_json::to_value(command).map_err(|e| BrowserError::Protocol(e.to_string()))?;
        let result = self.send(&method, params, session_id).await?;
        C::response_from_value(result).map_err(|e| BrowserError::Protocol(e.to_string()))
    }

    pub async fn send(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value, BrowserError> {
        self.send_with_timeout(method, params, session_id, Duration::from_secs(20))
            .await
    }

    /// Use an explicit response timeout for optional body capture. Unlike
    /// canceling `send()` from outside, this removes the pending CDP sender
    /// before returning and avoids accumulating abandoned requests.
    pub async fn send_with_timeout(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
        response_timeout: Duration,
    ) -> Result<Value, BrowserError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session_id {
            message["sessionId"] = session_id.into();
        }
        if std::env::var_os("RUSTSCRAPLING_TRACE_CDP_METHODS").is_some() {
            eprintln!("CDP send id={id} method={method}");
        }
        self.history.lock().await.push(method.to_owned());
        let data = Message::Text(message.to_string().into());
        let deadline = tokio::time::Instant::now() + response_timeout;
        match tokio::time::timeout_at(deadline, async move {
            self.writer.lock().await.send(data).await
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.pending.lock().await.remove(&id);
                return Err(BrowserError::Io(error.to_string()));
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(BrowserError::Timeout(method.into()));
            }
        }
        match tokio::time::timeout_at(deadline, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BrowserError::Disconnected),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(BrowserError::Timeout(method.into()))
            }
        }
    }
}

// Generic WebSocket hub.
//
// One broadcast channel (capacity 256) shared by the whole application:
// any code can publish a JSON event with `global_hub().broadcast(...)`
// and every connected `GET /ws` client receives it as a JSON text frame.
// The hub is a process-wide singleton because `AppContext` carries no
// WebSocket state; `WsHub::new` still exists for tests and for code
// that wants an isolated hub.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json;
use crate::modules::network::{note_connection_closed, note_connection_open};
use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::Message;
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;

/// A broadcast hub for JSON WebSocket events.
pub struct WsHub {
    sender: broadcast::Sender<Value>,
}

impl WsHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self { sender }
    }

    /// Publish a JSON event to every connected client. Messages sent
    /// while nobody is connected are dropped.
    pub fn broadcast(&self, message: Value) {
        let _ = self.sender.send(message);
    }

    /// Subscribe to broadcast events.
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.sender.subscribe()
    }
}

impl Clone for WsHub {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl Default for WsHub {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_HUB: OnceLock<WsHub> = OnceLock::new();

/// The process-wide hub used by `GET /ws` and by every publisher.
pub fn global_hub() -> &'static WsHub {
    GLOBAL_HUB.get_or_init(WsHub::new)
}

/// `GET /ws`: upgrade to WebSocket and forward every broadcast event
/// as a JSON text frame. Client ping frames are answered with pong;
/// other client messages are ignored. The connection is counted in the
/// network module statistics until it closes.
pub async fn ws_handler(
    req: HttpRequest,
    stream: web::Payload,
    ctx: web::Data<Arc<AppContext>>,
) -> Result<HttpResponse, actix_web::Error> {
    let debug = ctx.is_debug();
    let (response, mut session, mut msg_stream) = match actix_ws::handle(&req, stream) {
        Ok(parts) => parts,
        Err(error) => {
            let err = UnifiedError::bad_request(format!("WebSocket handshake failed: {error}"));
            return Ok(json::error_response(&err, debug));
        }
    };
    note_connection_open("websocket");
    let mut receiver = global_hub().subscribe();
    actix_web::rt::spawn(async move {
        loop {
            tokio::select! {
                incoming = msg_stream.next() => {
                    match incoming {
                        Some(Ok(Message::Ping(bytes))) => {
                            if session.pong(&bytes).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(_)) => {}
                    }
                }
                broadcast = receiver.recv() => {
                    match broadcast {
                        Ok(message) => {
                            let text = serde_json::to_string(&message).unwrap_or_default();
                            if session.text(text).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
        let _ = session.close(None).await;
        note_connection_closed("websocket");
    });
    Ok(response)
}

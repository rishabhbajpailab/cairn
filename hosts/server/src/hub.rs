//! Live updates (the old Hub DO): an in-process broadcast feeding `/hub` WebSockets. Change
//! events are hints to refetch, so a slow client that lags just skips ahead.

use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use cairn_app::AppState;
use cairn_app::auth::Scope;
use cairn_app::ports::{ChangeEvent, Hub};
use std::collections::HashMap;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct BroadcastHub(pub broadcast::Sender<String>);

impl BroadcastHub {
    pub fn new() -> Self {
        Self(broadcast::channel(256).0)
    }
}

#[async_trait]
impl Hub for BroadcastHub {
    async fn publish(&self, ev: &ChangeEvent) {
        match serde_json::to_string(ev) {
            // Err = nobody listening, which is fine.
            Ok(s) => {
                let _ = self.0.send(s);
            }
            Err(e) => tracing::warn!(error = %e, "could not encode change event"),
        }
    }
}

/// `GET /hub?token=…`. Browsers can't set headers on WebSocket upgrades, so the token comes
/// in the query and is checked like a bearer token with `read` scope.
pub async fn upgrade(
    State((app, hub)): State<(AppState, BroadcastHub)>,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let mut h = HeaderMap::new();
    if let Ok(v) = format!("Bearer {}", q.get("token").map(String::as_str).unwrap_or("")).parse() {
        h.insert(header::AUTHORIZATION, v);
    }
    if app.tokens.check(&h, Scope::Read).is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let rx = hub.0.subscribe();
    ws.on_upgrade(move |socket| forward(socket, rx))
}

async fn forward(mut socket: WebSocket, mut rx: broadcast::Receiver<String>) {
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => tracing::debug!(skipped = n, "hub client lagged"),
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

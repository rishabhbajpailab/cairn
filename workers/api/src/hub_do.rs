//! Hub Durable Object: holds browser WebSockets (hibernation API) and fans out change
//! events. Replaces database live queries, so no connection to SurrealDB stays open.

use worker::{DurableObject, Env, Request, Response, Result, State, WebSocket, WebSocketPair, durable_object};

#[durable_object]
pub struct Hub {
    state: State,
}

impl DurableObject for Hub {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        match req.path().as_str() {
            "/publish" => {
                let text = req.text().await?;
                for ws in self.state.get_websockets() {
                    if let Err(e) = ws.send_with_str(&text) {
                        tracing::debug!(error = %e, "dropping dead socket");
                    }
                }
                Response::empty()
            }
            _ => {
                // WebSocket upgrade (auth already checked by the API Worker).
                let pair = WebSocketPair::new()?;
                self.state.accept_web_socket(&pair.server);
                Response::from_websocket(pair.client)
            }
        }
    }

    async fn websocket_close(&self, ws: WebSocket, code: usize, reason: String, _clean: bool) -> Result<()> {
        ws.close(Some(code as u16), Some(reason))
    }
}

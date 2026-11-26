//! Discord over the HTTP interactions endpoint: no long-running gateway connection needed.
//! docs/ingest.md § Discord.

use crate::state::AppState;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use cairn_domain::{CaptureSource, SourceKind};
use cairn_http::{HttpRequest, HttpTransport};
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct DiscordKeys {
    pub public_key: VerifyingKey,
    pub bot_token: String,
    pub dm_user_id: String,
}

impl DiscordKeys {
    pub fn new(public_key_hex: &str, bot_token: String, dm_user_id: &str) -> Result<Self, String> {
        let bytes: [u8; 32] = hex::decode(public_key_hex.trim())
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|_| "DISCORD_PUBLIC_KEY must be 32 bytes hex".to_string())?;
        Ok(Self {
            public_key: VerifyingKey::from_bytes(&bytes).map_err(|e| e.to_string())?,
            bot_token,
            dm_user_id: dm_user_id.to_string(),
        })
    }
}

/// Discord requires rejecting bad signatures with 401; it sends invalid ones on purpose to test.
pub fn verify(key: &VerifyingKey, headers: &HeaderMap, body: &[u8]) -> bool {
    let get = |h: &str| headers.get(h).and_then(|v| v.to_str().ok());
    let (Some(sig_hex), Some(ts)) = (get("x-signature-ed25519"), get("x-signature-timestamp")) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(sig_hex) else { return false };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else { return false };
    let sig = Signature::from_bytes(&sig_arr);
    let mut msg = ts.as_bytes().to_vec();
    msg.extend_from_slice(body);
    key.verify_strict(&msg, &sig).is_ok()
}

const PING: u64 = 1;
const APPLICATION_COMMAND: u64 = 2;
const MESSAGE_COMPONENT: u64 = 3;
const EPHEMERAL: u64 = 1 << 6;

fn reply(content: impl Into<String>) -> Json<Value> {
    Json(json!({ "type": 4, "data": { "content": content.into(), "flags": EPHEMERAL } }))
}

pub async fn interactions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, StatusCode> {
    let keys = state.discord.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    if !verify(&keys.public_key, &headers, &body) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let v: Value = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    // Only you may use the bot.
    let user = v.pointer("/member/user/id").or_else(|| v.pointer("/user/id")).and_then(Value::as_str);
    if v["type"].as_u64() != Some(PING) && user != Some(keys.dm_user_id.as_str()) {
        return Ok(reply("This bot is private."));
    }

    match v["type"].as_u64() {
        Some(PING) => Ok(Json(json!({ "type": 1 }))),
        Some(APPLICATION_COMMAND) => Ok(command(&state, &v).await),
        Some(MESSAGE_COMPONENT) => {
            // custom_id = "act:<item_id>:<action>": run the plugin action (M4).
            let id = v.pointer("/data/custom_id").and_then(Value::as_str).unwrap_or("");
            Ok(reply(format!("Action `{id}` not wired up yet (M4).")))
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

fn option<'a>(v: &'a Value, name: &str) -> Option<&'a str> {
    v.pointer("/data/options")?.as_array()?.iter().find(|o| o["name"] == name)?.get("value")?.as_str()
}

async fn command(state: &AppState, v: &Value) -> Json<Value> {
    let name = v.pointer("/data/name").and_then(Value::as_str).unwrap_or("");
    let mut source = CaptureSource::new(SourceKind::Discord);
    source.reference = v.get("id").and_then(Value::as_str).map(|id| format!("interaction:{id}"));

    let text = match name {
        "c" => option(v, "text").map(str::to_string),
        "todo" => option(v, "text").map(|t| format!("todo: {t}")),
        "Save to Cairn" => v
            .pointer("/data/target_id")
            .and_then(Value::as_str)
            .and_then(|id| v.pointer(&format!("/data/resolved/messages/{id}/content")))
            .and_then(Value::as_str)
            .map(str::to_string),
        "ai" => {
            return match (option(v, "action"), &state.gateway) {
                (Some("off"), Some(gw)) => match gw.disable().await {
                    Ok(()) => reply("AI disabled. Re-enabling requires a gateway deploy (see docs/spend-control.md)."),
                    Err(e) => reply(format!("Could not reach gateway: {e}")),
                },
                (Some("status"), Some(gw)) => match gw.status().await {
                    Ok(s) => reply(format!("```json\n{s:#}\n```")),
                    Err(e) => reply(format!("Could not reach gateway: {e}")),
                },
                _ => reply("Usage: /ai status | /ai off (there is no `on`)"),
            };
        }
        _ => None,
    };

    let Some(text) = text else {
        return reply(format!("`/{name}` isn't implemented yet (see ROADMAP M4)."));
    };
    match crate::ingest::ingest(state, source, "text/plain".into(), Value::String(text)).await {
        Ok(r) => match r.planned {
            Some(p) => reply(format!("Saved as **{}**: {}", p.item_type, p.title)),
            None => reply("Stored raw capture (will be sorted later)."),
        },
        Err(e) => reply(format!("Failed to store: {}", e.message)),
    }
}

/// Outbound DM via Discord REST (reminders, digests). Buttons come from type `actions`
/// with a `discord_button` label; custom_id = "act:<item_id>:<action>".
pub async fn send_dm(
    http: &dyn HttpTransport,
    keys: &DiscordKeys,
    content: &str,
    buttons: &[(String, String)],
) -> Result<(), String> {
    let auth = format!("Bot {}", keys.bot_token);
    let req = HttpRequest::post_json(
        "https://discord.com/api/v10/users/@me/channels",
        &json!({ "recipient_id": keys.dm_user_id }),
    )
    .map_err(|e| e.to_string())?
    .header("authorization", &auth);
    let resp = http.send(req).await.map_err(|e| e.to_string())?;
    if !resp.is_success() {
        return Err(format!("create DM failed: {} {}", resp.status, resp.text()));
    }
    let ch = resp.json().map_err(|e| e.to_string())?;
    let channel_id = ch["id"].as_str().ok_or("no DM channel id")?;
    let components = if buttons.is_empty() {
        json!([])
    } else {
        let row: Vec<Value> = buttons
            .iter()
            .take(5)
            .map(|(label, custom_id)| json!({ "type": 2, "style": 2, "label": label, "custom_id": custom_id }))
            .collect();
        json!([{ "type": 1, "components": row }])
    };
    let req = HttpRequest::post_json(
        format!("https://discord.com/api/v10/channels/{channel_id}/messages"),
        &json!({ "content": content, "components": components }),
    )
    .map_err(|e| e.to_string())?
    .header("authorization", &auth);
    let resp = http.send(req).await.map_err(|e| e.to_string())?;
    if resp.is_success() { Ok(()) } else { Err(format!("send failed: {} {}", resp.status, resp.text())) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn signature_roundtrip_and_tamper() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        let body = br#"{"type":1}"#;
        let ts = "1700000000";
        let mut msg = ts.as_bytes().to_vec();
        msg.extend_from_slice(body);
        let sig = sk.sign(&msg);
        let mut h = HeaderMap::new();
        h.insert("x-signature-ed25519", hex::encode(sig.to_bytes()).parse().unwrap());
        h.insert("x-signature-timestamp", ts.parse().unwrap());
        assert!(verify(&vk, &h, body));
        assert!(!verify(&vk, &h, br#"{"type":2}"#));
        assert!(!verify(&vk, &HeaderMap::new(), body));
    }
}

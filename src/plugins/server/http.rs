//! Loopback routes: the WebSocket endpoint, plugin files and the SDK bootstrap script.

use std::{fs::File, io::Read, path::PathBuf, time::Duration};

use axum::{
    Router,
    extract::{
        Path as RoutePath, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{Hub, Identity, MAX_MESSAGE, Panel, Shared, lock, relative_url};
use crate::plugins::package;

const MAX_ASSET: u64 = 24 * 1024 * 1024;

pub(super) fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(upgrade))
        .route("/p/{token}/{*path}", get(asset))
        .with_state(state)
}

pub(in crate::plugins) fn local_request(headers: &HeaderMap, port: u16) -> bool {
    let expected = format!("127.0.0.1:{port}");
    let local = format!("localhost:{port}");
    let host = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok());
    if host != Some(expected.as_str()) && host != Some(local.as_str()) {
        return false;
    }
    headers.get(header::ORIGIN).is_none_or(|origin| {
        origin.to_str().is_ok_and(|origin| {
            origin == format!("http://{expected}") || origin == format!("http://{local}")
        })
    })
}

async fn upgrade(
    State(state): State<Shared>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !local_request(&headers, lock(&state).port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |ws| connection(ws, state))
}

async fn connection(mut ws: WebSocket, state: Shared) {
    let registration = tokio::time::timeout(Duration::from_secs(5), ws.recv()).await;
    let Ok(Some(Ok(Message::Text(text)))) = registration else {
        return;
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let (Some(uuid), Some(event)) = (data["uuid"].as_str(), data["event"].as_str()) else {
        return;
    };
    let (tx, mut outgoing) = mpsc::channel(128);
    if !lock(&state).authenticate(uuid, event, tx) {
        return;
    }
    loop {
        tokio::select! {
            frame = ws.recv() => match frame {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(data) = serde_json::from_str::<Value>(&text) {
                        let url = lock(&state).handle(uuid, &data);
                        if let Some(url) = url {
                            // ShellExecute can block while the browser starts.
                            tokio::task::spawn_blocking(move || crate::actions::open_http(&url));
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            },
            message = outgoing.recv() => {
                let Some(message) = message else { break };
                if ws.send(Message::Text(message.to_string().into())).await.is_err() { break; }
            }
        }
    }
    lock(&state).disconnected(uuid);
}

async fn asset(
    State(state): State<Shared>,
    headers: HeaderMap,
    RoutePath((token, path)): RoutePath<(String, String)>,
) -> Response {
    let (panel, script) = {
        let hub = lock(&state);
        if !local_request(&headers, hub.port) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let Some(panel) = hub.panels.get(&token).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let script = if path == "_deckx.js" {
            let Some(script) = bootstrap(&hub, &token, &panel) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            Some(script)
        } else {
            None
        };
        (panel, script)
    };
    if let Some(script) = script {
        return response("text/javascript; charset=utf-8", script.into_bytes());
    }
    let (file, mut bytes, entry) =
        match tokio::task::spawn_blocking(move || read_asset(&panel, &path)).await {
            Ok(Ok(asset)) => asset,
            Ok(Err(status)) => return status.into_response(),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
    let mime = match file
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    };
    if entry && mime.starts_with("text/html") {
        bytes.extend_from_slice(
            format!("\n<script src=\"/p/{token}/_deckx.js\"></script>\n").as_bytes(),
        );
    }
    response(mime, bytes)
}

/// The script injected into a panel's entry page. It calls the SDK's
/// `connectElgatoStreamDeckSocket` once the page defines it.
fn bootstrap(hub: &Hub, token: &str, panel: &Panel) -> Option<String> {
    let plugin = hub.plugins.get(panel.identity.plugin())?;
    let action = match &panel.identity {
        Identity::Inspector { instance, .. } => {
            hub.packet(instance, "willAppear", json!({})).to_string()
        }
        Identity::Plugin(_) => "{}".to_owned(),
    };
    let args = json!([
        hub.port.to_string(),
        token,
        panel.identity.event(),
        hub.info(plugin).to_string(),
        action
    ]);
    Some(format!(
        "(() => {{ let tries = 0; const t = setInterval(() => {{ if (typeof window.connectElgatoStreamDeckSocket === 'function') {{ clearInterval(t); window.connectElgatoStreamDeckSocket(...{args}); }} else if (++tries > 300) clearInterval(t); }}, 50); }})();"
    ))
}

/// Reads a plugin file and reports whether it is the panel's entry page.
fn read_asset(panel: &Panel, path: &str) -> Result<(PathBuf, Vec<u8>, bool), StatusCode> {
    let file = package::local_file(&panel.root, path).ok_or(StatusCode::NOT_FOUND)?;
    let mut bytes = Vec::new();
    File::open(&file)
        .and_then(|handle| handle.take(MAX_ASSET + 1).read_to_end(&mut bytes))
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if bytes.len() as u64 > MAX_ASSET {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let entry = relative_url(&panel.root, &file).is_ok_and(|name| name == panel.entry);
    Ok((file, bytes, entry))
}

fn response(mime: &'static str, bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        bytes,
    )
        .into_response()
}

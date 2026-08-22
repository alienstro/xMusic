//! The daemon, over its localhost API.
//!
//! An outbound adapter: it knows about `ureq`, routes and status codes, and
//! nothing about panes, keys or what any of these calls are for. Everything it
//! sends and receives is a protocol type, so a change to the wire is a change in
//! one crate rather than in two that have to be kept in step.

use std::time::Duration;

use serde::de::DeserializeOwned;

use xmusic_protocol::{
    BrowseRequest, Feed, LikeRequest, ListState, PlayRequest, PlayerState, PlaylistRequest,
    SearchRequest, TransportAction, TransportRequest, AUTH_HEADER, BASE_URL,
};

use crate::adapters::daemon_process;

const REQUEST_TIMEOUT: Duration = Duration::from_millis(3000);

pub fn player_state() -> Result<PlayerState, String> {
    get("/state")
}

pub fn list_state() -> Result<ListState, String> {
    get("/list")
}

pub fn search(query: &str, timeout: Duration) -> Result<(), String> {
    post(
        "/search",
        &SearchRequest {
            query: query.to_string(),
        },
        timeout,
    )
}

pub fn browse(feed: Feed, timeout: Duration) -> Result<(), String> {
    post("/browse", &BrowseRequest { feed }, timeout)
}

pub fn open_playlist(browse_id: &str, timeout: Duration) -> Result<(), String> {
    post(
        "/playlist",
        &PlaylistRequest {
            browse_id: browse_id.to_string(),
        },
        timeout,
    )
}

pub fn play(video_id: &str, queue: &[String], timeout: Duration) -> Result<(), String> {
    post(
        "/play",
        &PlayRequest {
            video_id: video_id.to_string(),
            queue: queue.to_vec(),
        },
        timeout,
    )
}

pub fn like(video_id: &str, liked: bool, timeout: Duration) -> Result<(), String> {
    post(
        "/like",
        &LikeRequest {
            video_id: video_id.to_string(),
            liked,
        },
        timeout,
    )
}

pub fn transport(action: TransportAction, timeout: Duration) -> Result<(), String> {
    post("/control", &TransportRequest { action }, timeout)
}

pub fn seek(delta: i64, timeout: Duration) -> Result<(), String> {
    post("/seek", &serde_json::json!({ "delta": delta }), timeout)
}

pub fn volume(delta: i64, timeout: Duration) -> Result<(), String> {
    post("/volume", &serde_json::json!({ "delta": delta }), timeout)
}

pub fn set_window_visible(visible: bool, timeout: Duration) -> Result<(), String> {
    let route = if visible { "/show-window" } else { "/hide-window" };
    post(route, &serde_json::json!({}), timeout)
}

pub fn import_cookies(body: &xmusic_protocol::CookiesRequest, timeout: Duration) -> Result<(), String> {
    post("/cookies", body, timeout)
}

fn get<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    let token = daemon_process::control_token()?;
    ureq::get(&format!("{BASE_URL}{path}"))
        .set(AUTH_HEADER, &token)
        .timeout(REQUEST_TIMEOUT)
        .call()
        .map_err(describe)?
        .into_json::<T>()
        .map_err(|error| format!("{path}: malformed response: {error}"))
}

fn post<B: serde::Serialize>(path: &str, body: &B, timeout: Duration) -> Result<(), String> {
    let token = daemon_process::control_token()?;
    ureq::post(&format!("{BASE_URL}{path}"))
        .set(AUTH_HEADER, &token)
        .timeout(timeout)
        .send_json(serde_json::to_value(body).map_err(|error| error.to_string())?)
        .map(|_| ())
        .map_err(describe)
}

fn describe(error: ureq::Error) -> String {
    match error {
        // The daemon answers with {"ok":false,"error":"..."} on failure; surface that, not a bare status code.
        ureq::Error::Status(code, response) => match response.into_json::<serde_json::Value>() {
            Ok(body) => body
                .get("error")
                .and_then(|value| value.as_str())
                .map(|message| message.to_string())
                .unwrap_or_else(|| format!("daemon returned HTTP {code}")),
            Err(_) => format!("daemon returned HTTP {code}"),
        },
        ureq::Error::Transport(transport) => format!("daemon unreachable: {transport}"),
    }
}

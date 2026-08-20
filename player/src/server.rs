//! Control server on 127.0.0.1. Every mutating route turns into a JavaScript
//! call against the hidden webview; every read serves the state the page last
//! reported.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, WebviewWindow};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::bridge::WaitError;
use crate::state::Shared;

/// How long a control call waits for the page to report back. Generous enough
/// for a busy webview, short enough that a stuck page fails rather than hangs.
const DISPATCH_TIMEOUT: Duration = Duration::from_millis(2000);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Loopback only. Must match `BASE_URL` in the tui crate.
pub const BIND_ADDR: &str = "127.0.0.1:13723";
const MAX_BODY: u64 = 64 * 1024;
const AUTH_HEADER: &str = "X-Xmusic-Token";
const EXPECTED_HOST: &str = "127.0.0.1:13723";
const PROTOCOL_VERSION: u32 = 1;

pub fn run(app: AppHandle, shared: Arc<Shared>, control_token: String) {
    let server = match Server::http(BIND_ADDR) {
        Ok(server) => server,
        Err(error) => {
            // Almost always "address in use", i.e. a daemon is already running.
            // Exiting is the right move: two daemons would fight over playback.
            eprintln!("xmusic-player: cannot bind {BIND_ADDR}: {error}");
            eprintln!("xmusic-player: another daemon is probably already running.");
            app.exit(1);
            return;
        }
    };
    println!("xmusic-player: control server listening on http://{BIND_ADDR}");

    for mut request in server.incoming_requests() {
        let (status, body) = handle(&app, &shared, &control_token, &mut request);
        let json_header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
            .expect("static header is valid");
        let response = Response::from_string(body)
            .with_status_code(status)
            .with_header(json_header);
        if let Err(error) = request.respond(response) {
            eprintln!("xmusic-player: failed to send response: {error}");
        }
    }
}

fn handle(
    app: &AppHandle,
    shared: &Arc<Shared>,
    control_token: &str,
    request: &mut Request,
) -> (u16, String) {
    let method = request.method().clone();
    // Strip any query string; none of these routes take one.
    let path = request
        .url()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_string();

    if !has_expected_host(request) {
        return (403, error_body("unexpected Host header"));
    }
    if path != "/health" {
        if has_header(request, "Origin") {
            return (403, error_body("browser-origin requests are not accepted"));
        }
        if !is_authorized(request, control_token) {
            return (401, error_body("missing or invalid control token"));
        }
    }
    if method == Method::Post && !has_json_content_type(request) {
        return (415, error_body("Content-Type must be application/json"));
    }

    let body = match method {
        Method::Post => match read_body(request) {
            Ok(value) => value,
            Err(message) => return (400, error_body(&message)),
        },
        _ => Value::Null,
    };

    match (&method, path.as_str()) {
        (Method::Get, "/health") => (
            200,
            json!({
                "ok": true,
                "service": "xmusic-player",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": PROTOCOL_VERSION
            })
            .to_string(),
        ),

        (Method::Get, "/state") => {
            let player = shared.player.lock().expect("player mutex poisoned").clone();
            (200, serde_json::to_string(&player).expect("state serialises"))
        }

        (Method::Get, "/search-results") => {
            let search = shared.search.lock().expect("search mutex poisoned").clone();
            (200, serde_json::to_string(&search).expect("search serialises"))
        }

        (Method::Post, "/search") => match body.get("query").and_then(Value::as_str) {
            Some(query) if !query.trim().is_empty() => {
                let query = query.trim();
                let seq = shared.begin_search(query);
                let timeout_shared = Arc::clone(shared);
                std::thread::spawn(move || {
                    std::thread::sleep(SEARCH_TIMEOUT);
                    timeout_shared.fail_search(
                        seq,
                        format!(
                            "search did not answer within {}s",
                            SEARCH_TIMEOUT.as_secs()
                        ),
                    );
                });
                // serde_json produces a correctly escaped JS string literal.
                // Interpolating the raw query here would be an injection hole.
                let literal = serde_json::to_string(query).expect("string serialises");
                match eval(app, &format!("window.__xmSearch({seq}, {literal})")) {
                    Ok(()) => (202, json!({ "seq": seq }).to_string()),
                    Err(message) => {
                        shared.fail_search(seq, message.clone());
                        (503, error_body(&message))
                    }
                }
            }
            _ => (400, error_body("missing or empty \"query\"")),
        },

        (Method::Post, "/play") => match body.get("videoId").and_then(Value::as_str) {
            Some(video_id) if is_video_id(video_id) => {
                dispatch(app, shared, "play", json!({ "videoId": video_id }))
            }
            Some(_) => (400, error_body("\"videoId\" is not a valid YouTube id")),
            None => (400, error_body("missing \"videoId\"")),
        },

        (Method::Post, "/control") => match body.get("action").and_then(Value::as_str) {
            Some(action @ ("play" | "pause" | "play_pause" | "next" | "prev")) => {
                dispatch(app, shared, "transport", json!({ "action": action }))
            }
            Some(other) => (400, error_body(&format!("unknown action \"{other}\""))),
            None => (400, error_body("missing \"action\"")),
        },

        (Method::Post, "/seek") => match numeric_arg(&body, "seconds", "delta") {
            Some((value, relative)) => dispatch(
                app,
                shared,
                "seek",
                json!({ "value": value, "relative": relative }),
            ),
            None => (400, error_body("expected \"seconds\" or \"delta\"")),
        },

        (Method::Post, "/volume") => match numeric_arg(&body, "level", "delta") {
            Some((value, relative)) => dispatch(
                app,
                shared,
                "volume",
                json!({ "value": value, "relative": relative }),
            ),
            None => (400, error_body("expected \"level\" or \"delta\"")),
        },

        // Makes the hidden window visible so the user can sign in, without
        // needing a recompile to flip a `visible(false)` flag.
        (Method::Post, "/show-window") => respond(set_visible(app, true)),
        (Method::Post, "/hide-window") => respond(set_visible(app, false)),

        (Method::Post, "/quit") => {
            let handle = app.clone();
            // Respond before exiting, otherwise the caller sees a dropped
            // connection instead of an acknowledgement.
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                handle.exit(0);
            });
            (200, json!({ "ok": true, "quitting": true }).to_string())
        }

        // Reads the page's own view of itself without going through IPC, which
        // is exactly what you need when IPC is the thing that's broken. The
        // probe is smuggled out through the document URL's fragment, since
        // `eval` cannot return a value.
        (Method::Get, "/diagnose") => match diagnose(app) {
            Ok(report) => (200, report),
            Err(message) => (503, error_body(&message)),
        },

        _ => (404, error_body("no such route")),
    }
}

/// Runs one control action on the page and answers with what actually happened.
///
/// 200 means the page did it. 409 means the page is there but could not — the
/// player has not loaded, or a control it needs is missing. 503/504 mean the
/// page never answered at all.
fn dispatch(app: &AppHandle, shared: &Arc<Shared>, action: &str, args: Value) -> (u16, String) {
    let pending = shared.bridge.dispatch();
    let script = format!(
        "window.__xmDispatch({}, {}, {})",
        pending.id(),
        serde_json::to_string(action).expect("string serialises"),
        args
    );
    if let Err(message) = eval(app, &script) {
        return (503, error_body(&message));
    }
    match pending.wait(DISPATCH_TIMEOUT) {
        Ok(()) => (200, json!({ "ok": true }).to_string()),
        Err(WaitError::Rejected(message)) => (409, error_body(&message)),
        Err(WaitError::Timeout(timeout)) => (
            504,
            error_body(&format!(
                "the page did not answer within {}ms",
                timeout.as_millis()
            )),
        ),
        Err(WaitError::Disconnected) => (503, error_body("the page dropped the request")),
    }
}

fn has_expected_host(request: &Request) -> bool {
    header_value(request, "Host")
        .is_some_and(|host| host.eq_ignore_ascii_case(EXPECTED_HOST))
}

fn has_header(request: &Request, name: &'static str) -> bool {
    request
        .headers()
        .iter()
        .any(|header| header.field.equiv(name))
}

fn header_value(request: &Request, name: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str().to_string())
}

fn is_authorized(request: &Request, expected: &str) -> bool {
    header_value(request, AUTH_HEADER)
        .is_some_and(|provided| constant_time_equal(provided.as_bytes(), expected.as_bytes()))
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn has_json_content_type(request: &Request) -> bool {
    header_value(request, "Content-Type").is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
    })
}

fn read_body(request: &mut Request) -> Result<Value, String> {
    let mut raw = String::new();
    request
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_string(&mut raw)
        .map_err(|error| format!("unreadable body: {error}"))?;
    if raw.len() as u64 > MAX_BODY {
        return Err(format!("request body exceeds {MAX_BODY} bytes"));
    }
    if raw.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&raw).map_err(|error| format!("invalid JSON body: {error}"))
}

/// Reads either an absolute (`absolute_key`) or relative (`relative_key`)
/// numeric argument, returning the value and whether it is relative.
fn numeric_arg(body: &Value, absolute_key: &str, relative_key: &str) -> Option<(i64, bool)> {
    if let Some(value) = body.get(absolute_key).and_then(Value::as_i64) {
        return Some((value, false));
    }
    body.get(relative_key)
        .and_then(Value::as_i64)
        .map(|value| (value, true))
}

/// YouTube ids are 11 URL-safe base64 characters. Checked so a malformed id
/// can't reach the page as something other than an id.
fn is_video_id(candidate: &str) -> bool {
    candidate.len() == 11
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn window(app: &AppHandle) -> Result<WebviewWindow, String> {
    app.get_webview_window(crate::WINDOW_LABEL)
        .ok_or_else(|| "player window is gone".to_string())
}

fn eval(app: &AppHandle, script: &str) -> Result<(), String> {
    window(app)?
        .eval(script)
        .map_err(|error| format!("eval failed: {error}"))
}

fn set_visible(app: &AppHandle, visible: bool) -> Result<(), String> {
    let window = window(app)?;
    let result = if visible {
        window.show().and_then(|()| window.set_focus())
    } else {
        window.hide()
    };
    result.map_err(|error| format!("window visibility change failed: {error}"))
}

fn respond(result: Result<(), String>) -> (u16, String) {
    match result {
        Ok(()) => (200, json!({ "ok": true }).to_string()),
        Err(message) => (503, error_body(&message)),
    }
}

fn error_body(message: &str) -> String {
    json!({ "ok": false, "error": message }).to_string()
}


const PROBE: &str = r#"
(() => {
  const report = {
    href: location.href,
    docTitle: document.title,
    tauri: typeof window.__TAURI__,
    tauriCore: window.__TAURI__ ? typeof window.__TAURI__.core : 'n/a',
    internals: typeof window.__TAURI_INTERNALS__,
    invoke: window.__TAURI_INTERNALS__ ? typeof window.__TAURI_INTERNALS__.invoke : 'n/a',
    injected: !!window.__xmInstalled,
    xmSearch: typeof window.__xmSearch,
    ytcfg: !!window.ytcfg,
    moviePlayer: !!document.querySelector('#movie_player'),
    ytmusicApp: !!document.querySelector('ytmusic-app'),
    lastError: window.__xmLastError || null,
    auth: window.__xmAuth || null,
  };
  history.replaceState(null, '', '#xm=' + encodeURIComponent(JSON.stringify(report)));
})()
"#;

fn diagnose(app: &AppHandle) -> Result<String, String> {
    let window = window(app)?;
    eval(app, PROBE)?;
    // Give the webview a moment to apply the history entry before reading back.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let url = window
        .url()
        .map_err(|error| format!("cannot read webview url: {error}"))?;
    let fragment = url
        .fragment()
        .ok_or("probe did not reach the page: no URL fragment")?;
    let encoded = fragment
        .strip_prefix("xm=")
        .ok_or_else(|| format!("unexpected fragment: {fragment}"))?;
    percent_decode(encoded)
}

/// Minimal percent-decoder: the probe payload is JSON put through
/// `encodeURIComponent`, so only `%XX` escapes need undoing.
fn percent_decode(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| "malformed escape".to_string())?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| "malformed escape".to_string())?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|error| format!("probe was not valid UTF-8: {error}"))
}

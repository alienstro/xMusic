//! The control server on 127.0.0.1.
//!
//! An inbound adapter and nothing else: it authenticates a request, turns its
//! body into a typed one, calls the application, and turns the answer back into
//! a status code. Which operations need the page awake, how a list is sequenced,
//! and how long anything is given all belong to `application.rs` — none of that
//! is decided here.

use std::io::Read;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server};

use xmusic_protocol::{
    BrowseRequest, CookiesRequest, ErrorBody, LikeRequest, PlayRequest, PlaylistRequest,
    RelativeOr, SearchRequest, TransportRequest, AUTH_HEADER, BIND_ADDR, PROTOCOL_VERSION,
    SERVICE_NAME,
};

use crate::application::{PlayerError, PlayerService};

const MAX_BODY: u64 = 64 * 1024;
const EXPECTED_HOST: &str = BIND_ADDR;

/// One answer, before it becomes bytes.
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn ok() -> Self {
        Self::json(200, json!({ "ok": true }))
    }

    fn json(status: u16, value: serde_json::Value) -> Self {
        Self {
            status,
            body: value.to_string(),
        }
    }

    fn raw(status: u16, body: String) -> Self {
        Self { status, body }
    }

    fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            body: serde_json::to_string(&ErrorBody::new(message)).expect("error body serialises"),
        }
    }
}

/// The one place an application failure becomes a status code.
///
/// The distinction the client depends on is between a page that answered "no"
/// and a page that never answered at all, so those get different codes and the
/// page's own words either way.
impl From<PlayerError> for Reply {
    fn from(error: PlayerError) -> Self {
        let status = match error {
            PlayerError::BadRequest(_) => 400,
            PlayerError::Refused(_) => 409,
            PlayerError::Unavailable(_) => 503,
            PlayerError::Timeout(_) => 504,
        };
        Reply::error(status, error.message())
    }
}

impl Reply {
    /// Whatever the application answered, as an answer on the wire.
    fn of<T: Into<Reply>>(result: Result<T, PlayerError>) -> Self {
        match result {
            Ok(value) => value.into(),
            Err(error) => error.into(),
        }
    }
}

/// A command that succeeded has nothing to add beyond that it did.
impl From<()> for Reply {
    fn from((): ()) -> Self {
        Reply::ok()
    }
}

/// A list route answers "accepted, and here is the number to recognise it by"; the list itself lands in `GET /list`.
impl From<u64> for Reply {
    fn from(seq: u64) -> Self {
        Reply::json(202, json!({ "seq": seq }))
    }
}

pub fn run(service: Arc<PlayerService>, control_token: String) {
    let server = match Server::http(BIND_ADDR) {
        Ok(server) => server,
        Err(error) => {
            // Almost always "address in use"; exiting is right, since two daemons would fight over playback.
            eprintln!("xmusic-player: cannot bind {BIND_ADDR}: {error}");
            eprintln!("xmusic-player: another daemon is probably already running.");
            std::process::exit(1);
        }
    };
    println!("xmusic-player: control server listening on http://{BIND_ADDR}");

    for mut request in server.incoming_requests() {
        let reply = handle(&service, &control_token, &mut request);
        let json_header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
            .expect("static header is valid");
        let response = Response::from_string(reply.body)
            .with_status_code(reply.status)
            .with_header(json_header);
        if let Err(error) = request.respond(response) {
            eprintln!("xmusic-player: failed to send response: {error}");
        }
    }
}

fn handle(service: &Arc<PlayerService>, control_token: &str, request: &mut Request) -> Reply {
    let method = request.method().clone();
    // Strip any query string; none of these routes take one.
    let path = request
        .url()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_string();

    if let Some(refusal) = gate(request, control_token, &method, &path) {
        return refusal;
    }

    let post = method == Method::Post;
    let body = match post {
        true => match read_body(request) {
            Ok(raw) => raw,
            Err(message) => return Reply::error(400, &message),
        },
        false => String::new(),
    };

    match (post, path.as_str()) {
        (false, "/health") => Reply::json(
            200,
            json!({
                "ok": true,
                "service": SERVICE_NAME,
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": PROTOCOL_VERSION
            }),
        ),

        (false, "/state") => Reply::raw(
            200,
            serde_json::to_string(&service.player_state()).expect("state serialises"),
        ),

        // `/search-results` is the name this had before there was anything but
        // search to list. Kept as an alias for one version, because breaking a
        // documented route to rename it buys nothing.
        (false, "/list" | "/search-results") => Reply::raw(
            200,
            serde_json::to_string(&service.list_state()).expect("list serialises"),
        ),

        // Reads the page's view of itself without using IPC, for when IPC is the broken thing.
        (false, "/diagnose") => match service.diagnose() {
            Ok(report) => Reply::raw(200, report),
            Err(error) => Reply::from(error),
        },

        (true, "/search") => with(&body, |body: SearchRequest| service.search(&body.query)),
        (true, "/browse") => with(&body, |body: BrowseRequest| service.browse(body.feed)),
        (true, "/playlist") => with(&body, |body: PlaylistRequest| {
            service.open_playlist(&body.browse_id)
        }),
        (true, "/play") => with(&body, |body: PlayRequest| {
            service.play(&body.video_id, &body.queue)
        }),
        (true, "/like") => with(&body, |body: LikeRequest| {
            service.like(&body.video_id, body.liked)
        }),
        (true, "/control") => with(&body, |body: TransportRequest| {
            service.transport(body.action)
        }),
        (true, "/seek") => match numeric(&body, "seconds", "delta") {
            Some(change) => Reply::of(service.seek(change)),
            None => Reply::error(400, "expected \"seconds\" or \"delta\""),
        },
        (true, "/volume") => match numeric(&body, "level", "delta") {
            Some(change) => Reply::of(service.volume(change)),
            None => Reply::error(400, "expected \"level\" or \"delta\""),
        },
        (true, "/cookies") => with(&body, |body: CookiesRequest| {
            service.import_cookies(body.cookies)
        }),

        // Hibernation is automatic; these drive it by hand, for measuring what
        // the page costs and for testing the wake path without waiting out the
        // idle timeout. `/wake` has already happened by the time it gets here.
        (true, "/sleep") => match service.unload() {
            Ok(()) => Reply::json(200, json!({ "ok": true, "hibernating": true })),
            Err(error) => Reply::from(error),
        },
        // Waking is the whole of this route: every other route that needs the
        // page has already waited for it by the time the application returns.
        (true, "/wake") => match service.wake() {
            Ok(()) => Reply::json(200, json!({ "ok": true, "hibernating": false })),
            Err(error) => Reply::from(error),
        },

        // Reveals the hidden window without needing a recompile to flip `visible(false)`.
        (true, "/show-window") => Reply::of(service.set_window_visible(true)),
        (true, "/hide-window") => Reply::of(service.set_window_visible(false)),

        (true, "/quit") => {
            // Respond before exiting, or the caller sees a dropped connection instead of an acknowledgement.
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(150));
                std::process::exit(0);
            });
            Reply::json(200, json!({ "ok": true, "quitting": true }))
        }

        _ => Reply::error(404, "no such route"),
    }
}

/// Everything a request has to satisfy before its body is worth reading.
fn gate(
    request: &Request,
    control_token: &str,
    method: &Method,
    path: &str,
) -> Option<Reply> {
    if !header_value(request, "Host").is_some_and(|host| host.eq_ignore_ascii_case(EXPECTED_HOST)) {
        return Some(Reply::error(403, "unexpected Host header"));
    }
    if path != "/health" {
        if has_header(request, "Origin") {
            return Some(Reply::error(403, "browser-origin requests are not accepted"));
        }
        if !is_authorized(request, control_token) {
            return Some(Reply::error(401, "missing or invalid control token"));
        }
    }
    if *method == Method::Post && !has_json_content_type(request) {
        return Some(Reply::error(415, "Content-Type must be application/json"));
    }
    None
}

/// Deserialises a body into the type the route expects and hands it to the
/// application. A body that will not parse is a `400` with serde's own
/// complaint, which names the field rather than the route.
fn with<B, T>(body: &str, run: impl FnOnce(B) -> Result<T, PlayerError>) -> Reply
where
    B: DeserializeOwned,
    T: Into<Reply>,
{
    match serde_json::from_str::<B>(body) {
        Ok(parsed) => Reply::of(run(parsed)),
        Err(error) => Reply::error(400, &format!("invalid request body: {error}")),
    }
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

fn read_body(request: &mut Request) -> Result<String, String> {
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
        // Routes that take nothing still parse a body; an empty one is an empty object.
        return Ok("{}".to_string());
    }
    Ok(raw)
}

/// Reads a change expressed either absolutely (`absolute_key`) or relatively (`relative_key`).
fn numeric(body: &str, absolute_key: &str, relative_key: &str) -> Option<RelativeOr> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    if let Some(absolute) = value.get(absolute_key).and_then(serde_json::Value::as_i64) {
        return Some(RelativeOr::Absolute(absolute));
    }
    value
        .get(relative_key)
        .and_then(serde_json::Value::as_i64)
        .map(RelativeOr::Relative)
}

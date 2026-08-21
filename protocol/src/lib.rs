//! What the daemon and the client agree on: the JSON they exchange, the
//! vocabulary that JSON is written in, and the handful of constants that would
//! silently break the pair if the two halves disagreed about them.
//!
//! This crate names the problem rather than the plumbing, so it depends on
//! nothing that carries the messages: no Tauri, no ratatui, no HTTP client or
//! server. Both binaries depend on it and neither depends on the other.

pub mod action;
pub mod error;
pub mod model;
pub mod request;

pub use action::{Feed, TransportAction};
pub use error::{ErrorBody, HealthResponse};
pub use model::{ListItem, ListState, PlayerState, Source};
pub use request::{
    BrowseRequest, Cookie, CookiesRequest, LikeRequest, PlayRequest, PlaylistRequest, RelativeOr,
    SearchRequest, TransportRequest,
};

/// Bumped whenever the daemon and client stop understanding each other. A client
/// that meets an older protocol treats the daemon as too old rather than probing
/// for which routes happen to exist.
pub const PROTOCOL_VERSION: u32 = 2;

/// Loopback only, and the same string on both sides: a daemon nobody can find is
/// indistinguishable from one that is not running.
pub const BIND_ADDR: &str = "127.0.0.1:13723";
pub const BASE_URL: &str = "http://127.0.0.1:13723";

/// Every route but `GET /health` carries this. Its value lives in
/// `~/.xmusic/control.token` and is regenerated on every daemon start.
pub const AUTH_HEADER: &str = "X-Xmusic-Token";

/// The name the daemon answers to in `GET /health`, so a client that finds
/// something else on the port says so rather than talking to a stranger.
pub const SERVICE_NAME: &str = "xmusic-player";

//! What a route says when it answers, in both directions.

use serde::{Deserialize, Serialize};

/// The failure body every route uses, so a client can show the daemon's own
/// words rather than a bare status code.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    pub ok: bool,
    pub error: String,
}

impl ErrorBody {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: message.into(),
        }
    }
}

/// `GET /health`, the one route that needs no token: enough for a client to tell
/// a daemon it can talk to from a stranger on the same port.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    #[serde(default)]
    pub service: Option<String>,
    pub version: String,
    #[serde(default)]
    pub protocol: Option<u32>,
}

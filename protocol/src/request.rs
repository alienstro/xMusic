//! The bodies routes accept, as types.
//!
//! Deserialising into these is the validation: an unknown feed name or a
//! misspelt transport action fails to parse rather than reaching a hand-written
//! list of accepted spellings, and the client cannot send one by accident at all.

use serde::{Deserialize, Serialize};

use crate::action::{Feed, TransportAction};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowseRequest {
    pub feed: Feed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistRequest {
    /// A playlist or album id as YouTube Music hands it out: `VL` + a playlist id, or an `MPRE`-prefixed album id.
    pub browse_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayRequest {
    pub video_id: String,
    /// The list the track was chosen from, in the order it is shown, so what
    /// follows is the next row rather than the radio YouTube Music invents
    /// around a lone track. Optional: a `/play` with no queue behaves as it
    /// always did, which is what a `curl` by hand should still do.
    #[serde(default)]
    pub queue: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LikeRequest {
    pub video_id: String,
    /// The state to end in, not a toggle: two clients pressing the heart at once should agree about where it ends up.
    pub liked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransportRequest {
    pub action: TransportAction,
}

/// A change expressed either as where to end up or as how far to move.
///
/// Seek and volume both take one, and both mean the same thing by it, so they
/// share a type rather than each parsing two optional numbers of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelativeOr {
    Absolute(i64),
    Relative(i64),
}

impl RelativeOr {
    pub fn value(self) -> i64 {
        match self {
            RelativeOr::Absolute(value) | RelativeOr::Relative(value) => value,
        }
    }

    pub fn is_relative(self) -> bool {
        matches!(self, RelativeOr::Relative(_))
    }
}

/// One cookie copied out of the user's own browser.
///
/// These lose the `HttpOnly` flag their browser set, which only stops scripts
/// reading a cookie and has no bearing on whether Google accepts one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Unix seconds, or `None` for a session cookie — which WebKit drops when the
    /// process ends, so the browser's own expiry is carried across to stop an
    /// imported session dying on the first restart.
    #[serde(default)]
    pub expires: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CookiesRequest {
    pub cookies: Vec<Cookie>,
}

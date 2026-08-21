//! What the daemon reports: the player, and whatever list was last asked for.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::action::Feed;

/// A snapshot of the YouTube Music player, refreshed by the injected script.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerState {
    /// False while the page is booting, or if YouTube Music served a page with no player element at all.
    pub ready: bool,
    /// True once `ytcfg` carries an InnerTube key, which is all a list needs. The player element arrives seconds later, so `ready` lags this.
    pub api_ready: bool,
    pub video_id: String,
    pub title: String,
    pub artist: String,
    /// "Artist • Album • Year" as shown in the player bar. Cosmetic.
    pub byline: String,
    /// Set only while `ready` is false: how the page looks to the injected script, which is the one thing worth knowing when the daemon never reports a player.
    pub diagnostic: String,
    pub is_playing: bool,
    pub is_buffering: bool,
    /// Seconds.
    pub position: u32,
    /// Seconds. Zero until the stream is loaded.
    pub duration: u32,
    /// 0-100.
    pub volume: u32,
    pub muted: bool,
    pub logged_in: bool,
    /// Whether the now-playing track is liked, or `None` where the page has not said. Unknown is not the same as not liked.
    pub liked: Option<bool>,
    /// Set by the daemon rather than the page: true while the webview has been parked on a blank document to give the page's memory back.
    pub hibernating: bool,
}

/// One row of a list. A song, a playlist and an album differ in which fields they
/// carry, not in which type describes them: a song has a `video_id` and a
/// duration, a playlist or album has a `browse_id` to open instead.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListItem {
    pub video_id: String,
    /// Set on a playlist or album row only: the id to browse for its tracks. Already carries YouTube Music's own `VL` prefix where it needs one.
    pub browse_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Pre-formatted "3:59"; YouTube Music never gives us raw seconds here.
    pub duration: String,
    /// `None` where the response carried no like state at all, which is every search result.
    pub liked: Option<bool>,
    /// Largest artwork URL the response carried. Parsed because it arrives for free; nothing draws it yet.
    pub thumbnail: String,
}

impl ListItem {
    /// Whether this row is a playlist or album to open rather than a track to play.
    pub fn is_openable(&self) -> bool {
        !self.browse_id.is_empty()
    }
}

/// What produced a list. The client files a reply by this rather than by what is
/// on screen, so a reply that arrives after the user has moved on still lands in
/// the tab that asked for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Source {
    #[default]
    Search,
    Feed(Feed),
    /// One playlist's or album's tracks. Carries the id it was asked for, which is how the right drill-down recognises its own reply.
    Playlist(String),
}

impl Source {
    /// A sentence-friendly name for this list, for a message about it failing.
    pub fn noun(&self) -> &str {
        match self {
            Source::Search => "Search",
            Source::Feed(feed) => feed.title(),
            Source::Playlist(_) => "This list",
        }
    }
}

/// One flat string on the wire rather than a tagged object, because `GET /list`
/// is a route people read with `curl`, and `"source": "liked"` says what
/// `{"feed": "liked"}` says with less ceremony. A drill-down carries the id it
/// was asked for, since that is what tells one open playlist from another.
impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Search => formatter.write_str("search"),
            Source::Feed(feed) => formatter.write_str(feed.as_str()),
            Source::Playlist(browse_id) => write!(formatter, "playlist:{browse_id}"),
        }
    }
}

impl FromStr for Source {
    type Err = ();

    fn from_str(candidate: &str) -> Result<Self, Self::Err> {
        if candidate == "search" {
            return Ok(Source::Search);
        }
        if let Some(browse_id) = candidate.strip_prefix("playlist:") {
            return Ok(Source::Playlist(browse_id.to_string()));
        }
        candidate.parse::<Feed>().map(Source::Feed)
    }
}

impl Serialize for Source {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Source {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse()
            .map_err(|()| serde::de::Error::custom(format!("unknown list source {raw:?}")))
    }
}

/// The daemon's one list slot, whatever produced it.
///
/// A browse and a search differ in origin and in nothing else, so they share one
/// slot rather than having one each; `seq` distinguishes fresh results from stale
/// ones and lets a reply for a tab the user has moved past be dropped.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ListState {
    pub seq: u64,
    pub source: Source,
    /// What to call it on screen: the query for a search, the feed's name otherwise.
    pub label: String,
    pub pending: bool,
    /// The page stopped at its page cap, so this is part of the feed rather than all of it.
    pub truncated: bool,
    pub error: Option<String>,
    pub items: Vec<ListItem>,
}

//! Shared state written by the page and read by the control server.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::bridge::Bridge;

/// A snapshot of the YT Music player, refreshed by the injected script.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    /// False while the page is still booting, or if YT Music served a page
    /// without its player element at all.
    pub ready: bool,
    pub video_id: String,
    pub title: String,
    pub artist: String,
    /// "Artist • Album • Year" as shown in the player bar. Cosmetic.
    pub byline: String,
    /// Set only while `ready` is false: what the page looks like from the
    /// injected script's point of view. The one thing worth knowing when the
    /// daemon comes up but never reports a player.
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Pre-formatted "3:59"; YT Music never gives us raw seconds here.
    pub duration: String,
}

/// The most recent search. `seq` lets a client tell fresh results from stale
/// ones, and lets the daemon drop a reply for a query the user has moved past.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchState {
    pub seq: u64,
    pub query: String,
    pub pending: bool,
    pub error: Option<String>,
    pub results: Vec<SearchResult>,
}

#[derive(Default)]
pub struct Shared {
    pub player: Mutex<PlayerState>,
    pub search: Mutex<SearchState>,
    /// Outstanding control calls waiting on the page to report back.
    pub bridge: Bridge,
    seq: AtomicU64,
}

impl Shared {
    /// Opens a new search, returning its sequence number. Any in-flight reply
    /// for an older sequence is discarded when it arrives.
    pub fn begin_search(&self, query: &str) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut search = self.search.lock().expect("search mutex poisoned");
        // Keep the previous results on screen while the new ones load rather
        // than blanking the list.
        let previous = std::mem::take(&mut search.results);
        *search = SearchState {
            seq,
            query: query.to_string(),
            pending: true,
            error: None,
            results: previous,
        };
        seq
    }

    pub fn finish_search(
        &self,
        seq: u64,
        query: String,
        results: Vec<SearchResult>,
        error: Option<String>,
    ) {
        let mut search = self.search.lock().expect("search mutex poisoned");
        if seq != search.seq || !search.pending {
            // A newer search has already been issued, or this request already
            // timed out. Either way, this reply is stale.
            return;
        }
        search.query = query;
        search.pending = false;
        search.error = error;
        search.results = results;
    }

    pub fn fail_search(&self, seq: u64, error: String) {
        let mut search = self.search.lock().expect("search mutex poisoned");
        if seq != search.seq || !search.pending {
            return;
        }
        search.pending = false;
        search.error = Some(error);
    }
}

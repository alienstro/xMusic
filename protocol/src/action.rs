//! The vocabulary of things that can be asked for, as types rather than strings.
//!
//! A misspelt transport action or feed name used to be a runtime 400 discovered
//! by whoever typed it. Naming them here means the compiler catches it on both
//! sides, and the daemon's validation becomes deserialisation rather than a
//! hand-written list of accepted spellings.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// What the transport controls can be asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportAction {
    Play,
    Pause,
    PlayPause,
    Next,
    Prev,
}

impl TransportAction {
    /// The wire spelling, which is also what `inject.js` switches on.
    pub fn as_str(self) -> &'static str {
        match self {
            TransportAction::Play => "play",
            TransportAction::Pause => "pause",
            TransportAction::PlayPause => "play_pause",
            TransportAction::Next => "next",
            TransportAction::Prev => "prev",
        }
    }
}

impl fmt::Display for TransportAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TransportAction {
    type Err = ();

    fn from_str(candidate: &str) -> Result<Self, Self::Err> {
        match candidate {
            "play" => Ok(TransportAction::Play),
            "pause" => Ok(TransportAction::Pause),
            "play_pause" => Ok(TransportAction::PlayPause),
            "next" => Ok(TransportAction::Next),
            "prev" => Ok(TransportAction::Prev),
            _ => Err(()),
        }
    }
}

/// A library feed, which is one `browseId` on the far side and one tab on the near one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feed {
    Liked,
    Playlists,
    Albums,
    History,
}

impl Feed {
    pub const ALL: [Feed; 4] = [Feed::Liked, Feed::Playlists, Feed::Albums, Feed::History];

    /// The wire spelling, which is also the name `inject.js` maps to a `browseId`.
    pub fn as_str(self) -> &'static str {
        match self {
            Feed::Liked => "liked",
            Feed::Playlists => "playlists",
            Feed::Albums => "albums",
            Feed::History => "history",
        }
    }

    /// What to call it on screen and in a message about it.
    pub fn title(self) -> &'static str {
        match self {
            Feed::Liked => "Liked",
            Feed::Playlists => "Playlists",
            Feed::Albums => "Albums",
            Feed::History => "History",
        }
    }

    /// Whether this feed answers with cards to open rather than songs to play.
    pub fn is_grid(self) -> bool {
        matches!(self, Feed::Playlists | Feed::Albums)
    }
}

impl fmt::Display for Feed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Feed {
    type Err = ();

    fn from_str(candidate: &str) -> Result<Self, Self::Err> {
        Feed::ALL
            .into_iter()
            .find(|feed| feed.as_str() == candidate)
            .ok_or(())
    }
}

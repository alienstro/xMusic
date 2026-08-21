//! Everything the interface knows, and nothing about how it learned it.
//!
//! The model holds no client, no socket and no thread. `update` changes it and
//! returns effects; `view` reads it. That is the whole of its contract, and it
//! is what lets the optimistic paint below be reasoned about on its own.

use std::time::{Duration, Instant};

use xmusic_protocol::PlayerState;

use crate::panes::Panes;

/// How long a local change is trusted over the daemon's report: the page's answer has to travel back through IPC and a poll, and redrawing the old value meanwhile is what makes a working control feel broken.
pub const OPTIMISM: Duration = Duration::from_millis(1500);

pub enum Mode {
    Normal,
    Editing,
    /// Waiting for a y/n answer before stopping the daemon.
    ConfirmStopDaemon,
}

pub struct Model {
    pub mode: Mode,
    pub input: String,
    /// Which tab is on screen, what each has loaded, and the drill-down stack.
    pub panes: Panes,
    pub player: PlayerState,
    pub status: String,
    pub online: bool,
    /// A list the user is waiting on. Only for the list on screen: a pane loading in the background is not something to put a spinner on.
    pub loading_list: bool,
    pub should_quit: bool,
    /// When the interface opened, which is all a spinner needs to turn at one speed whatever the redraw rate.
    pub started: Instant,

    pub(crate) stop_daemon_then_quit: bool,
    /// Values the user has just asked for, held until the daemon confirms them.
    pub(crate) guessed_volume: Option<(u32, Instant)>,
    pub(crate) guessed_position: Option<(u32, Instant)>,
    pub(crate) guessed_playing: Option<(bool, Instant)>,
    /// A like this session set on the now-playing track, held for as long as that track is loaded.
    ///
    /// Not a timed guess like the others: a like placed through InnerTube never
    /// reaches YouTube Music's own player bar, which is where the reported state
    /// is read from, so waiting for the daemon to agree would mean waiting
    /// forever and then repainting the wrong heart.
    pub(crate) set_like: Option<(String, bool)>,
    /// The track Enter asked for, held until the daemon reports it playing; a title rather than a flag, because the player bar still describes the old track for about a second.
    pub(crate) awaiting: Option<String>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            input: String::new(),
            panes: Panes::default(),
            player: PlayerState::default(),
            status: "Press / to search".into(),
            online: false,
            loading_list: false,
            should_quit: false,
            started: Instant::now(),
            stop_daemon_then_quit: false,
            guessed_volume: None,
            guessed_position: None,
            guessed_playing: None,
            set_like: None,
            awaiting: None,
        }
    }
}

impl Model {
    /// Whether the player is waiting on something rather than playing.
    pub fn is_loading(&self) -> bool {
        // An unloaded page is not a loading one: it is waiting for a reason to come back.
        self.online
            && !self.player.hibernating
            && (self.awaiting.is_some() || self.player.is_buffering || !self.player.ready)
    }

    /// The title to show while a requested track has not started yet.
    pub fn loading_title(&self) -> Option<&str> {
        self.awaiting.as_deref()
    }

    /// Overlays what the user asked for onto what the daemon reported, dropping a guess once the daemon agrees or has had long enough to be wrong.
    pub(crate) fn reconcile(&mut self, mut state: PlayerState) -> PlayerState {
        let now = Instant::now();

        if let Some((volume, until)) = self.guessed_volume {
            if now >= until || state.volume == volume {
                self.guessed_volume = None;
            } else {
                state.volume = volume;
            }
        }

        if let Some((playing, until)) = self.guessed_playing {
            if now >= until || state.is_playing == playing {
                self.guessed_playing = None;
            } else {
                state.is_playing = playing;
                // Buffering would draw its own marker and hide the change.
                state.is_buffering = false;
            }
        }

        // Kept for as long as the track is, rather than for a moment: nothing
        // will ever come back to confirm it. A change of track drops it.
        match &self.set_like {
            Some((video_id, liked)) if *video_id == state.video_id => state.liked = Some(*liked),
            Some(_) => self.set_like = None,
            None => {}
        }

        if let Some((position, until)) = self.guessed_position {
            // Position never stops moving, so "agrees" means close enough, not equal.
            if now >= until || state.position.abs_diff(position) <= 2 {
                self.guessed_position = None;
            } else {
                state.position = position;
            }
        }

        state
    }
}

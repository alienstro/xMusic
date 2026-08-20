//! Interface state and key handling; knows nothing about HTTP, and only emits [`Command`]s and consumes [`Event`]s.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::widgets::ListState;

use crate::client::{Client, Command, Event, PlayerState, SearchResult, SearchState};

const SEEK_STEP: i64 = 5;
const VOLUME_STEP: i64 = 5;

/// How long a local change is trusted over the daemon's report: the page's answer has to travel back through IPC and a poll, and redrawing the old value meanwhile is what makes a working control feel broken.
const OPTIMISM: Duration = Duration::from_millis(1500);

pub enum Mode {
    Normal,
    Editing,
    /// Waiting for a y/n answer before stopping the daemon.
    ConfirmStopDaemon,
}

pub struct App {
    pub mode: Mode,
    pub input: String,
    pub results: Vec<SearchResult>,
    pub list: ListState,
    pub player: PlayerState,
    pub status: String,
    pub online: bool,
    pub searching: bool,
    pub should_quit: bool,
    /// When the interface opened, which is all a spinner needs to turn at one speed whatever the redraw rate.
    pub started: Instant,

    client: Client,
    /// Sequence number of the results on screen, so the selection only resets when they are genuinely new.
    shown_search: u64,
    stop_daemon_then_quit: bool,
    /// Values the user has just asked for, held until the daemon confirms them.
    guessed_volume: Option<(u32, Instant)>,
    guessed_position: Option<(u32, Instant)>,
    guessed_playing: Option<(bool, Instant)>,
    /// The track Enter asked for, held until the daemon reports it playing; a title rather than a flag, because the player bar still describes the old track for about a second.
    awaiting: Option<String>,
}

impl App {
    pub fn new(client: Client) -> Self {
        Self {
            mode: Mode::Normal,
            input: String::new(),
            results: Vec::new(),
            list: ListState::default(),
            player: PlayerState::default(),
            status: "Press / to search".into(),
            online: false,
            searching: false,
            should_quit: false,
            started: Instant::now(),
            client,
            shown_search: 0,
            stop_daemon_then_quit: false,
            guessed_volume: None,
            guessed_position: None,
            guessed_playing: None,
            awaiting: None,
        }
    }

    // --------------------------------------------------------------- events ---

    pub fn absorb_events(&mut self) {
        for event in self.client.drain() {
            match event {
                Event::State(state) => {
                    self.online = true;
                    let was = self.player.video_id.clone();
                    self.player = self.reconcile(state);
                    // Any change of track ends the wait, so a request the page ignored cannot spin forever.
                    if self.player.video_id != was {
                        self.awaiting = None;
                    }
                }
                Event::Search(search) => self.apply_search(search),
                Event::Unreachable(message) => {
                    self.online = false;
                    // While deliberately stopping the daemon, unreachable is the goal, not an error.
                    if !self.stop_daemon_then_quit {
                        self.status = message;
                    }
                }
                Event::Failed(message) | Event::Notice(message) => self.status = message,
                Event::DaemonStopped(message) => {
                    self.status = message;
                    self.online = false;
                    if self.stop_daemon_then_quit {
                        self.should_quit = true;
                    }
                }
                Event::DaemonStopFailed(message) => {
                    self.stop_daemon_then_quit = false;
                    self.status = message;
                }
            }
        }
    }

    /// Overlays what the user asked for onto what the daemon reported, dropping a guess once the daemon agrees or has had long enough to be wrong.
    fn reconcile(&mut self, mut state: PlayerState) -> PlayerState {
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

    fn apply_search(&mut self, search: SearchState) {
        self.searching = search.pending;
        if search.seq == self.shown_search {
            return;
        }
        if search.pending {
            // The query is accepted but the results are in flight; keep the old list rather than blanking it.
            return;
        }
        self.shown_search = search.seq;
        self.results = search.results;
        self.list
            .select((!self.results.is_empty()).then_some(0));
        self.status = match search.error {
            Some(error) => format!("Search failed: {error}"),
            None if self.results.is_empty() => format!("No results for \"{}\"", search.query),
            None => format!("{} results for \"{}\"", self.results.len(), search.query),
        };
    }

    /// Whether the player is waiting on something rather than playing.
    pub fn is_loading(&self) -> bool {
        self.online
            && (self.awaiting.is_some() || self.player.is_buffering || !self.player.ready)
    }

    /// The title to show while a requested track has not started yet.
    pub fn loading_title(&self) -> Option<&str> {
        self.awaiting.as_deref()
    }

    // ------------------------------------------------------------------ keys ---

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match self.mode {
            Mode::Normal => self.handle_normal_key(code, modifiers),
            Mode::Editing => self.handle_editing_key(code),
            Mode::ConfirmStopDaemon => self.handle_confirm_key(code),
        }
    }

    fn handle_normal_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('Q') => {
                self.mode = Mode::ConfirmStopDaemon;
                self.status = "Stop the daemon and quit? Playback will end. (y/n)".into();
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true
            }

            KeyCode::Char('/') => {
                self.mode = Mode::Editing;
                self.input.clear();
            }

            KeyCode::Char(' ') => self.toggle_play(),
            KeyCode::Char('n') => self.client.send(Command::Transport("next")),
            KeyCode::Char('p') => self.client.send(Command::Transport("prev")),

            KeyCode::Left | KeyCode::Char('h') => self.nudge_position(-SEEK_STEP),
            KeyCode::Right | KeyCode::Char('l') => self.nudge_position(SEEK_STEP),
            KeyCode::Char('+') | KeyCode::Char('=') => self.nudge_volume(VOLUME_STEP),
            KeyCode::Char('-') | KeyCode::Char('_') => self.nudge_volume(-VOLUME_STEP),

            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Enter => self.play_selected(),

            // Google refuses sign-in in an embedded webview, so it happens in the user's browser and this copies the result across.
            KeyCode::Char('L') => {
                self.client.send(Command::SignIn);
                self.status = "Reading your browser session - macOS may ask for keychain permission".into();
            }
            // The player window is only worth looking at when the page itself has gone wrong.
            KeyCode::Char('W') => {
                self.client.send(Command::ShowWindow);
                self.status = "Showing the player window - press H to hide it".into();
            }
            KeyCode::Char('H') => {
                self.client.send(Command::HideWindow);
                self.status = "Player window hidden".into();
            }
            _ => {}
        }
    }

    fn handle_editing_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.submit_search(),
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.input.clear();
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn handle_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.stop_daemon_then_quit = true;
                self.status = "Stopping the daemon...".into();
                self.client.send(Command::StopDaemon);
                self.mode = Mode::Normal;
            }
            _ => {
                self.mode = Mode::Normal;
                self.status = "Cancelled - daemon left running".into();
            }
        }
    }

    // ---------------------------------------------------------------- actions ---

    fn submit_search(&mut self) {
        let query = self.input.trim().to_string();
        self.mode = Mode::Normal;
        if query.is_empty() {
            self.status = "Empty query".into();
            return;
        }
        self.status = format!("Searching for \"{query}\"...");
        self.searching = true;
        self.client.send(Command::Search(query));
        self.input.clear();
    }

    fn toggle_play(&mut self) {
        let playing = !self.player.is_playing;
        self.player.is_playing = playing;
        self.player.is_buffering = false;
        self.guessed_playing = Some((playing, Instant::now() + OPTIMISM));
        self.client.send(Command::Transport("play_pause"));
    }

    fn nudge_position(&mut self, delta: i64) {
        if self.player.duration == 0 {
            self.status = "Nothing loaded to seek in".into();
            return;
        }
        let target = (self.player.position as i64 + delta)
            .clamp(0, self.player.duration as i64) as u32;
        self.player.position = target;
        self.guessed_position = Some((target, Instant::now() + OPTIMISM));
        self.client.send(Command::Seek { delta });
    }

    fn nudge_volume(&mut self, delta: i64) {
        let target = (self.player.volume as i64 + delta).clamp(0, 100) as u32;
        self.player.volume = target;
        self.guessed_volume = Some((target, Instant::now() + OPTIMISM));
        self.client.send(Command::Volume { delta });
    }

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let last = self.results.len() - 1;
        let current = self.list.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, last as isize) as usize;
        self.list.select(Some(next));
    }

    fn play_selected(&mut self) {
        let Some(result) = self.list.selected().and_then(|i| self.results.get(i)) else {
            self.status = "Nothing selected".into();
            return;
        };
        self.status = format!("Playing {} - {}", result.artist, result.title);
        // The new track starts at zero; a guess about the old one is now a lie.
        self.guessed_position = None;
        self.awaiting = Some(result.title.clone());
        self.client.send(Command::Play(result.video_id.clone()));
    }
}

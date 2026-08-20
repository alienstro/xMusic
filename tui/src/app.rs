//! Interface state and key handling. Knows nothing about HTTP; it emits
//! [`Command`]s and consumes [`Event`]s.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::widgets::ListState;

use crate::client::{Client, Command, Event, PlayerState, SearchResult, SearchState};

const SEEK_STEP: i64 = 5;
const VOLUME_STEP: i64 = 5;

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

    client: Client,
    /// Sequence number of the search whose results are on screen, so the
    /// selection is only reset when the results are genuinely new.
    shown_search: u64,
    stop_daemon_then_quit: bool,
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
            client,
            shown_search: 0,
            stop_daemon_then_quit: false,
        }
    }

    // --------------------------------------------------------------- events ---

    pub fn absorb_events(&mut self) {
        for event in self.client.drain() {
            match event {
                Event::State(state) => {
                    self.online = true;
                    self.player = state;
                }
                Event::Search(search) => self.apply_search(search),
                Event::Unreachable(message) => {
                    self.online = false;
                    // While deliberately stopping the daemon, an unreachable
                    // daemon is the goal, not an error worth reporting.
                    if !self.stop_daemon_then_quit {
                        self.status = message;
                    }
                }
                Event::Failed(message) => self.status = message,
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

    fn apply_search(&mut self, search: SearchState) {
        self.searching = search.pending;
        if search.seq == self.shown_search {
            return;
        }
        if search.pending {
            // The daemon has accepted the query but the results are still in
            // flight; keep showing the old list rather than blanking it.
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

            KeyCode::Char(' ') => self.client.send(Command::Transport("play_pause")),
            KeyCode::Char('n') => self.client.send(Command::Transport("next")),
            KeyCode::Char('p') => self.client.send(Command::Transport("prev")),

            KeyCode::Left | KeyCode::Char('h') => {
                self.client.send(Command::Seek { delta: -SEEK_STEP })
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.client.send(Command::Seek { delta: SEEK_STEP })
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.client.send(Command::Volume { delta: VOLUME_STEP })
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.client.send(Command::Volume { delta: -VOLUME_STEP })
            }

            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Enter => self.play_selected(),

            // Sign-in: the daemon's window is hidden, so reveal it on demand.
            KeyCode::Char('L') => {
                self.client.send(Command::ShowWindow);
                self.status = "Showing the player window - sign in, then press H".into();
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
        self.client.send(Command::Play(result.video_id.clone()));
    }
}

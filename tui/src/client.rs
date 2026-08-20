//! Background HTTP worker.
//!
//! All daemon traffic happens on its own thread so a slow or dead daemon can
//! never stall the render loop. The interface sends [`Command`]s and drains
//! [`Event`]s; it never blocks on the network.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::daemon::BASE_URL;

const POLL_INTERVAL: Duration = Duration::from_millis(400);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(3000);
const COMMAND_QUEUE_CAPACITY: usize = 8;
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerState {
    pub ready: bool,
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub byline: String,
    pub diagnostic: String,
    pub is_playing: bool,
    pub is_buffering: bool,
    pub position: u32,
    pub duration: u32,
    pub volume: u32,
    pub muted: bool,
    pub logged_in: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchResult {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchState {
    pub seq: u64,
    pub query: String,
    pub pending: bool,
    pub error: Option<String>,
    pub results: Vec<SearchResult>,
}

#[derive(Debug)]
pub enum Command {
    Search(String),
    Play(String),
    Transport(&'static str),
    Seek { delta: i64 },
    Volume { delta: i64 },
    ShowWindow,
    HideWindow,
    StopDaemon,
}

#[derive(Debug)]
pub enum Event {
    State(PlayerState),
    Search(SearchState),
    /// The daemon could not be reached; carries the reason.
    Unreachable(String),
    /// A command failed. Shown on the status line.
    Failed(String),
    DaemonStopped(String),
    DaemonStopFailed(String),
}

pub struct Client {
    commands: SyncSender<Command>,
    stop: SyncSender<()>,
    events: Receiver<Event>,
}

impl Client {
    pub fn spawn() -> Self {
        let (command_tx, command_rx) = mpsc::sync_channel::<Command>(COMMAND_QUEUE_CAPACITY);
        let (stop_tx, stop_rx) = mpsc::sync_channel::<()>(1);
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        std::thread::Builder::new()
            .name("daemon-client".into())
            .spawn(move || worker(command_rx, stop_rx, event_tx))
            .expect("failed to spawn client thread");
        Self {
            commands: command_tx,
            stop: stop_tx,
            events: event_rx,
        }
    }

    pub fn send(&self, command: Command) {
        if matches!(command, Command::StopDaemon) {
            let _ = self.stop.try_send(());
        } else {
            // Key repeat can produce commands faster than HTTP can settle them.
            // A bounded queue drops excess repeats instead of replaying stale
            // volume or seek changes long after the key was released.
            let _ = self.commands.try_send(command);
        }
    }

    pub fn drain(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

fn worker(commands: Receiver<Command>, stop: Receiver<()>, events: Sender<Event>) {
    let mut next_poll = Instant::now();
    loop {
        if stop.try_recv().is_ok() {
            match crate::daemon::stop() {
                Ok(message) => {
                    let _ = events.send(Event::DaemonStopped(message));
                }
                Err(message) => {
                    let _ = events.send(Event::DaemonStopFailed(message));
                    continue;
                }
            }
            return;
        }

        let wait = next_poll
            .saturating_duration_since(Instant::now())
            .min(STOP_CHECK_INTERVAL);
        match commands.recv_timeout(wait) {
            Ok(command) => {
                match dispatch(&command) {
                    Err(message) => {
                        let _ = events.send(Event::Failed(message));
                    }
                    Ok(()) => {}
                }
                // Reflect the result promptly rather than waiting out the poll.
                next_poll = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {
                if Instant::now() >= next_poll {
                    poll(&events);
                    next_poll = Instant::now() + POLL_INTERVAL;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn dispatch(command: &Command) -> Result<(), String> {
    match command {
        Command::Search(query) => post("/search", serde_json::json!({ "query": query })),
        Command::Play(video_id) => post("/play", serde_json::json!({ "videoId": video_id })),
        Command::Transport(action) => post("/control", serde_json::json!({ "action": action })),
        Command::Seek { delta } => post("/seek", serde_json::json!({ "delta": delta })),
        Command::Volume { delta } => post("/volume", serde_json::json!({ "delta": delta })),
        Command::ShowWindow => post("/show-window", serde_json::json!({})),
        Command::HideWindow => post("/hide-window", serde_json::json!({})),
        Command::StopDaemon => crate::daemon::stop().map(|_| ()),
    }
}

fn poll(events: &Sender<Event>) {
    match get::<PlayerState>("/state") {
        Ok(state) => {
            let _ = events.send(Event::State(state));
        }
        Err(message) => {
            let _ = events.send(Event::Unreachable(message));
            // No point asking for search results from a daemon that just failed.
            return;
        }
    }
    if let Ok(search) = get::<SearchState>("/search-results") {
        let _ = events.send(Event::Search(search));
    }
}

fn get<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let token = crate::daemon::control_token()?;
    ureq::get(&format!("{BASE_URL}{path}"))
        .set(crate::daemon::AUTH_HEADER, &token)
        .timeout(REQUEST_TIMEOUT)
        .call()
        .map_err(describe)?
        .into_json::<T>()
        .map_err(|error| format!("{path}: malformed response: {error}"))
}

fn post(path: &str, body: serde_json::Value) -> Result<(), String> {
    let token = crate::daemon::control_token()?;
    ureq::post(&format!("{BASE_URL}{path}"))
        .set(crate::daemon::AUTH_HEADER, &token)
        .timeout(REQUEST_TIMEOUT)
        .send_json(body)
        .map(|_| ())
        .map_err(describe)
}

fn describe(error: ureq::Error) -> String {
    match error {
        // The daemon answers with {"ok":false,"error":"..."} on failure; surface
        // that instead of a bare status code.
        ureq::Error::Status(code, response) => match response.into_json::<serde_json::Value>() {
            Ok(body) => body
                .get("error")
                .and_then(|value| value.as_str())
                .map(|message| message.to_string())
                .unwrap_or_else(|| format!("daemon returned HTTP {code}")),
            Err(_) => format!("daemon returned HTTP {code}"),
        },
        ureq::Error::Transport(transport) => format!("daemon unreachable: {transport}"),
    }
}

//! Background HTTP worker: all daemon traffic runs on its own thread so a slow or dead daemon can never stall the render loop, and the interface only sends [`Command`]s and drains [`Event`]s.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::daemon::BASE_URL;

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(3000);
const COMMAND_QUEUE_CAPACITY: usize = 64;
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(100);

/// How long a burst of seek keys is collected before one seek is sent, because every `seekTo` re-buffers the stream and ten held-down presses would be ten stalls; deltas compose, so the burst becomes one seek.
const SEEK_DEBOUNCE: Duration = Duration::from_millis(140);

/// The same for volume, which is cheap on the page but still not worth one request per key repeat.
const VOLUME_DEBOUNCE: Duration = Duration::from_millis(60);

/// How long to let the page apply a command before reading state back.
const SETTLE: Duration = Duration::from_millis(60);

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
    SignIn,
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
    /// Something worth saying that is not a failure.
    Notice(String),
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
            // The worker folds relative commands together rather than dropping them, so a held-down key lands where the user aimed.
            let _ = self.commands.try_send(command);
        }
    }

    pub fn drain(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

fn worker(commands: Receiver<Command>, stop: Receiver<()>, events: Sender<Event>) {
    let mut next_poll = Instant::now();
    // Relative changes waiting to be sent as one, and when to send them.
    let mut seek = Pending::new(SEEK_DEBOUNCE);
    let mut volume = Pending::new(VOLUME_DEBOUNCE);

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

        // Wake for whichever comes first: the next poll, a debounce expiring, or the stop check.
        let now = Instant::now();
        let mut wait = next_poll
            .saturating_duration_since(now)
            .min(STOP_CHECK_INTERVAL);
        for due in [seek.due, volume.due].into_iter().flatten() {
            wait = wait.min(due.saturating_duration_since(now));
        }

        match commands.recv_timeout(wait) {
            Ok(Command::Seek { delta }) => seek.add(delta),
            Ok(Command::Volume { delta }) => volume.add(delta),
            // Sign-in reads a keychain and copies a database, far too slow to hold up polling and unordered with respect to the page anyway.
            Ok(Command::SignIn) => {
                let events = events.clone();
                std::thread::spawn(move || {
                    let _ = events.send(match sign_in() {
                        Ok(message) => Event::Notice(message),
                        Err(message) => Event::Failed(message),
                    });
                });
            }
            Ok(command) => {
                run(&command, &events);
                next_poll = Instant::now() + SETTLE;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if let Some(delta) = seek.take_if_due() {
            run(&Command::Seek { delta }, &events);
            next_poll = Instant::now() + SETTLE;
        }
        if let Some(delta) = volume.take_if_due() {
            run(&Command::Volume { delta }, &events);
            next_poll = Instant::now() + SETTLE;
        }

        if Instant::now() >= next_poll {
            poll(&events);
            next_poll = Instant::now() + POLL_INTERVAL;
        }
    }
}

/// A relative change accumulated across a burst of key repeats.
struct Pending {
    total: i64,
    due: Option<Instant>,
    debounce: Duration,
}

impl Pending {
    fn new(debounce: Duration) -> Self {
        Self { total: 0, due: None, debounce }
    }

    fn add(&mut self, delta: i64) {
        self.total += delta;
        // Each further press pushes the deadline out, so the request goes once the user stops.
        self.due = Some(Instant::now() + self.debounce);
    }

    fn take_if_due(&mut self) -> Option<i64> {
        if self.due.is_none_or(|due| Instant::now() < due) {
            return None;
        }
        self.due = None;
        // A burst that cancels itself out (right then left) needs no request.
        Some(std::mem::take(&mut self.total)).filter(|total| *total != 0)
    }
}

fn run(command: &Command, events: &Sender<Event>) {
    if let Err(message) = dispatch(command) {
        let _ = events.send(Event::Failed(message));
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
        Command::SignIn => sign_in().map(|_| ()),
        Command::StopDaemon => crate::daemon::stop().map(|_| ()),
    }
}

/// Hands the player a session from the user's browser, opening YouTube Music there when none is found, which is the ordinary first-time case rather than a failure.
pub fn sign_in() -> Result<String, String> {
    const YTM: &str = "https://music.youtube.com";

    let session = match crate::cookies::find_session() {
        Ok(session) => session,
        Err(reason) => {
            crate::cookies::open_in_browser(YTM)
                .map_err(|problem| format!("{reason}; {problem}"))?;
            return Err(format!(
                "{reason}. Opened YouTube Music in your browser - sign in there, then press L again"
            ));
        }
    };

    let count = session.cookies.len();
    let cookies: Vec<_> = session
        .cookies
        .iter()
        .map(|cookie| {
            serde_json::json!({
                "name": cookie.name,
                "value": cookie.value,
                "expires": cookie.expires,
            })
        })
        .collect();
    post("/cookies", serde_json::json!({ "cookies": cookies }))?;
    Ok(format!(
        "Imported {count} cookies from {} - reloading YouTube Music",
        session.browser
    ))
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
        // The daemon answers with {"ok":false,"error":"..."} on failure; surface that, not a bare status code.
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

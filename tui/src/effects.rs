//! What the interface asks the world to do, and the thread that does it.
//!
//! `update` never performs an effect; it returns them. This file is the only
//! place that runs one, on its own thread, so a slow or dead daemon can never
//! stall the render loop. Everything it learns comes back as a [`Message`].

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::time::{Duration, Instant};

use xmusic_protocol::{Cookie, CookiesRequest, Feed, TransportAction};

use crate::adapters::{browser_session, daemon_process, http_client};
use crate::update::Message;

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(3000);

/// What an effect gets instead when the daemon has unloaded its page: the daemon
/// holds the request open while YouTube Music loads again, which is a page load,
/// not a hang.
const WAKE_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const EFFECT_QUEUE_CAPACITY: usize = 64;
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(100);

/// How long a burst of seek keys is collected before one seek is sent, because every `seekTo` re-buffers the stream and ten held-down presses would be ten stalls; deltas compose, so the burst becomes one seek.
const SEEK_DEBOUNCE: Duration = Duration::from_millis(140);

/// The same for volume, which is cheap on the page but still not worth one request per key repeat.
const VOLUME_DEBOUNCE: Duration = Duration::from_millis(60);

/// How long to let the page apply an effect before reading state back.
const SETTLE: Duration = Duration::from_millis(60);

#[derive(Debug)]
pub enum Effect {
    Search(String),
    Browse(Feed),
    OpenPlaylist(String),
    Like { video_id: String, liked: bool },
    Play(String),
    Transport(TransportAction),
    Seek { delta: i64 },
    Volume { delta: i64 },
    ShowWindow,
    HideWindow,
    SignIn,
    StopDaemon,
}

pub struct Runner {
    effects: SyncSender<Effect>,
    stop: SyncSender<()>,
    messages: Receiver<Message>,
}

impl Runner {
    pub fn spawn() -> Self {
        let (effect_tx, effect_rx) = mpsc::sync_channel::<Effect>(EFFECT_QUEUE_CAPACITY);
        let (stop_tx, stop_rx) = mpsc::sync_channel::<()>(1);
        let (message_tx, message_rx) = mpsc::channel::<Message>();
        std::thread::Builder::new()
            .name("effect-runner".into())
            .spawn(move || worker(effect_rx, stop_rx, message_tx))
            .expect("failed to spawn the effect runner");
        Self {
            effects: effect_tx,
            stop: stop_tx,
            messages: message_rx,
        }
    }

    pub fn send(&self, effect: Effect) {
        if matches!(effect, Effect::StopDaemon) {
            let _ = self.stop.try_send(());
        } else {
            // The worker folds relative effects together rather than dropping them, so a held-down key lands where the user aimed.
            let _ = self.effects.try_send(effect);
        }
    }

    pub fn drain(&self) -> Vec<Message> {
        self.messages.try_iter().collect()
    }
}

fn worker(effects: Receiver<Effect>, stop: Receiver<()>, messages: Sender<Message>) {
    let mut next_poll = Instant::now();
    // Whether the daemon last reported its page unloaded, which is what decides
    // how long the next effect is given to answer.
    let mut hibernating = false;
    // Relative changes waiting to be sent as one, and when to send them.
    let mut seek = Pending::new(SEEK_DEBOUNCE);
    let mut volume = Pending::new(VOLUME_DEBOUNCE);

    loop {
        if stop.try_recv().is_ok() {
            match daemon_process::stop() {
                Ok(message) => {
                    let _ = messages.send(Message::DaemonStopped(message));
                }
                Err(message) => {
                    let _ = messages.send(Message::DaemonStopFailed(message));
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

        match effects.recv_timeout(wait) {
            Ok(Effect::Seek { delta }) => seek.add(delta),
            Ok(Effect::Volume { delta }) => volume.add(delta),
            // Signing in reads a keychain and copies a database, far too slow to hold up polling and unordered with respect to the page anyway.
            Ok(Effect::SignIn) => {
                let messages = messages.clone();
                std::thread::spawn(move || {
                    let _ = messages.send(match sign_in() {
                        Ok(message) => Message::Notice(message),
                        Err(message) => Message::Failed(message),
                    });
                });
            }
            Ok(effect) => {
                run(&effect, &messages, timeout_for(hibernating));
                next_poll = Instant::now() + SETTLE;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if let Some(delta) = seek.take_if_due() {
            run(&Effect::Seek { delta }, &messages, timeout_for(hibernating));
            next_poll = Instant::now() + SETTLE;
        }
        if let Some(delta) = volume.take_if_due() {
            run(&Effect::Volume { delta }, &messages, timeout_for(hibernating));
            next_poll = Instant::now() + SETTLE;
        }

        if Instant::now() >= next_poll {
            if let Some(unloaded) = poll(&messages) {
                hibernating = unloaded;
            }
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
        Self {
            total: 0,
            due: None,
            debounce,
        }
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

fn timeout_for(hibernating: bool) -> Duration {
    if hibernating {
        WAKE_REQUEST_TIMEOUT
    } else {
        REQUEST_TIMEOUT
    }
}

fn run(effect: &Effect, messages: &Sender<Message>, timeout: Duration) {
    let Err(message) = perform(effect, timeout) else {
        return;
    };
    // A like is the one effect whose result the interface has already drawn, so
    // its failure has to say which track to repaint, not just what went wrong.
    let _ = messages.send(match effect {
        Effect::Like { video_id, liked } => Message::LikeRejected {
            video_id: video_id.clone(),
            liked: !liked,
            message,
        },
        _ => Message::Failed(message),
    });
}

fn perform(effect: &Effect, timeout: Duration) -> Result<(), String> {
    match effect {
        Effect::Search(query) => http_client::search(query, timeout),
        Effect::Browse(feed) => http_client::browse(*feed, timeout),
        Effect::OpenPlaylist(browse_id) => http_client::open_playlist(browse_id, timeout),
        Effect::Like { video_id, liked } => http_client::like(video_id, *liked, timeout),
        Effect::Play(video_id) => http_client::play(video_id, timeout),
        Effect::Transport(action) => http_client::transport(*action, timeout),
        Effect::Seek { delta } => http_client::seek(*delta, timeout),
        Effect::Volume { delta } => http_client::volume(*delta, timeout),
        Effect::ShowWindow => http_client::set_window_visible(true, timeout),
        Effect::HideWindow => http_client::set_window_visible(false, timeout),
        Effect::SignIn => sign_in().map(|_| ()),
        Effect::StopDaemon => daemon_process::stop().map(|_| ()),
    }
}

/// Hands the player a session from the user's browser, opening YouTube Music there when none is found, which is the ordinary first-time case rather than a failure.
pub fn sign_in() -> Result<String, String> {
    const YTM: &str = "https://music.youtube.com";

    let session = match browser_session::find_session() {
        Ok(session) => session,
        Err(reason) => {
            browser_session::open_in_browser(YTM)
                .map_err(|problem| format!("{reason}; {problem}"))?;
            return Err(format!(
                "{reason}. Opened YouTube Music in your browser - sign in there, then press L again"
            ));
        }
    };

    let count = session.cookies.len();
    let request = CookiesRequest {
        cookies: session
            .cookies
            .iter()
            .map(|cookie| Cookie {
                name: cookie.name.clone(),
                value: cookie.value.clone(),
                expires: cookie.expires,
            })
            .collect(),
    };
    // Importing a session reloads the page, and may have to load it first if it
    // was unloaded while idle, so this one always gets the longer limit.
    http_client::import_cookies(&request, WAKE_REQUEST_TIMEOUT)?;
    Ok(format!(
        "Imported {count} cookies from {} - reloading YouTube Music",
        session.browser
    ))
}

/// Whether the daemon's page is unloaded, or `None` if the daemon did not answer.
fn poll(messages: &Sender<Message>) -> Option<bool> {
    let hibernating = match http_client::player_state() {
        Ok(state) => {
            let hibernating = state.hibernating;
            let _ = messages.send(Message::Player(state));
            hibernating
        }
        Err(message) => {
            let _ = messages.send(Message::Unreachable(message));
            // No point asking for a list from a daemon that just failed.
            return None;
        }
    };
    if let Ok(list) = http_client::list_state() {
        let _ = messages.send(Message::List(list));
    }
    Some(hibernating)
}

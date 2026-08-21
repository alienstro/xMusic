//! What the daemon does, with nothing in it about how it was asked.
//!
//! Every route, every timer, and every report from the page arrives here. This
//! is the one owner of the daemon's mutable state and the only place that
//! decides policy: which operations need the page awake and how awake, how a
//! list is sequenced, how long each kind of call is given, and when an idle page
//! is given back.
//!
//! It talks to the page through [`PageDriver`] and to callers through
//! [`PlayerError`], so it holds no HTTP types, no Tauri handles, and no
//! JavaScript.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use xmusic_protocol::{
    Cookie, Feed, ListItem, ListState, PlayerState, RelativeOr, Source, TransportAction,
};

use crate::lifecycle::{
    self, Need, PageLifecycle, ResumePoint, RESTORE_TIMEOUT, SWEEP_INTERVAL, WAKE_POLL,
    WAKE_TIMEOUT,
};
use crate::ports::{PageCommand, PageDestination, PageDriver, PageError, PageQuery};

/// How long a control call waits for the page: generous enough for a busy webview, short enough that a stuck page fails rather than hangs.
const DISPATCH_TIMEOUT: Duration = Duration::from_millis(2000);

/// A like is one request, but a signed one against Google rather than a method call on the player.
const LIKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a search may take before the list is failed rather than left pending.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Longer, because a feed is several requests: the page follows its own continuations and answers once. Must outlast `BROWSE_TIMEOUT_MS` in `inject.js`, so a slow feed is reported by the page rather than timing out underneath it.
const BROWSE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long YouTube Music is given to notice a freshly imported session before the page is reloaded onto it.
const COOKIE_SETTLE: Duration = Duration::from_millis(150);

/// Why an operation did not happen. Deliberately about the operation rather than
/// about a transport: the caller's adapter decides what each one looks like on
/// the wire.
#[derive(Clone, Debug)]
pub enum PlayerError {
    /// The request could not mean anything — a malformed id, a value out of range.
    BadRequest(String),
    /// The page is there and could not comply, and said why.
    Refused(String),
    /// The page could not be reached, or never came back.
    Unavailable(String),
    Timeout(String),
}

impl PlayerError {
    pub fn message(&self) -> &str {
        match self {
            PlayerError::BadRequest(message)
            | PlayerError::Refused(message)
            | PlayerError::Unavailable(message)
            | PlayerError::Timeout(message) => message,
        }
    }
}

impl From<PageError> for PlayerError {
    fn from(error: PageError) -> Self {
        match error {
            PageError::Refused(message) => PlayerError::Refused(message),
            PageError::Unreachable(message) => PlayerError::Unavailable(message),
            timeout @ PageError::Timeout(_) => PlayerError::Timeout(timeout.message()),
        }
    }
}

pub type Outcome<T> = Result<T, PlayerError>;

/// The one list the daemon holds, and the numbering that says which list it is.
///
/// Separate from the service because the watchdog that fails a list the page
/// never answered needs to outlive the request that armed it, and this is all it
/// needs: not the page, not the player, not the lifecycle.
#[derive(Default)]
struct ListSlot {
    state: Mutex<ListState>,
    seq: AtomicU64,
}

impl ListSlot {
    fn read(&self) -> ListState {
        self.state.lock().expect("list mutex poisoned").clone()
    }

    /// Numbers a new list and puts it in the slot, pending. The previous items
    /// stay on screen while the new ones load, rather than the list blanking.
    fn open(&self, source: Source, label: &str) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut state = self.state.lock().expect("list mutex poisoned");
        let previous = std::mem::take(&mut state.items);
        *state = ListState {
            seq,
            source,
            label: label.to_string(),
            pending: true,
            truncated: false,
            error: None,
            items: previous,
        };
        seq
    }

    fn finish(
        &self,
        seq: u64,
        label: String,
        items: Vec<ListItem>,
        truncated: bool,
        error: Option<String>,
    ) {
        let mut state = self.state.lock().expect("list mutex poisoned");
        // A newer list was asked for, or this one was already failed; either way the reply is stale.
        if seq != state.seq || !state.pending {
            return;
        }
        state.label = label;
        state.pending = false;
        state.truncated = truncated;
        state.error = error;
        state.items = items;
    }

    fn fail(&self, seq: u64, message: String) {
        let mut state = self.state.lock().expect("list mutex poisoned");
        if seq != state.seq || !state.pending {
            return;
        }
        state.pending = false;
        state.error = Some(message);
    }
}

pub struct PlayerService {
    page: Arc<dyn PageDriver>,
    player: Mutex<PlayerState>,
    list: Arc<ListSlot>,
    lifecycle: Mutex<PageLifecycle>,
    /// When the page was last needed. Reads of cached state do not count, or the client's own polling would keep the page alive forever.
    activity: Mutex<Instant>,
}

impl PlayerService {
    pub fn new(page: Arc<dyn PageDriver>) -> Self {
        Self {
            page,
            player: Mutex::default(),
            list: Arc::default(),
            lifecycle: Mutex::default(),
            // Counting from startup rather than from zero, so a daemon nobody talks to still waits out a full timeout before unloading the page it just loaded.
            activity: Mutex::new(Instant::now()),
        }
    }

    // ------------------------------------------------------------- reading ---

    /// The player as callers see it: what the page last reported, plus the one field the page has no way of knowing.
    pub fn player_state(&self) -> PlayerState {
        let mut player = self.player.lock().expect("player mutex poisoned").clone();
        player.hibernating = self.lifecycle().is_hibernating();
        player
    }

    pub fn list_state(&self) -> ListState {
        self.list.read()
    }

    // ------------------------------------------------------------- reports ---

    /// The page's own account of itself, on its timer and after every command.
    pub fn report_player(&self, state: PlayerState) {
        *self.player.lock().expect("player mutex poisoned") = state;
    }

    /// One finished list. Dropped if a newer one has been asked for since, or if this one has already been failed.
    pub fn report_list(
        &self,
        seq: u64,
        label: String,
        items: Vec<ListItem>,
        truncated: bool,
        error: Option<String>,
    ) {
        self.list.finish(seq, label, items, truncated, error);
    }

    // -------------------------------------------------------------- lists ---

    pub fn search(&self, query: &str) -> Outcome<u64> {
        let query = query.trim();
        if query.is_empty() {
            return Err(PlayerError::BadRequest("missing or empty \"query\"".into()));
        }
        self.ensure(Need::Api)?;
        self.open_list(
            Source::Search,
            query,
            SEARCH_TIMEOUT,
            "search",
            |seq| PageQuery::Search {
                seq,
                query: query.to_string(),
            },
        )
    }

    pub fn browse(&self, feed: Feed) -> Outcome<u64> {
        self.ensure(Need::Api)?;
        self.open_list(
            Source::Feed(feed),
            feed.title(),
            BROWSE_TIMEOUT,
            "feed",
            |seq| PageQuery::Browse { seq, feed },
        )
    }

    pub fn open_playlist(&self, browse_id: &str) -> Outcome<u64> {
        if !is_browse_id(browse_id) {
            return Err(PlayerError::BadRequest(
                "\"browseId\" is not a valid YouTube Music id".into(),
            ));
        }
        self.ensure(Need::Api)?;
        self.open_list(
            Source::Playlist(browse_id.to_string()),
            browse_id,
            BROWSE_TIMEOUT,
            "playlist",
            |seq| PageQuery::Playlist {
                seq,
                browse_id: browse_id.to_string(),
            },
        )
    }

    /// Opens a list, starts the call that fills it, and arms the watchdog that
    /// fails it if the page never answers. Every list route is this and a query.
    fn open_list(
        &self,
        source: Source,
        label: &str,
        timeout: Duration,
        noun: &str,
        query: impl FnOnce(u64) -> PageQuery,
    ) -> Outcome<u64> {
        let seq = self.list.open(source, label);

        if let Err(error) = self.page.start(query(seq)) {
            let error = PlayerError::from(error);
            self.list.fail(seq, error.message().to_string());
            return Err(error);
        }

        let watchdog = Arc::clone(&self.list);
        let complaint = format!("the {noun} did not answer within {}s", timeout.as_secs());
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            watchdog.fail(seq, complaint);
        });
        Ok(seq)
    }

    // ------------------------------------------------------------ commands ---

    pub fn play(&self, video_id: &str) -> Outcome<()> {
        let video_id = self.checked_video_id(video_id)?;
        self.command(
            Need::Player,
            PageCommand::Play { video_id },
            DISPATCH_TIMEOUT,
        )
    }

    pub fn transport(&self, action: TransportAction) -> Outcome<()> {
        self.command(
            Need::Player,
            PageCommand::Transport(action),
            DISPATCH_TIMEOUT,
        )
    }

    pub fn seek(&self, change: RelativeOr) -> Outcome<()> {
        self.command(Need::Player, PageCommand::Seek(change), DISPATCH_TIMEOUT)
    }

    pub fn volume(&self, change: RelativeOr) -> Outcome<()> {
        self.command(Need::Player, PageCommand::Volume(change), DISPATCH_TIMEOUT)
    }

    /// A like needs the InnerTube key rather than the player: it is a signed call against Google, not something done to the audio.
    pub fn like(&self, video_id: &str, liked: bool) -> Outcome<()> {
        let video_id = self.checked_video_id(video_id)?;
        self.command(
            Need::Api,
            PageCommand::Like { video_id, liked },
            LIKE_TIMEOUT,
        )
    }

    /// Takes a session copied out of the user's real browser, which is the only way this page can be signed in at all.
    pub fn import_cookies(self: &Arc<Self>, cookies: Vec<Cookie>) -> Outcome<()> {
        if cookies.is_empty() {
            return Err(PlayerError::BadRequest("expected a \"cookies\" array".into()));
        }
        self.command(Need::Api, PageCommand::AdoptCookies(cookies), DISPATCH_TIMEOUT)?;

        // YouTube Music decides who you are at boot, so the cookies mean nothing
        // until the page starts again.
        let service = Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(COOKIE_SETTLE);
            if let Err(error) = service.page.navigate(PageDestination::Music) {
                eprintln!(
                    "xmusic-player: could not reload after importing cookies: {}",
                    error.message()
                );
            }
        });
        Ok(())
    }

    pub fn set_window_visible(&self, visible: bool) -> Outcome<()> {
        self.page.set_visible(visible).map_err(PlayerError::from)
    }

    pub fn diagnose(&self) -> Outcome<String> {
        self.page.diagnose().map_err(PlayerError::from)
    }

    fn command(&self, need: Need, command: PageCommand, timeout: Duration) -> Outcome<()> {
        self.ensure(need)?;
        self.page
            .dispatch(command, timeout)
            .map_err(PlayerError::from)
    }

    // ----------------------------------------------------------- lifecycle ---

    pub fn lifecycle(&self) -> PageLifecycle {
        self.lifecycle.lock().expect("lifecycle mutex poisoned").clone()
    }

    fn set_lifecycle(&self, next: PageLifecycle) {
        *self.lifecycle.lock().expect("lifecycle mutex poisoned") = next;
    }

    /// Marks the page as in use, pushing the next hibernation out.
    pub fn touch(&self) {
        *self.activity.lock().expect("activity mutex poisoned") = Instant::now();
    }

    pub fn idle_for(&self) -> Duration {
        self.activity
            .lock()
            .expect("activity mutex poisoned")
            .elapsed()
    }

    /// Drops the page, remembering what was loaded so a later wake can put it back.
    pub fn unload(&self) -> Outcome<()> {
        let idle = self.idle_for();
        let player = self.player_state();
        let resume = (!player.video_id.is_empty()).then(|| ResumePoint {
            video_id: player.video_id,
            position: player.position,
        });

        self.page.navigate(PageDestination::Blank)?;
        self.set_lifecycle(PageLifecycle::Hibernating { resume });
        // Forgotten, so an unloaded page cannot be read as one still holding a track.
        *self.player.lock().expect("player mutex poisoned") = PlayerState::default();
        println!(
            "xmusic-player: page unloaded after {}s idle; it reloads on the next command",
            idle.as_secs()
        );
        Ok(())
    }

    /// Brings the page back if it was dropped, and does not return until it can
    /// do what the caller needs. A no-op beyond a timestamp when the page is
    /// loaded, which is the usual case.
    pub fn ensure(&self, need: Need) -> Outcome<()> {
        self.touch();
        let resume = match self.lifecycle() {
            PageLifecycle::Live => return Ok(()),
            PageLifecycle::Hibernating { resume } | PageLifecycle::Waking { resume } => resume,
        };

        // Moved out of Hibernating before the navigation, not after: the state
        // pump is what notices the page coming back, and it holds off while the
        // page is parked.
        self.set_lifecycle(PageLifecycle::Waking {
            resume: resume.clone(),
        });
        self.page.navigate(PageDestination::Music)?;

        let deadline = Instant::now() + WAKE_TIMEOUT;
        loop {
            let player = self.player_state();
            let usable = match need {
                Need::Api => player.api_ready,
                Need::Player => player.ready,
            };
            if usable {
                break;
            }
            if Instant::now() >= deadline {
                return Err(PlayerError::Unavailable(format!(
                    "the page did not finish loading within {}s",
                    WAKE_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(WAKE_POLL);
        }
        self.set_lifecycle(PageLifecycle::Live);
        // The page load itself took seconds; the timeout should start from now.
        self.touch();

        // Restored paused and where it was, so the command that triggered this
        // wake is still the one that decides what happens next. The queue the
        // track belonged to does not survive, and cannot: it lived in the old
        // document. A list does not need the track back at all.
        match resume {
            Some(resume) if need == Need::Player => self
                .page
                .dispatch(
                    PageCommand::Restore {
                        video_id: resume.video_id,
                        position: resume.position,
                    },
                    RESTORE_TIMEOUT,
                )
                .map_err(|error| {
                    PlayerError::Unavailable(format!(
                        "could not restore the last track: {}",
                        error.message()
                    ))
                }),
            _ => Ok(()),
        }
    }

    /// Loads the page back and waits until it can play, for the route that does nothing else.
    pub fn wake(&self) -> Outcome<()> {
        self.ensure(Need::Player)
    }

    /// Watches for idleness for the life of the process.
    pub fn sweep(self: Arc<Self>) {
        let Some(timeout) = lifecycle::idle_timeout() else {
            return;
        };
        loop {
            std::thread::sleep(SWEEP_INTERVAL);
            if self.lifecycle().is_hibernating() {
                continue;
            }
            // Playback is activity in its own right: a long album is not an idle daemon.
            let player = self.player_state();
            if player.is_playing || player.is_buffering {
                self.touch();
                continue;
            }
            if self.idle_for() < timeout {
                continue;
            }
            if let Err(error) = self.unload() {
                eprintln!("xmusic-player: could not unload the page: {}", error.message());
            }
        }
    }

    /// Asks the page for its state, unless it is parked — there is nothing to
    /// ask an unloaded page, and asking would only wake WebKit five times a
    /// second to say so.
    pub fn pump_state(&self) {
        if self.lifecycle().is_hibernating() {
            return;
        }
        self.page.pump_state();
    }

    // ------------------------------------------------------------- helpers ---

    fn checked_video_id(&self, candidate: &str) -> Outcome<String> {
        if is_video_id(candidate) {
            Ok(candidate.to_string())
        } else {
            Err(PlayerError::BadRequest(
                "\"videoId\" is not a valid YouTube id".into(),
            ))
        }
    }
}

/// YouTube ids are 11 URL-safe base64 characters, checked so a malformed one cannot reach the page as something other than an id.
fn is_video_id(candidate: &str) -> bool {
    candidate.len() == 11
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A playlist or album id as YouTube Music hands it out: `VL` + a playlist id, or an `MPRE`-prefixed album id.
fn is_browse_id(candidate: &str) -> bool {
    (2..=64).contains(&candidate.len())
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

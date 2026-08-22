//! Where the page is in its life, and what each caller needs of it.
//!
//! The webview is the expensive half of this program. YouTube Music's own
//! front-end holds a few hundred megabytes for as long as the document exists,
//! playing or not, and hiding the window does nothing about that. Parking the
//! webview on a blank document releases all of it and keeps the process, its
//! cookies, and the control server alive, at the price of a page load the next
//! time the daemon needs the page.
//!
//! This module holds the shape of that decision. Who acts on it is
//! `application.rs`; how the page is actually moved is the page adapter's
//! business.

use std::sync::OnceLock;
use std::time::Duration;

/// How long a wake waits for the page to become usable before it gives up and says so.
pub const WAKE_TIMEOUT: Duration = Duration::from_secs(30);
pub const WAKE_POLL: Duration = Duration::from_millis(100);

/// How long the page is given to put the previous track back. Longer than the
/// same limit inside `inject.js`, so a slow restore is reported by the page
/// rather than timing out underneath it.
pub const RESTORE_TIMEOUT: Duration = Duration::from_secs(25);

/// How often idleness is checked. Coarse on purpose: the timeout it enforces is minutes.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// What was loaded when the page was unloaded, so the next wake can put it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumePoint {
    pub video_id: String,
    /// Seconds.
    pub position: u32,
    /// The list this track was playing from, so the wake puts back what follows it as well as the track itself.
    pub queue: Vec<String>,
}

/// Where the page is, as far as the daemon controls it.
///
/// Deliberately not a place to record whether the page has finished booting:
/// that is the page's own report, and having two owners of one fact is how a
/// page ends up described as ready and hibernating at once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PageLifecycle {
    /// Loaded, or loading for the first time.
    #[default]
    Live,
    /// Parked on a blank document, with what to put back when it returns.
    Hibernating { resume: Option<ResumePoint> },
    /// Coming back: the navigation has been issued and the page is not usable yet.
    Waking { resume: Option<ResumePoint> },
}

impl PageLifecycle {
    /// What callers are told, which is one bit: is the page there or not.
    pub fn is_hibernating(&self) -> bool {
        matches!(self, PageLifecycle::Hibernating { .. })
    }
}

/// What a caller needs from the page.
///
/// A list only needs the InnerTube key out of `ytcfg`, which lands seconds
/// before the player element does, so browsing does not wait for playback
/// machinery it will never touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Api,
    Player,
}

/// How long the page may sit unused, from `XMUSIC_IDLE_TIMEOUT` in seconds.
/// `0` keeps the page loaded for the life of the daemon.
pub fn idle_timeout() -> Option<Duration> {
    static TIMEOUT: OnceLock<Option<Duration>> = OnceLock::new();
    *TIMEOUT.get_or_init(|| match std::env::var("XMUSIC_IDLE_TIMEOUT") {
        Err(_) => Some(DEFAULT_IDLE_TIMEOUT),
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(seconds) => Some(Duration::from_secs(seconds)),
            Err(_) => {
                eprintln!(
                    "xmusic-player: ignoring XMUSIC_IDLE_TIMEOUT={raw:?}: expected whole seconds"
                );
                Some(DEFAULT_IDLE_TIMEOUT)
            }
        },
    })
}

//! What the application needs from the page, stated without saying how.
//!
//! YouTube Music's own front-end is the most fragile dependency in this project.
//! Naming what the application asks of it here means a page change is contained
//! in the adapter that speaks to it — the webview and `inject.js` — instead of
//! spreading through the control server and out to the terminal.

use std::time::Duration;

use xmusic_protocol::{Cookie, Feed, RelativeOr, TransportAction};

/// Where the page should be. Blank is how the daemon gives the page's memory back without losing the process, its cookies, or the control server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageDestination {
    Music,
    Blank,
}

/// Something to do to the page that answers when it is done.
#[derive(Clone, Debug)]
pub enum PageCommand {
    Play { video_id: String },
    Transport(TransportAction),
    Seek(RelativeOr),
    Volume(RelativeOr),
    Like { video_id: String, liked: bool },
    AdoptCookies(Vec<Cookie>),
    /// Put back what was loaded before an idle unload, paused and where it was.
    Restore { video_id: String, position: u32 },
}

/// Something to ask the page for that answers later, by reporting a list.
///
/// These are not `PageCommand`s because they do not finish when the page
/// acknowledges them: a feed is several requests, and the answer arrives through
/// the report channel under the sequence number it was opened with.
#[derive(Clone, Debug)]
pub enum PageQuery {
    Search { seq: u64, query: String },
    Browse { seq: u64, feed: Feed },
    Playlist { seq: u64, browse_id: String },
}

/// Why the page did not do something. The distinction that matters is between a
/// page that answered "no" and a page that did not answer at all.
#[derive(Clone, Debug)]
pub enum PageError {
    /// The page is there and said why not, in words worth passing on.
    Refused(String),
    /// The page could not be reached at all.
    Unreachable(String),
    Timeout(Duration),
}

impl PageError {
    pub fn message(&self) -> String {
        match self {
            PageError::Refused(message) | PageError::Unreachable(message) => message.clone(),
            PageError::Timeout(timeout) => {
                format!("the page did not answer within {}ms", timeout.as_millis())
            }
        }
    }
}

/// The page, as the application sees it.
pub trait PageDriver: Send + Sync {
    fn navigate(&self, destination: PageDestination) -> Result<(), PageError>;

    /// Runs one command and waits for the page to say what happened.
    fn dispatch(&self, command: PageCommand, timeout: Duration) -> Result<(), PageError>;

    /// Starts one list-producing call. Returns as soon as the page has accepted it; the list itself arrives later.
    fn start(&self, query: PageQuery) -> Result<(), PageError>;

    fn set_visible(&self, visible: bool) -> Result<(), PageError>;

    /// Asks the page to report its state now, rather than waiting for its own timer.
    fn pump_state(&self);

    /// What the page sees of itself, for when the ordinary reporting path is the broken thing.
    fn diagnose(&self) -> Result<String, PageError>;
}

//! The page, as Tauri and `inject.js` actually provide it.
//!
//! Everything webview-shaped lives here: navigation, JavaScript evaluation, the
//! reply channel that turns a fire-and-forget `eval` into a call with an answer,
//! and the probe that reads the page's own view of itself when the ordinary
//! reporting path is the broken thing. Nothing above this file knows any of it.
//!
//! `eval` is how a Tauri host reaches its own webview; there is no other channel.
//! The discipline that makes it safe is that no caller-supplied value is ever
//! interpolated into script text directly: every one goes through `serde_json`,
//! which escapes it into a JavaScript string literal, so a hostile query or id
//! arrives at the page as data. That was verified end to end - see the injection
//! test in docs/verified-ytm-contract.md.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, WebviewWindow};

use xmusic_protocol::RelativeOr;

use crate::bridge::{Bridge, WaitError};
use crate::ports::{PageCommand, PageDestination, PageDriver, PageError, PageQuery};
use crate::{WINDOW_LABEL, YTM_URL};

/// Where the page goes to release its memory. Same webview, same cookie store, no document.
const BLANK_URL: &str = "about:blank";

/// How long the probe is given to apply its result to the URL before it is read back.
const PROBE_SETTLE: Duration = Duration::from_millis(300);

pub struct TauriPage {
    app: AppHandle,
    /// Outstanding commands waiting on the page to report back.
    bridge: Arc<Bridge>,
}

impl TauriPage {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            bridge: Arc::default(),
        }
    }

    /// The end of the reply channel the IPC command `report_command` settles.
    pub fn bridge(&self) -> Arc<Bridge> {
        Arc::clone(&self.bridge)
    }

    fn window(&self) -> Result<WebviewWindow, PageError> {
        self.app
            .get_webview_window(WINDOW_LABEL)
            .ok_or_else(|| PageError::Unreachable("player window is gone".to_string()))
    }

    fn eval(&self, script: &str) -> Result<(), PageError> {
        self.window()?
            .eval(script)
            .map_err(|error| PageError::Unreachable(format!("eval failed: {error}")))
    }
}

impl PageDriver for TauriPage {
    fn navigate(&self, destination: PageDestination) -> Result<(), PageError> {
        let url = match destination {
            PageDestination::Music => YTM_URL,
            PageDestination::Blank => BLANK_URL,
        };
        let parsed: tauri::Url = url
            .parse()
            .map_err(|error| PageError::Unreachable(format!("{url} is not a URL: {error}")))?;
        self.window()?
            .navigate(parsed)
            .map_err(|error| PageError::Unreachable(format!("navigation to {url} failed: {error}")))
    }

    /// `eval` returns as soon as the script is queued, so a command that only
    /// evaluated could not tell success from a no-op — setting the volume before
    /// the player exists used to answer 200 while doing nothing at all. Each call
    /// therefore carries an id the page quotes back when it has finished.
    fn dispatch(&self, command: PageCommand, timeout: Duration) -> Result<(), PageError> {
        let (action, args) = describe(command);
        let pending = self.bridge.dispatch();
        let script = format!(
            "window.__xmDispatch({}, {}, {})",
            pending.id(),
            literal(action),
            args
        );
        self.eval(&script)?;

        match pending.wait(timeout) {
            Ok(()) => Ok(()),
            Err(WaitError::Rejected(message)) => Err(PageError::Refused(message)),
            Err(WaitError::Timeout(timeout)) => Err(PageError::Timeout(timeout)),
            Err(WaitError::Disconnected) => {
                Err(PageError::Unreachable("the page dropped the request".to_string()))
            }
        }
    }

    fn start(&self, query: PageQuery) -> Result<(), PageError> {
        // Every value crossing into script text goes through serde_json, which
        // escapes it into a JavaScript string literal; interpolating a raw query
        // or id would be an injection hole.
        let script = match query {
            PageQuery::Search { seq, query } => {
                format!("window.__xmSearch({seq}, {})", literal(&query))
            }
            PageQuery::Browse { seq, feed } => {
                format!("window.__xmBrowse({seq}, {})", literal(feed.as_str()))
            }
            PageQuery::Playlist { seq, browse_id } => {
                format!("window.__xmPlaylist({seq}, {})", literal(&browse_id))
            }
        };
        self.eval(&script)
    }

    fn set_visible(&self, visible: bool) -> Result<(), PageError> {
        let window = self.window()?;
        let result = if visible {
            window.show().and_then(|()| window.set_focus())
        } else {
            window.hide()
        };
        result.map_err(|error| {
            PageError::Unreachable(format!("window visibility change failed: {error}"))
        })
    }

    fn pump_state(&self) {
        // Fails harmlessly while the page is still booting.
        let _ = self.eval("window.__xmReport && window.__xmReport()");
    }

    fn diagnose(&self) -> Result<String, PageError> {
        let window = self.window()?;
        self.eval(PROBE)?;
        // Give the webview a moment to apply the history entry before reading back.
        std::thread::sleep(PROBE_SETTLE);
        let url = window
            .url()
            .map_err(|error| PageError::Unreachable(format!("cannot read webview url: {error}")))?;
        let fragment = url.fragment().ok_or_else(|| {
            PageError::Unreachable("probe did not reach the page: no URL fragment".to_string())
        })?;
        let encoded = fragment
            .strip_prefix("xm=")
            .ok_or_else(|| PageError::Unreachable(format!("unexpected fragment: {fragment}")))?;
        percent_decode(encoded).map_err(PageError::Unreachable)
    }
}

fn literal(text: &str) -> String {
    serde_json::to_string(text).expect("string serialises")
}

fn change(value: RelativeOr) -> Value {
    json!({ "value": value.value(), "relative": value.is_relative() })
}

/// The wire form `inject.js` switches on. The one place the two vocabularies meet.
fn describe(command: PageCommand) -> (&'static str, Value) {
    match command {
        PageCommand::Play { video_id, queue } => {
            ("play", json!({ "videoId": video_id, "queue": queue }))
        }
        PageCommand::Transport(action) => ("transport", json!({ "action": action.as_str() })),
        PageCommand::Seek(value) => ("seek", change(value)),
        PageCommand::Volume(value) => ("volume", change(value)),
        PageCommand::Like { video_id, liked } => {
            ("like", json!({ "videoId": video_id, "liked": liked }))
        }
        PageCommand::AdoptCookies(cookies) => (
            "adopt_cookies",
            json!({ "cookies": serde_json::to_value(cookies).expect("cookies serialise") }),
        ),
        PageCommand::Restore {
            video_id,
            position,
            queue,
        } => (
            "restore",
            json!({ "videoId": video_id, "position": position, "queue": queue }),
        ),
    }
}

/// Reads the page's view of itself without using IPC, for when IPC is the broken
/// thing. The result comes out through the URL fragment because `eval` cannot
/// return a value.
const PROBE: &str = r#"
(() => {
  const report = {
    href: location.href,
    docTitle: document.title,
    tauri: typeof window.__TAURI__,
    tauriCore: window.__TAURI__ ? typeof window.__TAURI__.core : 'n/a',
    internals: typeof window.__TAURI_INTERNALS__,
    invoke: window.__TAURI_INTERNALS__ ? typeof window.__TAURI_INTERNALS__.invoke : 'n/a',
    injected: !!window.__xmInstalled,
    xmSearch: typeof window.__xmSearch,
    xmBrowse: typeof window.__xmBrowse,
    ytcfg: !!window.ytcfg,
    moviePlayer: !!document.querySelector('#movie_player'),
    ytmusicApp: !!document.querySelector('ytmusic-app'),
    lastError: window.__xmLastError || null,
    auth: window.__xmAuth || null,
  };
  history.replaceState(null, '', '#xm=' + encodeURIComponent(JSON.stringify(report)));
})()
"#;

/// Minimal percent-decoder: the probe payload is `encodeURIComponent` output, so only `%XX` escapes need undoing.
fn percent_decode(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| "malformed escape".to_string())?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| "malformed escape".to_string())?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|error| format!("probe was not valid UTF-8: {error}"))
}

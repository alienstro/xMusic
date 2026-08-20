//! xmusic-player: a windowless YouTube Music playback daemon.
//!
//! Loads music.youtube.com in a hidden webview and exposes a small HTTP control
//! API on 127.0.0.1:13723. The terminal client (xmusic) drives it over that
//! API; this process owns nothing but the page and the audio.

mod bridge;
mod pidfile;
mod server;
mod state;

use std::sync::Arc;

use tauri::{State, WebviewUrl, WebviewWindowBuilder};

use bridge::Outcome;
use state::{PlayerState, SearchResult, Shared};

pub const WINDOW_LABEL: &str = "player";

const YTM_URL: &str = "https://music.youtube.com";

/// YT Music serves a stripped "Your browser is deprecated" page - no player, no
/// ytcfg - to user agents it doesn't recognise, and the WKWebView default is not
/// known to be accepted. Pinning a current desktop Chrome UA avoids the whole
/// question. If playback ever reports `ready: false` forever, bump this.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";

const INJECT: &str = include_str!("inject.js");

#[tauri::command]
fn report_state(shared: State<Arc<Shared>>, state: PlayerState) {
    *shared.player.lock().expect("player mutex poisoned") = state;
}

#[tauri::command]
fn report_search_results(
    shared: State<Arc<Shared>>,
    seq: u64,
    query: String,
    results: Vec<SearchResult>,
    error: Option<String>,
) {
    shared.finish_search(seq, query, results, error);
}

/// Reports the result of one dispatched control call. See `bridge.rs` for why
/// this round trip exists at all.
#[tauri::command]
fn report_command(shared: State<Arc<Shared>>, id: u64, ok: bool, error: Option<String>) {
    shared.bridge.settle(id, Outcome { ok, error });
}

fn main() {
    let runtime = match pidfile::acquire() {
        Ok(runtime) => runtime,
        Err(message) => {
            eprintln!("xmusic-player: {message}");
            return;
        }
    };
    let control_token = runtime.token().to_string();
    let shared = Arc::new(Shared::default());
    let server_shared = Arc::clone(&shared);

    tauri::Builder::default()
        .manage(shared)
        .invoke_handler(tauri::generate_handler![
            report_state,
            report_search_results,
            report_command
        ])
        .setup(move |app| {
            // No Dock icon or menu bar: this is a daemon, not an app the user
            // switches to. The window can still be shown on demand for sign-in.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            WebviewWindowBuilder::new(
                app,
                WINDOW_LABEL,
                WebviewUrl::External(YTM_URL.parse().expect("YTM_URL is a valid URL")),
            )
            .title("xmusic-player")
            .inner_size(1280.0, 900.0)
            .visible(false)
            .user_agent(USER_AGENT)
            .initialization_script(INJECT)
            .build()?;

            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("control-server".into())
                .spawn(move || server::run(handle, server_shared, control_token))?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build xmusic-player")
        .run(|_app, _event| {});
}

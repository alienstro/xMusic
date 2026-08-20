//! xmusic-player: a windowless YouTube Music daemon that loads music.youtube.com in a hidden webview and exposes a control API on 127.0.0.1:13723, owning nothing but the page and the audio.

mod bridge;
mod pidfile;
mod server;
mod state;

use std::sync::Arc;
use std::time::Duration;

use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use bridge::Outcome;
use state::{PlayerState, SearchResult, Shared};

pub const WINDOW_LABEL: &str = "player";

const YTM_URL: &str = "https://music.youtube.com";

/// YT Music serves a player-less "browser is deprecated" page to user agents it does not recognise, so a current desktop Chrome UA is pinned here; bump it if playback ever reports `ready: false` forever.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";

const INJECT: &str = include_str!("inject.js");

/// How often the daemon asks the page for state: a hidden WKWebView has `setInterval` throttled to about a second, which leaves every reading visibly behind the key that changed it, and `eval` is not throttled.
const STATE_PUMP: Duration = Duration::from_millis(200);

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

/// Reports the result of one dispatched control call; see `bridge.rs` for why the round trip exists.
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
            // No Dock icon or menu bar: this is a daemon, not an app to switch to.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let window = WebviewWindowBuilder::new(
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

            // Closing the only window would end the process and the music with it, so the close button hides it instead.
            window.on_window_event({
                let window = window.clone();
                move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            });

            let pump = app.handle().clone();
            std::thread::Builder::new()
                .name("state-pump".into())
                .spawn(move || loop {
                    std::thread::sleep(STATE_PUMP);
                    if let Some(window) = pump.get_webview_window(WINDOW_LABEL) {
                        // Fails harmlessly while the page is still booting.
                        let _ = window.eval("window.__xmReport && window.__xmReport()");
                    }
                })?;

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

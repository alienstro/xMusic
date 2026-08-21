//! xmusic-player: a windowless YouTube Music daemon that loads
//! `music.youtube.com` in a hidden webview and exposes a control API on
//! 127.0.0.1:13723, owning nothing but the page and the audio.
//!
//! This file is the composition root and nothing else. It builds the webview,
//! wires the page adapter to the application service, and starts the three
//! inbound edges — the control server, the page's own IPC reports, and the
//! timers. What any of them then do is `application.rs`.

mod adapters;
mod application;
mod bridge;
mod lifecycle;
mod pidfile;
mod ports;

use std::sync::Arc;
use std::time::Duration;

use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use xmusic_protocol::{ListItem, PlayerState};

use adapters::tauri_page::TauriPage;
use application::PlayerService;
use bridge::{Bridge, Outcome};

pub const WINDOW_LABEL: &str = "player";

pub const YTM_URL: &str = "https://music.youtube.com";

/// YT Music serves a player-less "browser is deprecated" page to user agents it does not recognise, so a current desktop Chrome UA is pinned here; bump it if playback ever reports `ready: false` forever.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";

const INJECT: &str = include_str!("inject.js");

/// The injected script, with the one knob the page reads from the environment.
///
/// The page's artwork is the largest slice of its memory that is not audio, and
/// nothing in this process ever looks at a decoded thumbnail: the terminal draws
/// its own from the URLs InnerTube already returns. `XMUSIC_KEEP_IMAGES=1` puts
/// the page's images back, which is worth having when `/show-window` is being
/// used to see what YouTube Music is actually doing.
fn inject_script() -> String {
    let keep = std::env::var("XMUSIC_KEEP_IMAGES").is_ok_and(|value| !value.is_empty() && value != "0");
    format!("window.__xmKeepImages = {keep};\n{INJECT}")
}

/// How often the daemon asks the page for state: a hidden WKWebView has `setInterval` throttled to about a second, which leaves every reading visibly behind the key that changed it, and `eval` is not throttled.
const STATE_PUMP: Duration = Duration::from_millis(200);

// ------------------------------------------------------------ page reports ---
// The page's own inbound edge. Each of these does nothing but hand what arrived
// to the application; none of them decides anything.

#[tauri::command]
fn report_state(service: State<Arc<PlayerService>>, state: PlayerState) {
    service.report_player(state);
}

/// One finished list, under the sequence number it was opened with.
#[tauri::command]
fn report_list(
    service: State<Arc<PlayerService>>,
    seq: u64,
    label: String,
    items: Vec<ListItem>,
    truncated: bool,
    error: Option<String>,
) {
    service.report_list(seq, label, items, truncated, error);
}

/// The result of one dispatched command; see `bridge.rs` for why the round trip exists.
#[tauri::command]
fn report_command(bridge: State<Arc<Bridge>>, id: u64, ok: bool, error: Option<String>) {
    bridge.settle(id, Outcome { ok, error });
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

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            report_state,
            report_list,
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
            .initialization_script(inject_script())
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

            // The one place the page adapter is chosen. Everything above it is
            // written against `PageDriver` and would not notice another one.
            let page = Arc::new(TauriPage::new(app.handle().clone()));
            let bridge = page.bridge();
            let service = Arc::new(PlayerService::new(page));
            app.manage(Arc::clone(&service));
            app.manage(bridge);

            let pump = Arc::clone(&service);
            std::thread::Builder::new()
                .name("state-pump".into())
                .spawn(move || loop {
                    std::thread::sleep(STATE_PUMP);
                    pump.pump_state();
                })?;

            match lifecycle::idle_timeout() {
                Some(timeout) => println!(
                    "xmusic-player: the page unloads after {}s idle; XMUSIC_IDLE_TIMEOUT=0 keeps it loaded",
                    timeout.as_secs()
                ),
                None => println!("xmusic-player: idle unloading is disabled"),
            }
            let sweeper = Arc::clone(&service);
            std::thread::Builder::new()
                .name("hibernate-sweep".into())
                .spawn(move || sweeper.sweep())?;

            let server = Arc::clone(&service);
            std::thread::Builder::new()
                .name("control-server".into())
                .spawn(move || adapters::http::run(server, control_token))?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build xmusic-player")
        .run(|_app, _event| {});
}

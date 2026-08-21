//! One function: what a message does to the model, and what the world is asked
//! to do about it.
//!
//! Everything here is a state transition plus a list of effects. Nothing in this
//! file performs one, which is what makes an optimistic paint and its reversal
//! two ordinary transitions rather than a callback tangled into a request.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers};

use xmusic_protocol::{Feed, ListState, PlayerState, Source, TransportAction};

use crate::effects::Effect;
use crate::model::{Mode, Model, OPTIMISM};
use crate::panes::Pane;

const SEEK_STEP: i64 = 5;
const VOLUME_STEP: i64 = 5;

/// Everything that can happen to the interface, from the keyboard or from the daemon.
#[derive(Debug)]
pub enum Message {
    Key(KeyCode, KeyModifiers),
    Player(PlayerState),
    List(ListState),
    /// The daemon could not be reached; carries the reason.
    Unreachable(String),
    /// An effect failed. Shown on the status line.
    Failed(String),
    /// Something worth saying that is not a failure.
    Notice(String),
    /// YouTube Music refused a like, so the heart the interface already painted has to go back.
    LikeRejected {
        video_id: String,
        /// The state to restore, which is what it was before the keypress.
        liked: bool,
        message: String,
    },
    DaemonStopped(String),
    DaemonStopFailed(String),
}

pub fn update(model: &mut Model, message: Message) -> Vec<Effect> {
    match message {
        Message::Key(code, modifiers) => key(model, code, modifiers),

        Message::Player(state) => {
            model.online = true;
            let was = model.player.video_id.clone();
            model.player = model.reconcile(state);
            // Any change of track ends the wait, so a request the page ignored cannot spin forever.
            if model.player.video_id != was {
                model.awaiting = None;
            }
            Vec::new()
        }

        Message::List(list) => {
            apply_list(model, list);
            Vec::new()
        }

        Message::Unreachable(reason) => {
            model.online = false;
            // While deliberately stopping the daemon, unreachable is the goal, not an error.
            if !model.stop_daemon_then_quit {
                model.status = reason;
            }
            Vec::new()
        }

        Message::Failed(text) | Message::Notice(text) => {
            model.status = text;
            Vec::new()
        }

        Message::LikeRejected {
            video_id,
            liked,
            message,
        } => {
            paint_like(model, &video_id, Some(liked));
            model.status = format!("Could not change the like: {message}");
            Vec::new()
        }

        Message::DaemonStopped(text) => {
            model.status = text;
            model.online = false;
            if model.stop_daemon_then_quit {
                model.should_quit = true;
            }
            Vec::new()
        }

        Message::DaemonStopFailed(text) => {
            model.stop_daemon_then_quit = false;
            model.status = text;
            Vec::new()
        }
    }
}

// ------------------------------------------------------------------- lists ---

fn apply_list(model: &mut Model, list: ListState) {
    if list.pending {
        // Accepted but still loading; keep whatever is on screen rather than
        // blanking it, and only spin if it is this list the user is waiting on.
        model.loading_list = model.panes.awaiting(&list);
        return;
    }
    // The daemon serves its one list slot on every poll, so most of these are the
    // same list again; `accept` files only what is genuinely new, and says
    // whether it landed where the user is looking.
    if !model.panes.accept(&list) {
        return;
    }
    model.loading_list = false;

    let count = model.panes.visible().rows.len();
    // The daemon labels a drill-down with the id it was asked for, which is no
    // use on a status line; the name the row carried is what the user chose, and
    // is what the pane already knows it by.
    let name = model.panes.visible_title().to_string();
    model.status = match &list.error {
        Some(error) => format!("{} failed: {error}", list.source.noun()),
        None if count == 0 => empty_message(&list.source, &list.label),
        None if list.truncated => {
            format!("{count} loaded from {name} — more than xMusic loads at once")
        }
        None if matches!(list.source, Source::Search) => {
            format!("{count} results for \"{}\"", list.label)
        }
        None => format!("{count} in {name}"),
    };
}

/// An empty feed is a state, not a failure, and reads differently from one.
fn empty_message(source: &Source, label: &str) -> String {
    match source {
        Source::Search => format!("No results for \"{label}\""),
        Source::Feed(Feed::Liked) => "No liked songs yet".into(),
        Source::Feed(Feed::Playlists) => "No playlists yet".into(),
        Source::Feed(Feed::Albums) => "No saved albums yet".into(),
        Source::Feed(Feed::History) => "Nothing played yet".into(),
        Source::Playlist(_) => "Nothing in this list".into(),
    }
}

// -------------------------------------------------------------------- keys ---

fn key(model: &mut Model, code: KeyCode, modifiers: KeyModifiers) -> Vec<Effect> {
    match model.mode {
        Mode::Normal => normal_key(model, code, modifiers),
        Mode::Editing => editing_key(model, code),
        Mode::ConfirmStopDaemon => confirm_key(model, code),
    }
}

fn normal_key(model: &mut Model, code: KeyCode, modifiers: KeyModifiers) -> Vec<Effect> {
    match code {
        KeyCode::Char('q') => {
            model.should_quit = true;
            Vec::new()
        }
        KeyCode::Char('Q') => {
            model.mode = Mode::ConfirmStopDaemon;
            model.status = "Stop the daemon and quit? Playback will end. (y/n)".into();
            Vec::new()
        }
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            model.should_quit = true;
            Vec::new()
        }

        KeyCode::Char('/') => {
            model.mode = Mode::Editing;
            model.input.clear();
            Vec::new()
        }

        // Digits are unbound today, so panes can have them outright.
        KeyCode::Char(digit @ '1'..='5') => {
            switch_pane(model, Pane::ALL[digit as usize - '1' as usize])
        }
        KeyCode::Tab => step_pane(model, 1),
        KeyCode::BackTab => step_pane(model, -1),
        // Bound in `Mode::Editing` only, so normal-mode Esc is free for this.
        KeyCode::Esc => {
            if model.panes.leave() {
                model.status = pane_summary(model);
            }
            Vec::new()
        }
        KeyCode::Char('r') => reload(model),
        // Not `l`: that is seek-forward in the vim pairing with `h`, and `L` is sign-in.
        KeyCode::Char('f') => toggle_like(model),

        KeyCode::Char(' ') => toggle_play(model),
        KeyCode::Char('n') => vec![Effect::Transport(TransportAction::Next)],
        KeyCode::Char('p') => vec![Effect::Transport(TransportAction::Prev)],

        KeyCode::Left | KeyCode::Char('h') => nudge_position(model, -SEEK_STEP),
        KeyCode::Right | KeyCode::Char('l') => nudge_position(model, SEEK_STEP),
        KeyCode::Char('+') | KeyCode::Char('=') => nudge_volume(model, VOLUME_STEP),
        KeyCode::Char('-') | KeyCode::Char('_') => nudge_volume(model, -VOLUME_STEP),

        KeyCode::Down | KeyCode::Char('j') => {
            model.panes.move_cursor(1);
            Vec::new()
        }
        KeyCode::Up | KeyCode::Char('k') => {
            model.panes.move_cursor(-1);
            Vec::new()
        }
        // Contextual, because Enter already means play: on a track row it still
        // plays, and on a playlist or album — the only rows where playing one
        // thing makes no sense — it opens that list instead.
        KeyCode::Enter => activate_selected(model),

        // Google refuses sign-in in an embedded webview, so it happens in the user's browser and this copies the result across.
        KeyCode::Char('L') => {
            model.status =
                "Reading your browser session - macOS may ask for keychain permission".into();
            vec![Effect::SignIn]
        }
        // The player window is only worth looking at when the page itself has gone wrong.
        KeyCode::Char('W') => {
            model.status = "Showing the player window - press H to hide it".into();
            vec![Effect::ShowWindow]
        }
        KeyCode::Char('H') => {
            model.status = "Player window hidden".into();
            vec![Effect::HideWindow]
        }
        _ => Vec::new(),
    }
}

fn editing_key(model: &mut Model, code: KeyCode) -> Vec<Effect> {
    match code {
        KeyCode::Enter => submit_search(model),
        KeyCode::Esc => {
            model.mode = Mode::Normal;
            model.input.clear();
            Vec::new()
        }
        KeyCode::Backspace => {
            model.input.pop();
            Vec::new()
        }
        KeyCode::Char(character) => {
            model.input.push(character);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn confirm_key(model: &mut Model, code: KeyCode) -> Vec<Effect> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            model.stop_daemon_then_quit = true;
            model.status = "Stopping the daemon...".into();
            model.mode = Mode::Normal;
            vec![Effect::StopDaemon]
        }
        _ => {
            model.mode = Mode::Normal;
            model.status = "Cancelled - daemon left running".into();
            Vec::new()
        }
    }
}

// ----------------------------------------------------------------- actions ---

fn submit_search(model: &mut Model) -> Vec<Effect> {
    let query = model.input.trim().to_string();
    model.mode = Mode::Normal;
    model.input.clear();
    if query.is_empty() {
        model.status = "Empty query".into();
        return Vec::new();
    }
    // A search always answers into the search pane, so go there first.
    model.panes.switch(Pane::Search);
    model.panes.mark_visited();
    model.status = format!("Searching for \"{query}\"...");
    model.loading_list = true;
    vec![Effect::Search(query)]
}

fn switch_pane(model: &mut Model, pane: Pane) -> Vec<Effect> {
    if model.panes.switch(pane) {
        load_pane(model)
    } else {
        model.status = pane_summary(model);
        Vec::new()
    }
}

fn step_pane(model: &mut Model, delta: isize) -> Vec<Effect> {
    if model.panes.step(delta) {
        load_pane(model)
    } else {
        model.status = pane_summary(model);
        Vec::new()
    }
}

/// Asks for the active pane's feed. Search has nothing to load without a query, so it is simply marked as visited.
fn load_pane(model: &mut Model) -> Vec<Effect> {
    model.panes.mark_visited();
    let Some(feed) = model.panes.active.feed() else {
        model.status = "Press / to search".into();
        return Vec::new();
    };
    model.status = format!("Loading {}...", model.panes.active.title());
    model.loading_list = true;
    vec![Effect::Browse(feed)]
}

/// What a cached pane says for itself when it is stepped back into.
fn pane_summary(model: &Model) -> String {
    match model.panes.visible().rows.len() {
        0 => format!("{} — press r to load", model.panes.visible_title()),
        count => format!("{count} in {}", model.panes.visible_title()),
    }
}

fn reload(model: &mut Model) -> Vec<Effect> {
    model.loading_list = true;
    if let Some(browse_id) = model.panes.drilled_id() {
        let browse_id = browse_id.to_string();
        model.status = "Reloading...".into();
        return vec![Effect::OpenPlaylist(browse_id)];
    }
    match model.panes.active.feed() {
        Some(feed) => {
            model.status = format!("Reloading {}...", model.panes.active.title());
            vec![Effect::Browse(feed)]
        }
        None => {
            model.loading_list = false;
            model.status = "Nothing to reload — press / to search".into();
            Vec::new()
        }
    }
}

fn activate_selected(model: &mut Model) -> Vec<Effect> {
    let Some(row) = model.panes.visible().selected().cloned() else {
        model.status = "Nothing selected".into();
        return Vec::new();
    };
    if row.is_openable() {
        model.status = format!("Opening {}...", row.title);
        model.loading_list = true;
        model.panes.drill(row.browse_id.clone(), row.title);
        return vec![Effect::OpenPlaylist(row.browse_id)];
    }
    model.status = format!("Playing {} - {}", row.artist, row.title);
    // The new track starts at zero; a guess about the old one is now a lie.
    model.guessed_position = None;
    model.awaiting = Some(row.title);
    vec![Effect::Play(row.video_id)]
}

/// Toggles the like on the selected row, or on the now-playing track where there is no row to act on.
fn toggle_like(model: &mut Model) -> Vec<Effect> {
    let selected = model
        .panes
        .visible()
        .selected()
        .filter(|row| !row.video_id.is_empty())
        .map(|row| (row.video_id.clone(), row.liked));
    let (video_id, current) =
        selected.unwrap_or_else(|| (model.player.video_id.clone(), model.player.liked));

    if video_id.is_empty() {
        model.status = "Nothing to like".into();
        return Vec::new();
    }
    // An unknown state is treated as not liked, so the first press likes.
    let liked = !current.unwrap_or(false);
    paint_like(model, &video_id, Some(liked));
    model.status = if liked { "Liked".into() } else { "Like removed".into() };
    vec![Effect::Like { video_id, liked }]
}

/// Paints a like everywhere that track shows, so the heart answers the keypress rather than the next poll.
fn paint_like(model: &mut Model, video_id: &str, liked: Option<bool>) {
    model.panes.paint_like(video_id, liked);
    if model.player.video_id == video_id {
        model.player.liked = liked;
        model.set_like = liked.map(|liked| (video_id.to_string(), liked));
    }
}

fn toggle_play(model: &mut Model) -> Vec<Effect> {
    let playing = !model.player.is_playing;
    model.player.is_playing = playing;
    model.player.is_buffering = false;
    model.guessed_playing = Some((playing, Instant::now() + OPTIMISM));
    vec![Effect::Transport(TransportAction::PlayPause)]
}

fn nudge_position(model: &mut Model, delta: i64) -> Vec<Effect> {
    if model.player.duration == 0 {
        model.status = "Nothing loaded to seek in".into();
        return Vec::new();
    }
    let target =
        (model.player.position as i64 + delta).clamp(0, model.player.duration as i64) as u32;
    model.player.position = target;
    model.guessed_position = Some((target, Instant::now() + OPTIMISM));
    vec![Effect::Seek { delta }]
}

fn nudge_volume(model: &mut Model, delta: i64) -> Vec<Effect> {
    let target = (model.player.volume as i64 + delta).clamp(0, 100) as u32;
    model.player.volume = target;
    model.guessed_volume = Some((target, Instant::now() + OPTIMISM));
    vec![Effect::Volume { delta }]
}

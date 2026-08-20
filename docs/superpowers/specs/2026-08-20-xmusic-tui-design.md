# xmusic design

Date: 2026-08-20
Status: implemented

## Problem

Play music from a real YouTube Music account, driven entirely from a terminal.
Embedding the real `music.youtube.com` page in a webview is still the only
sanctioned way to get audio out of an account, so the page has to exist
somewhere — but nothing about it needs to be visible.

## Shape

Two processes, one HTTP boundary at `127.0.0.1:13723`.

- `xmusic-player` owns the page and the audio. Hidden webview, no Dock icon,
  small control API. Holds no interface logic.
- `xmusic` owns the interface. Talks HTTP, renders with ratatui, knows
  nothing about YouTube Music's internals.

Splitting on that line means the fragile part — everything that depends on
Google's page — is confined to one file (`player/src/inject.js`) behind a stable
JSON API. When YouTube Music changes, only that file and the contract doc move.

## Verification first

Every claim about the page was checked against a live session before any code was
written, over CDP against a throwaway browser profile. Results are recorded in
`docs/verified-ytm-contract.md`. This overturned three plausible-looking
approaches:

| Assumption | Reality |
|---|---|
| Search by scraping the rendered results page | Unnecessary. The page's own InnerTube endpoint returns structured JSON with no navigation. |
| Start tracks with `location.href = /watch?v=…` | Full page reload. Audio stops, the injected script restarts. |
| Start tracks with `movie_player.loadVideoById` | No reload, but desyncs YouTube Music's queue: the player bar keeps showing the old track and next/previous then misbehave. |
| Read play state from `#play-pause-button`'s `aria-label` | It is `null`. Would report "paused" forever. The text lives in `title`. |

What survived:

- Search: `POST /youtubei/v1/search` from inside the page, songs-only filter.
- Track start: `ytmusic-app.resolveCommand({watchEndpoint: {videoId}})` — no
  reload, and player, player bar, and URL all stay consistent.
- State: `#movie_player` — `getVideoData`, `getPlayerState`, `getCurrentTime`,
  `getDuration`, `getVolume`, `seekTo`, `setVolume`.
- Next/previous: YouTube Music's own buttons. The raw player has no queue
  (`getPlaylist()` is `null`), so skipping cannot bypass the app.

Verification also made progress, seek, and volume nearly free, so they are in
scope rather than deferred.

## Components

### `player/src/inject.js`

The only file that knows about YouTube Music. Polls the player every 500ms and
reports a state snapshot over IPC; exposes `__xmSearch` and the acknowledged
`__xmDispatch` control entry point for the daemon to call. Searches abort after
12 seconds, and track startup fails rather than falling back to a full navigation.
The script is guarded against double-installation.

### `player/src/state.rs`

Shared state plus search sequencing. Every search gets a number; a reply whose
number is stale is dropped instead of overwriting fresher results. Previous
results stay visible while a new search is in flight rather than blanking. A
server-side deadline clears searches whose page callback never arrives, and a
reply arriving after that deadline is ignored.

### `player/src/bridge.rs`

`eval` hands a script to the webview and returns once it is queued; it cannot
report what the script decided. Every control route would therefore answer 200
whether or not the page did anything, making a player that has not finished
loading indistinguishable from a working one. Each call carries an id, the page
reports the outcome back over IPC quoting that id, and the waiting request is
given the real answer — 409 with a reason when the page could not comply, 504
when it never replied.

That also makes the page state a reportable condition rather than a silent
no-op: transport and seek refuse with "nothing is loaded" instead of succeeding
at doing nothing.

### `player/src/server.rs`

Routes to JavaScript calls. Validates before evaluating: video ids must match
YouTube's 11-character alphabet, transport actions must be one of five known
strings, and query strings are serialised as JSON string literals rather than
interpolated. Binding failure means a daemon is already running, so the process
exits rather than competing for playback. `GET /health` is public; every other
route requires the per-run `X-Xmusic-Token`, rejects browser origins and an
unexpected Host, and requires JSON for POST bodies.

### `tui/src/daemon.rs`

Locating, starting, and stopping the daemon. Startup redirects the daemon's
output to `~/.xmusic/daemon.log`; sharing the terminal would corrupt the display.
The PID file is held under an exclusive process lock, signal fallback verifies
the executable before acting, and health checks replace incompatible daemon
versions instead of silently reusing them.

### `tui/src/client.rs`

A worker thread owning all HTTP. The interface sends commands and drains events,
so a hung daemon cannot stall rendering. Blocking HTTP inside the draw loop would
have made every frame wait on the network. The command channel is bounded so key
repeat cannot create an unbounded stale backlog, while daemon shutdown has a
separate priority channel.

### `tui/src/app.rs`, `tui/src/ui.rs`

State and key handling, then rendering. `app` emits commands and never touches
HTTP; `ui` reads state and never mutates it.

## Lifecycle

The client starts the daemon if the port is silent, and leaves it running on
exit: closing a terminal should not stop playback. Getting that right needs
`setsid` on the spawned daemon — inheriting the client's process group and
controlling terminal means a terminal hangup kills it, which was observed before
the fix and is the precise failure this design exists to avoid. Because that makes an
invisible process outliving its client, stopping it is deliberately
over-provisioned — `Q` in the interface (confirmed), `--kill-daemon`,
`POST /quit`, and a pid file at `~/.xmusic/daemon.pid` so `SIGTERM` then
`SIGKILL` still work when HTTP is unresponsive. The player holds an exclusive
lock on that file for its lifetime, so a stale file or reused PID is never
signalled. A per-run token in `~/.xmusic/control.token` authenticates the HTTP
surface and is stored with mode `0600`.

## Error handling

Failures surface on the status line and never panic the interface. An unreachable
daemon is a distinct visible state, not a frozen display. Missing DOM selectors
log and return false rather than throwing, so a broken selector degrades one
button. `Mutex` locks use `expect` with a named reason: a poisoned lock means a
reporting thread panicked, which is a bug, not a runtime condition.

## Out of scope

Library and playlist browsing, album/artist/playlist search results, queue
display, lyrics, scrobbling. All reachable through the same InnerTube pattern
later.

## Integration findings

Three Tauri requirements were discovered by building, not by reading:

- Application commands are blocked for remote origins unless a permission file
  in `player/permissions/` names them. `remote.urls` alone yields
  `report_state not allowed. Plugin not found`. Application permissions are
  referenced without a namespace prefix.
- `withGlobalTauri` must be true or `window.__TAURI__` does not exist.
- `generate_context!` requires `icons/icon.png` even with bundling off, and
  `frontendDist` resolves relative to the config file, not the workspace.

Because the webview has no visible console, the daemon grew a `GET /diagnose`
route that smuggles the page's self-report out through the document URL's
fragment — `eval` cannot return a value, so this is the only channel that works
when IPC itself is what's broken. It is what identified the permission problem,
and it stays in as the first thing to check when the daemon starts but never
reports a player.

## Verification

Verified end to end against live YouTube Music: state reporting, search (20
results parsed), play via `resolveCommand`, pause/resume/next/previous with the
player bar staying coherent, seek, volume clamped at both ends, graceful quit
with pid-file cleanup, and all four shutdown paths. The interface was driven
through a pty: search, navigate, play, transport, and the `Q` confirm-and-stop
flow. A query containing quotes and a script-closing sequence round-tripped as a
literal string.

## Open risks

- The WKWebView user agent question is closed: with the pinned desktop Chrome
  agent, YouTube Music loads fully in the hidden webview.
- Request signing for signed-in search follows Google's documented scheme, with
  the cookie and the scheme name paired correctly (`SAPISID` with `SAPISIDHASH`,
  `__Secure-3PAPISID` with `SAPISID3PHASH`; mispairing them gets the request
  rejected). It has not been exercised against a real signed-in session, so it is
  built to fail open: a refused signature falls back to an unsigned retry rather
  than failing the search. That fallback was verified by forcing the refusal
  branch. The consequence of the signing being wrong is therefore generic results
  rather than a broken client.

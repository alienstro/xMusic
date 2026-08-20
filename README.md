# xmusic-tui

A terminal client for YouTube Music, split across two processes:

1. **`xmusic-player`** — a Tauri app with no visible window. It loads the real
   `music.youtube.com` in a hidden webview, which is still the only sanctioned
   way to get audio out of your own account, and exposes a small HTTP control
   API on `127.0.0.1:13723`.
2. **`xmusic`** — a [ratatui](https://ratatui.rs) terminal app: search box,
   results list, now-playing bar with a progress meter. No webview, no GUI
   toolkit, just a terminal and an HTTP client.

```
 xmusic                                                                      signed out
 ──────────────────────────────────────────────────────────────────────────────────────
 / Search songs
 ──────────────────────────────────────────────────────────────────────────────────────
   TITLE                          ARTIST                  ALBUM                    TIME
   Says                           Nils Frahm              Spaces                   8:18
 ▌ Ode – Our Own Roof             Nils Frahm              Tripping with Nils Fr…  10:04
   Right Right Right              Nils Frahm              Music for Animals        7:26
   My Friend the Forest           Nils Frahm              All Melody               5:17
 ──────────────────────────────────────────────────────────────────────────────────────
 ▶ Ode – Our Own Roof
   Nils Frahm • Tripping with Nils Frahm • 2020
   0:23 ██▏░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ 10:04  vol ██████░░  70
 ──────────────────────────────────────────────────────────────────────────────────────
   Playing Nils Frahm - Ode – Our Own Roof
   / search · space play · n/p track · L sign in · ←/→ seek · +/- vol · q quit
```

```
┌──────────────┐   HTTP (127.0.0.1:13723)   ┌───────────────────┐
│  xmusic  │ ────────────────────────▶ │  xmusic-player    │
│  (ratatui)   │ ◀──────────────────────── │  (hidden webview) │
└──────────────┘   JSON: player state,     └───────────────────┘
                   search results                    │
                                                     ▼
                                            music.youtube.com
                                        (real page, real account)
```

RAM cost is roughly the same as a windowed client (~80–150MB while something is
loaded): hiding the window stops the painting, not the page, the JavaScript, or
the audio decode. What you gain is a terminal-native interface.

## Install

### macOS, with Homebrew

```bash
brew install alienstro/tap/xmusic
```

One command — Homebrew taps the repository automatically because the name is
fully qualified, so there is no separate `brew tap` step. It installs both
halves, `xmusic` and the `xmusic-player` daemon, into the same directory, which
matters because the client starts the daemon by looking beside itself.

Afterwards the short name works everywhere:

```bash
brew upgrade xmusic
brew info xmusic
brew uninstall xmusic
```

> **Not live yet.** This needs the tap published at `alienstro/homebrew-tap` and
> a tagged release of this repository. The formula is written and tested — see
> [`packaging/homebrew/`](packaging/homebrew/) — but until it is pushed, build
> from source below.

A bare `brew install xmusic`, with no namespace, would require the formula to
live in `homebrew/core`. That is a curated repository with a notability bar, so
it is not something a new project can opt into — unlike npm, you cannot publish
into its global namespace. The name is unclaimed, so it stays possible later.

### With Cargo

Install **both** binaries. The client looks for the daemon beside itself, and
then on `PATH`, so installing only the client leaves it with nothing to start:

```bash
cargo install --path player
cargo install --path tui
xmusic
```

The client's crate is `xmusic-tui`, but the binary it installs is `xmusic` —
that is the command. The daemon installs as `xmusic-player` beside it. To remove
both: `cargo uninstall xmusic-tui xmusic-player`.

### From source

```bash
git clone https://github.com/alienstro/xMusic.git
cd xMusic
cargo build
./target/debug/xmusic
```

The first build pulls in Tauri and takes a few minutes; later builds are
seconds. Both binaries land in `target/debug/`.

You need:

- **macOS.** Built and tested on macOS (Darwin 25). The code is written for Unix
  generally, but Linux is untested and would additionally need webkit2gtk for
  the webview.
- **Xcode Command Line Tools**, for the linker: `xcode-select --install`
- **Rust 1.85 or newer** — the workspace's declared minimum toolchain.
  If you have no toolchain:
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
  (Built and tested with 1.97.)

## Using it

Run `xmusic`. That is the only command you need — the client notices there is no
daemon running, starts one, and waits for it. The daemon loads YouTube Music in
a hidden webview, so for the first few seconds the now-playing line reads
`Loading YouTube Music`. That is it booting, not hanging.

### Play something

1. Press `/`
2. Type what you want — `boards of canada roygbiv`
3. Press `Enter`
4. Move with `j` and `k`, or the arrow keys
5. Press `Enter` on the track you want

The gutter bar shows where you are in the list; the track actually playing is
the amber one.

### Drive it

| Key | Does |
|---|---|
| `space` | Play / pause |
| `n` / `p` | Next / previous track |
| `←` `→` | Seek 5 seconds |
| `+` `-` | Volume |

If a key cannot do anything yet, the status line says why rather than pretending
it worked — press `space` before playing anything and it tells you to search
first.

### Sign in, if you want to

Search and playback both work signed out, so this is optional. Sign in for your
own library and recommendations, and an ad-free stream if you pay for Premium.

Google refuses to accept a sign-in from an embedded webview — it answers "this
browser or app may not be secure" — so xmusic never asks for your password and
the hidden window can never be logged into directly. You sign in with your
normal browser instead, and xmusic copies that session across:

1. Press `L` — if no signed-in session is found, your browser opens at
   music.youtube.com
2. Sign in there as you normally would
3. Press `L` again — xmusic reads the youtube.com cookies from your browser and
   hands them to the player, which reloads as you

Same thing from a shell: `xmusic --login`.

Brave, Chrome, Edge, Arc, Vivaldi and Chromium are supported. Reading the
cookies means decrypting them with a key held in your login keychain, so macOS
asks for permission the first time — that prompt is the whole cost of the
arrangement. Only the seventeen cookies that carry a YouTube session are read,
never the rest of your browsing.

The session then persists in the webview's own cookie store, so it is a one-off.
`xmusic --uninstall` deletes it again.

### Leave it, or stop it

```
q   quit the client — the music keeps playing
Q   quit and stop the daemon — asks first
```

`q` is the normal way out. The daemon runs in its own session, so it survives
closing the terminal and your music keeps going while you get on with something
else.

When you do want it to stop:

| Method | Notes |
|---|---|
| `Q` in the interface | Confirms first, then stops the daemon and exits |
| `xmusic --kill-daemon` | From any terminal |
| `xmusic --restart` | When it is wedged rather than stopped |
| `POST /quit` | The underlying authenticated endpoint |
| `SIGTERM`, then `SIGKILL` | Automatic fallback only while `~/.xmusic/daemon.pid` is locked by a verified `xmusic-player` process |

### Command line

```bash
xmusic                  # connect, starting the daemon if needed
xmusic --login          # copy your browser's YouTube session into the player
xmusic --restart        # stop whatever is there, start fresh, open the interface
xmusic --no-spawn       # fail instead of starting a daemon
xmusic --daemon-status  # is it running?
xmusic --kill-daemon    # stop it
xmusic --uninstall      # stop the daemon and delete its data (asks first)
xmusic --help
```

### If something looks wrong

The webview has no visible console, so the daemon reports the page's own view of
itself:

```bash
TOKEN="$(cat ~/.xmusic/control.token)"
curl -s -H "X-Xmusic-Token: $TOKEN" \
  http://127.0.0.1:13723/diagnose | python3 -m json.tool
```

A healthy answer has `tauri: "object"`, `invoke: "function"`, `injected: true`,
`ytcfg: true`, `moviePlayer: true`, and `lastError: null`.

`lastError` carries the reason the last IPC call was rejected, which is the one
thing you cannot see from outside the page. `auth` reports how the last search
was signed: `signed (SAPISIDHASH)`, `unsigned (no auth cookie)`, or
`signed request refused …, retried unsigned`.

The daemon's own output is in `~/.xmusic/daemon.log`.

## The interface

Nothing is boxed. Hairline rules and a fixed two-column gutter carry the
structure, which puts the whole weight of the layout on alignment — the column
widths and the meter are computed to the cell rather than approximated.

Amber is the only colour with a job: it marks the track that is playing and fills
the meter. Everything else is graded by brightness, so the interface sits inside
whatever colour scheme your terminal already has instead of fighting it. Two
separate channels carry two different facts — colour says what is playing, the
gutter bar says where you are — so neither needs a glyph of its own.

The progress meter uses eighth-width blocks, giving it sub-character resolution
so it glides rather than stepping a whole cell at a time.

Chrome is shed as the terminal shrinks, least useful first: the album column
below 78 columns, the artist below 40, the column header below 16 rows, the key
hints below 13, and the now-playing subtitle below 12. The meter is the last
thing to go — an interface that cannot show you where you are in the track has
stopped being a music player. Key hints truncate at whole labels, never
mid-word, and only ever advertise keys that currently do something: while you
are typing a query, the transport keys are just characters in the search box, so
they are not shown.

## Keys

Every binding, including the ones step 4 above leaves out.

| Key | Action |
|---|---|
| `/` | Search (Enter submits, Esc cancels) |
| `j` / `k`, `↓` / `↑` | Move through results |
| `Enter` | Play the selected track |
| `space` | Play / pause |
| `n` / `p` | Next / previous track |
| `←` / `→`, `h` / `l` | Seek ∓5s |
| `+` / `-` | Volume ±5 |
| `L` | Sign in: copy your YouTube session from your browser |
| `W` / `H` | Show / hide the player window (for diagnosing the page) |
| `q` | Quit the client, leave the daemon playing |
| `Q` | Quit and stop the daemon (asks first) |

## How it talks to YouTube Music

Every DOM path and player method this project depends on was verified against a
live `music.youtube.com` session; the probe results are in
[`docs/verified-ytm-contract.md`](docs/verified-ytm-contract.md). Three findings
shaped the design:

- **Search goes through YouTube Music's own InnerTube endpoint**, not the
  rendered page. The injected script POSTs to `/youtubei/v1/search` same-origin,
  which returns structured JSON — video id, title, artist, album, duration — with
  no navigation, so audio never stops. Scraping search-result DOM nodes is
  unnecessary.
- **Starting a track goes through `ytmusic-app.resolveCommand`.** Assigning
  `location.href` reloads the page and kills playback. `movie_player
  .loadVideoById` avoids the reload but desyncs YouTube Music's queue: the
  player bar keeps showing the previous track and the next/previous buttons then
  misbehave. `resolveCommand` routes through the app the way a click does, so the
  player, the player bar, and the URL all stay consistent.
- **Play state comes from `getPlayerState()`.** The `aria-label` on
  `#play-pause-button` is `null` — reading it would report "paused" forever. The
  attribute that actually holds the label text is `title`.

Only two things still touch the DOM: the next and previous buttons. The raw
player has no queue of its own (`getPlaylist()` returns `null`), so skipping has
to go through YouTube Music's controls. `previous` rewinds to 0 first, because
YouTube Music restarts the current track instead of stepping back once you are a
few seconds in.

### Control API

`GET /health` is the only unauthenticated route. Every other request must carry
`X-Xmusic-Token`, whose 256-bit value is stored in `~/.xmusic/control.token`
with mode `0600` and regenerated on every daemon start. POST requests must also
use `Content-Type: application/json`.

| Route | Body | Effect |
|---|---|---|
| `GET /health` | | Liveness probe |
| `GET /state` | | Track, play state, position, duration, volume, sign-in |
| `GET /search-results` | | Latest results, with `seq` and `pending` |
| `POST /search` | `{"query": "…"}` | Runs a search; results arrive asynchronously |
| `POST /play` | `{"videoId": "…"}` | Plays a track |
| `POST /control` | `{"action": "play_pause"}` | `play`, `pause`, `play_pause`, `next`, `prev` |
| `POST /seek` | `{"delta": -5}` or `{"seconds": 60}` | Relative or absolute |
| `POST /volume` | `{"delta": 5}` or `{"level": 40}` | Relative or absolute |
| `POST /show-window` / `/hide-window` | | Reveal or hide the webview |
| `POST /quit` | | Stop the daemon |
| `GET /diagnose` | | What the page sees: IPC availability, whether the script injected, whether YouTube Music actually loaded |

Searches carry a sequence number. A reply for a query the user has already
replaced is discarded rather than overwriting fresher results.

Control routes answer with what actually happened, not with whether the request
was accepted:

| Status | Meaning |
|---|---|
| `200` | The page did it |
| `400` | The request was malformed — bad video id, unknown action |
| `401` / `403` | The control token, host, or request origin was rejected |
| `409` | The page is there but could not comply, and says why: the player hasn't loaded, nothing is loaded to play, or a control it needs is missing |
| `415` | A POST did not use `application/json` |
| `503` / `504` | The page never answered |

This costs a round trip per call. `eval` cannot return a value, so each control
call carries an id that the page quotes when it reports the result back. Without
it every one of these routes would answer `200` whether or not anything
happened, and a player that hadn't finished loading would be indistinguishable
from a working one.

## Security note

`player/capabilities/default.json` grants `music.youtube.com` IPC access via
`remote.urls`, and `player/permissions/reporting.toml` names the three commands it
may call. Tauri v2 blocks every webview from calling into Rust by default,
including your own, and for remote origins it blocks each command individually —
`remote.urls` alone opens the door without saying what may come through. The page
can call exactly `report_state`, `report_search_results`, and `report_command`, all of
which only store data in memory; none of them takes any action on your machine.
The capability deliberately omits `core:default`, so the remote page receives no
general Tauri window, menu, tray, path, event, or resource permissions. It is still
a deliberate trust decision.

The control server binds `127.0.0.1` only, and validates video ids and transport
actions before they reach the page. Search queries are passed to JavaScript as
serialised string literals rather than interpolated, so a query containing quotes
is a query, not code. Loopback is not treated as authentication: non-health routes
require the per-run token, reject browser `Origin` headers, and require the exact
loopback `Host` value.

## Known limitations

- Same RAM floor as a windowed client. This trims the interface, not the page.
- No library or playlist browsing yet — search and play only. The same InnerTube
  pattern reaches `/youtubei/v1/browse`, so it's an endpoint and a pane away.
- Search is filtered to songs. Albums, artists, and playlists are not surfaced.
- The daemon holds one search at a time. Two clients pointed at the same daemon
  will overwrite each other's results — last search wins. Fine for one person at
  one keyboard, which is what this is for.
- Stopping the daemon is the only way playback ends. It is detached into its own
  session, so it survives quitting the client, closing the terminal, and logging
  out of the shell. If you forget about it, it keeps playing — `xmusic
  --daemon-status` tells you, `--kill-daemon` stops it.
- `GET /state` is up to 500ms behind, because the page reports on a 500ms timer.
  Reading it immediately after a control call can still show the old value. The
  control call's own response is authoritative; the state snapshot catches up.
- Signed-in search returns personalised results by signing the request the way
  Google's own client does. That signing has not been exercised against a real
  signed-in session, so it is treated as best-effort: if the signature is
  refused, the search is retried unsigned rather than failing, which is why a
  signed-in user can never end up worse off than a signed-out one. `GET
  /diagnose` reports which path was taken in its `auth` field.
- YouTube Music serves a stripped "browser is deprecated" page, with no player
  and no `ytcfg` at all, to user agents it doesn't recognise. The daemon pins a
  current desktop Chrome user agent, which is verified working in Tauri's
  WKWebView. If the interface ever sits on "Loading YouTube Music…" forever,
  that string in `player/src/main.rs` has gone stale — the authenticated
  `/diagnose` command above will say so.

## Licence

MIT

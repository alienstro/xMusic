# xMusic

A terminal client for YouTube Music, split across two processes over a shared
protocol crate:

1. **`xmusic-player`** — a Tauri app with no visible window. It loads the real
   `music.youtube.com` in a hidden webview, which is still the only sanctioned
   way to get audio out of your own account, and exposes a small HTTP control
   API on `127.0.0.1:13723`.
2. **`xmusic`** — a [ratatui](https://ratatui.rs) terminal app: five tabs over
   your library, a search box, a results list, and a now-playing bar with a
   progress meter. No webview, no GUI toolkit, just a terminal and an HTTP
   client.

Both depend on **`xmusic-protocol`** for the messages they exchange, and neither
depends on the other; see [How it is put together](#how-it-is-put-together).

```
 xmusic                                                                       signed in
 ──────────────────────────────────────────────────────────────────────────────────────
   Search │ Liked │ Playlists │ Albums │ History
 ──────────────────────────────────────────────────────────────────────────────────────
     TITLE                        ARTIST                  ALBUM                    TIME
   ♥ Says                         Nils Frahm              Spaces                   8:18
 ▌ ♡ Ode – Our Own Roof           Nils Frahm              Tripping with Nils Fr…  10:04
   ♥ Right Right Right            Nils Frahm              Music for Animals        7:26
   ♡ My Friend the Forest         Nils Frahm              All Melody               5:17
 ──────────────────────────────────────────────────────────────────────────────────────
 ▶ Ode – Our Own Roof   ♡
   Nils Frahm • Tripping with Nils Frahm • 2020
   0:23 ██▏░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ 10:04  vol ██████░░  70
 ──────────────────────────────────────────────────────────────────────────────────────
   127 in Liked
   1-5 pane · / search · f like · space play · n/p track · Esc back · r reload
```

```
┌──────────────┐   HTTP (127.0.0.1:13723)   ┌───────────────────┐
│    xmusic    │ ─────────────────────────▶ │   xmusic-player   │
│  (ratatui)   │ ◀───────────────────────── │ (hidden webview)  │
└──────────────┘   JSON: player state,      └───────────────────┘
                   lists, like state                  │
                                                      ▼
                                              music.youtube.com
                                          (real page, real account)
```

Hiding the window stops the painting, not the page, the JavaScript, or the audio
decode, so a loaded page costs what a windowed client costs — measured at
**~550MB** across the five processes it takes (385MB page, 94MB GPU, 27MB
daemon, 26MB web content, 16MB networking). Two things claw that back, and both
are described under [Memory](#memory): the page is unloaded once it has been
idle for a while, and its artwork is never fetched, because the terminal is
what draws it. What you gain over a windowed client is a terminal-native
interface, not a smaller browser.

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

### Your library

Five tabs across the top, selected with `1`–`5` or stepped through with `Tab`:

| Tab | What is in it |
|---|---|
| Search | Whatever you last searched for |
| Liked | Your liked songs |
| Playlists | The playlists you made and the ones you saved |
| Albums | Your saved albums |
| History | What you have listened to, newest first |

Each tab remembers what it loaded and where you were in it, so moving between
them costs nothing and does not refetch. A tab loads on its first visit; `r`
reloads it when you want fresh.

On a playlist or an album, `Enter` opens it rather than playing it — the only
rows where playing one thing makes no sense. A breadcrumb replaces the tabs
while you are inside one, so there is never a question which list is on screen,
and `Esc` comes back out.

### Like something

`f` likes the selected track, or unlikes it if it is already liked, and it does
the same for the now-playing track when the list you are on has nothing to act
on. The heart repaints on the keypress rather than on the next poll; if YouTube
Music refuses, the heart goes back and the status line says why.

A hollow heart means not liked, a filled one means liked, and no heart at all
means the response never said — search results carry no like state, and guessing
would be wrong on every row.

### Drive it

| Key | Does |
|---|---|
| `1`–`5` | Switch tab |
| `f` | Like / unlike |
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

Amber is the only colour with a job: it marks the track that is playing, the tab
you are on, and a liked track's heart, and it fills the meter. Everything else is
graded by brightness, so the interface sits inside whatever colour scheme your
terminal already has instead of fighting it. Two separate channels carry two
different facts — colour says what is playing, the gutter bar says where you are
— so neither needs a glyph of its own.

One row carries the tabs, the search box and the breadcrumb, because they are
never wanted at once: typing a query replaces the tabs, and opening a playlist
replaces them with the trail that led into it.

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
| `1`–`5` | Switch to that tab |
| `Tab` / `Shift-Tab` | Step through the tabs |
| `/` | Search (Enter submits, Esc cancels) |
| `j` / `k`, `↓` / `↑` | Move through the list |
| `Enter` | Play the selected track, or open the selected playlist or album |
| `Esc` | Leave a playlist or album, back to the tab |
| `f` | Like or unlike the selected track, or the one playing |
| `r` | Reload the list on screen |
| `space` | Play / pause |
| `n` / `p` | Next / previous track |
| `←` / `→`, `h` / `l` | Seek ∓5s |
| `+` / `-` | Volume ±5 |
| `L` | Sign in: copy your YouTube session from your browser |
| `W` / `H` | Show / hide the player window (for diagnosing the page) |
| `q` | Quit the client, leave the daemon playing |
| `Q` | Quit and stop the daemon (asks first) |

## How it is put together

Three crates, and one rule about which way the arrows point.

```
xmusic-protocol  ◀── xmusic-player
       ▲
       └────────────  xmusic (tui)
```

`xmusic-protocol` is what the two halves agree on: the JSON they exchange, the
vocabulary it is written in — feeds, transport actions, list sources — and the
constants that would silently break the pair if they drifted. It depends on
nothing that carries the messages, so neither binary can reach the other through
it, and a wire change is one edit rather than two that have to be kept in step.

Inside each binary the same split repeats. The behaviour sits in the middle and
the technology sits at the edges:

| | Daemon | Client |
|---|---|---|
| Behaviour | `application.rs` — what every route, timer and report does | `model.rs`, `update.rs` — one model, one pure transition |
| Page or view | `ports.rs` — what the page must be able to do | `view.rs` — draws the model, calls nothing |
| Edges | `adapters/http.rs`, `adapters/tauri_page.rs` | `effects.rs`, `adapters/*` |

So the daemon's control server authenticates a request, parses it into a typed
one, and maps an answer to a status code — and decides nothing else. Which
operations need the page awake, how a list is sequenced, how long each kind of
call is given, and when an idle page is handed back all live in one place. The
webview, `eval`, and the reply channel live behind `PageDriver`, which is why
YouTube Music changing shape is contained in one adapter and `inject.js` rather
than felt all the way out in the terminal.

The client is the same idea in its other well-known form: terminal keys and
daemon replies both become messages, `update` is the only thing that changes the
model and returns a list of effects rather than performing any, and a background
thread is the only thing that performs one. That is what makes an optimistic
heart and its reversal two ordinary state transitions instead of a callback
threaded through a request.

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
- **Every library tab is the same `browse` call with a different id**, and every
  row in every list is the same `musicResponsiveListItemRenderer` the search
  parser already read. Liked songs, playlists, saved albums and history differ
  only in the `browseId` they carry and in whether their shelf is a list or a
  grid, so five tabs cost one item parser and one grid parser rather than five
  of each. Which shape a row is gets read from the row, so a feed YouTube Music
  changes degrades to fewer fields rather than wrong ones.
- **Continuations are followed inside the page.** A feed arrives a page at a
  time, with the next page's token in the tail of the last one; the injected
  script walks them and reports one list, so the daemon asks for a tab and
  receives rows and never learns what a continuation token is. A page cap bounds
  a very large library — a truncated list says so rather than looking complete.
- **Likes are one more endpoint on the same signed path.** `like/like` and
  `like/removelike` take `{ target: { videoId } }`; the current state arrives
  with each row, on `likeButtonRenderer.likeStatus`. A refusal is an HTTP error
  rather than a quiet no-op, which is what lets an optimistic heart be reverted.
  The one exception is the now-playing track, whose state is read from the
  player bar: a like placed through InnerTube never reaches YouTube Music's own
  bar, so that heart shows what this session set until the track changes.

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
| `GET /state` | | Track, play state, position, duration, volume, sign-in, like |
| `GET /list` | | The current list: `items`, plus `seq`, `source`, `label`, `pending` and `truncated` |
| `GET /search-results` | | Alias of `GET /list`, kept for one version |
| `POST /search` | `{"query": "…"}` | Runs a search; results arrive asynchronously |
| `POST /browse` | `{"feed": "liked"}` | Loads one tab: `liked`, `playlists`, `albums`, `history` |
| `POST /playlist` | `{"browseId": "…"}` | Loads one playlist's or album's tracks |
| `POST /like` | `{"videoId": "…", "liked": true}` | Sets or clears a like |
| `POST /play` | `{"videoId": "…"}` | Plays a track |
| `POST /control` | `{"action": "play_pause"}` | `play`, `pause`, `play_pause`, `next`, `prev` |
| `POST /seek` | `{"delta": -5}` or `{"seconds": 60}` | Relative or absolute |
| `POST /volume` | `{"delta": 5}` or `{"level": 40}` | Relative or absolute |
| `POST /sleep` | | Unload the page now, without waiting out the idle timeout |
| `POST /wake` | | Load the page back and wait until it can play |
| `POST /show-window` / `/hide-window` | | Reveal or hide the webview |
| `POST /quit` | | Stop the daemon |
| `GET /diagnose` | | What the page sees: IPC availability, whether the script injected, whether YouTube Music actually loaded |

Every list route answers `202` with a sequence number and fills `GET /list`
asynchronously. A reply for a list the user has already moved past is discarded
rather than overwriting fresher results, and the `source` field says which tab
produced what is there.

Feed names are checked against a fixed set and ids against their shape before
either reaches the page, the same way transport actions already are; neither is
ever interpolated into script text as data.

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

## Memory

The page is the expensive half of this program, and almost none of what it
spends is spent on anything the terminal shows. Two measures follow from that.

**The page is unloaded while idle.** After five minutes with nothing playing and
no command, the webview is parked on `about:blank`, which releases the page and
everything it was holding. The process, its cookies, and the control server all
survive; the daemon's own footprint measured 18MB while unloaded. The next
command that needs the page loads it again first and waits — a wake measured
3.7s from a cold page to playable — so an unload is invisible apart from that
pause. `GET /state` reports `hibernating` while the page is gone, and the
interface says `Idle - page unloaded` rather than pretending to load forever.

Whatever was loaded comes back paused and at its old position, so pausing a
track, walking away, and pressing play still does what it looks like. The queue
that track belonged to does not come back, and cannot: it lived in the document
that was released.

`XMUSIC_IDLE_TIMEOUT` sets the idle period in seconds. `0` keeps the page loaded
for the life of the daemon, which is the right setting if a several-second wake
is worse for you than several hundred megabytes.

**Artwork is never loaded.** Every thumbnail the page fetches is decoded into a
bitmap it then holds, and no one ever looks at any of them — the window is
hidden, and the terminal draws its own artwork from the URLs InnerTube already
returns. So the injected script substitutes a 1x1 transparent GIF for every
image the page sets, and blanks CSS backgrounds with a stylesheet, since an
image computed away to `none` is never fetched at all. Search, playback, and
sign-in are unaffected; `XMUSIC_KEEP_IMAGES=1` puts the images back, which is
worth having alongside `/show-window` when you need to see what YouTube Music is
actually doing.

## Security note

`player/capabilities/default.json` grants `music.youtube.com` IPC access via
`remote.urls`, and `player/permissions/reporting.toml` names the three commands it
may call. Tauri v2 blocks every webview from calling into Rust by default,
including your own, and for remote origins it blocks each command individually —
`remote.urls` alone opens the door without saying what may come through. The page
can call exactly `report_state`, `report_list`, and `report_command`, all of
which only store data in memory; none of them takes any action on your machine.
The capability deliberately omits `core:default`, so the remote page receives no
general Tauri window, menu, tray, path, event, or resource permissions. It is still
a deliberate trust decision.

The control server binds `127.0.0.1` only. Feed names and transport actions are
enums in the shared protocol crate, so an unknown one fails to deserialise before
it is an operation at all, and video and playlist ids are checked for shape.
Everything that crosses into JavaScript is passed as a serialised string literal
rather than interpolated, so a query containing quotes is a query, not code. Loopback is not treated as authentication: non-health routes
require the per-run token, reject browser `Origin` headers, and require the exact
loopback `Host` value.

## Known limitations

- A loaded page costs what a windowed client costs. Unloading it while idle and
  dropping its artwork claw back most of that, but nothing makes YouTube Music's
  own front-end cheap while it is on screen's worth of DOM. See
  [Memory](#memory).
- Waking an unloaded page takes a few seconds, and the queue the last track
  belonged to does not survive the unload — only the track and its position do.
- Search is filtered to songs. Searching for an album, an artist or a playlist
  is not offered; the library tabs are how you reach those.
- Artists and subscriptions have no tab of their own yet. The same pattern
  reaches them — another `browseId` and another entry in the tab list.
- A very large library is truncated. Feeds are followed page by page up to a
  cap, and a list that hit the cap says how many it loaded rather than
  presenting a part as the whole. Paging on demand is the fix if it ever bites.
- Search results carry no like state, because YouTube Music does not send one
  with them. Those rows show no heart rather than a hollow one — pressing `f`
  there likes the track, which is almost always what you meant.
- The like on the now-playing line shows what this session set. YouTube Music's
  own player bar never hears about a like placed through InnerTube, so a track
  liked elsewhere since it started playing will not show through until the list
  it is in is reloaded.
- Thumbnail URLs are parsed and carried, but nothing draws them. Terminal
  artwork needs protocol detection and is its own piece of work.
- The daemon holds one list at a time. Two clients pointed at the same daemon
  will overwrite each other's tabs — last one wins. Fine for one person at one
  keyboard, which is what this is for.
- Stopping the daemon is the only way playback ends. It is detached into its own
  session, so it survives quitting the client, closing the terminal, and logging
  out of the shell. If you forget about it, it keeps playing — `xmusic
  --daemon-status` tells you, `--kill-daemon` stops it.
- `GET /state` is up to 200ms behind, because the daemon polls the page on a
  200ms timer.
  Reading it immediately after a control call can still show the old value. The
  control call's own response is authoritative; the state snapshot catches up.
- Signed-in requests are signed the way Google's own client does. If a signature
  is refused, a read is retried unsigned rather than failing, so a signed-in user
  can never end up worse off than a signed-out one; `GET /diagnose` reports which
  path was taken in its `auth` field. A like is not retried unsigned — an
  unsigned like cannot work, and retrying one would replace the real refusal with
  a `401` that says nothing about why.
- YouTube Music serves a stripped "browser is deprecated" page, with no player
  and no `ytcfg` at all, to user agents it doesn't recognise. The daemon pins a
  current desktop Chrome user agent, which is verified working in Tauri's
  WKWebView. If the interface ever sits on "Loading YouTube Music…" forever,
  that string in `player/src/main.rs` has gone stale — the authenticated
  `/diagnose` command above will say so.

## Licence

MIT

# xMusic library, likes, and tabs design

Date: 2026-08-21
Status: awaiting review

## Problem

The client can search and play, and that is all. Everything else a person
actually has in YouTube Music — the songs they liked, the playlists they made,
the albums they saved, what they listened to yesterday — is unreachable, and
there is no way to like a track from the terminal at all.

Search alone also needed no navigation: one query, one list. Five sources of
tracks do. Without somewhere to put them, each new feed would become another
key binding and another branch in an already 12K `app.rs`.

## Shape

Five tabs across the top of the interface, each backed by one InnerTube feed,
plus a like toggle that works on whatever row is selected.

```
 xMusic                                        robinx   ▸ playing
 ─────────────────────────────────────────────────────────────────
  Search │ Liked │ Playlists │ Albums │ History
 ─────────────────────────────────────────────────────────────────
   TITLE                      ARTIST         ALBUM          TIME
 ▌ Says                       Nils Frahm     Spaces         8:18
   Ode – Our Own Roof         Nils Frahm     Tripping      10:04
 ─────────────────────────────────────────────────────────────────
 ▶ Ode – Our Own Roof   ♥
   0:23 ██▏░░░░░░░░░░░░░░░░░░ 10:04    vol ██████░░ 70
 ─────────────────────────────────────────────────────────────────
  1-5 pane · / search · f like · space play · q quit
```

Nothing about how the daemon talks to YouTube Music changes. Every feed here is
the same `/youtubei/v1/browse` call the existing search already proves out,
differing only in the `browseId` it carries and the shape of the items that come
back. Likes are one more endpoint on the same signed, same-origin path.

## Verification first

The `browseId` values and the like endpoints in this document come from general
knowledge of InnerTube, not from this account. They are the single largest risk
in the work, and they are cheap to settle, so nothing is written against a guess:

1. Add a temporary probe reachable through `GET /diagnose`, or drive
   `music.youtube.com` in a real browser with DevTools open on the `youtubei`
   requests, and capture the actual request body the page sends when the Library,
   Liked Music, Albums, and History views are opened.
2. Capture one `like` and one `removelike` request the same way, including how
   the current like state is expressed in what comes back.
3. Record every finding in `docs/verified-ytm-contract.md` beside the existing
   entries, with the response shapes the parsers will rely on.
4. Only then write the parsers.

If a captured `browseId` disagrees with the table below, the captured one wins
and this document is corrected.

Expected feeds, to be confirmed by step 1:

| Tab | browseId | Item shape |
|---|---|---|
| Liked | `FEmusic_liked_videos` | list |
| Playlists | `FEmusic_liked_playlists` | grid |
| Albums | `FEmusic_liked_albums` | grid |
| History | `FEmusic_history` | list |

## Components

### `player/src/inject.js`

`parseSearchResponse` generalises into `parseMusicList`. A browse response and a
search response differ in their container, not in their items: both wrap
`musicResponsiveListItemRenderer`, which the existing item parser already reads.
The container walk therefore learns three shelf types — `musicShelfRenderer`
(search), `musicPlaylistShelfRenderer` (a playlist's tracks), and
`musicCarouselShelfRenderer`/`gridRenderer` (the library landing shelves) —
while the item parser stays one function.

Grid shelves need a second item parser. Playlists and albums arrive as
`musicTwoRowItemRenderer`, which carries a title, a subtitle, and a
`browseId`/`playlistId` rather than a videoId and a duration. This is the one
place where five tabs cost more than three would have: two item shapes instead
of one, distinguished by which renderer key is present rather than by which feed
was asked for, so a feed that changes shape degrades to "no items" instead of
mis-parsing.

Two entry points, each following `__xmSearch`'s existing discipline — a sequence
number echoed back, an `AbortController` timeout, results reported over IPC:

- `__xmBrowse(seq, feed)` maps a feed name to a `browseId` and walks
  continuations until the feed is exhausted or a page cap is reached.
- `__xmPlaylist(seq, playlistId)` browses `VL` + the id for one playlist's or
  album's tracks.

Continuations are followed inside the page. A response's tail carries
`continuationItemRenderer.continuationEndpoint.continuationCommand.token`, which
is resent as `{ continuation: token }` until it stops appearing. Rust never
learns what a continuation token is; it asks for a feed and receives a list. A
page cap bounds a very large Liked Music, and when the cap truncates a feed the
list says so rather than looking complete.

One new dispatch action, `like`, taking `{ videoId, liked }` and calling
`like/like` or `like/removelike` with `{ target: { videoId } }`. It reports
through the existing bridge, so the route answers with what the page did rather
than that a script was queued.

Reading the current like state is deliberately separate from setting it, because
the two have different sources. Step 2 of the verification settles which source
is real: the `likeStatus` field on items in a browse response, or the state of
the player bar's like control for the now-playing track.

### `player/src/state.rs`

`SearchState` becomes `ListState`, carrying the same `seq`, `pending`, `error`,
and items it does now plus a `source` naming what produced it. A browse and a
search differ in origin and in nothing else, so they share one slot rather than
having one each; the interface already knows which tab it asked about, and a
reply for a tab the user has left is discarded by sequence number exactly as a
stale search is today.

`SearchResult` gains the fields a library row needs — an optional `playlist_id`
and `browse_id` for a grid item, a `liked` flag, and the largest `thumbnail` URL
the response carried — and loses nothing, so the same struct describes a song, a
playlist, and an album. The thumbnail is parsed because it arrives in the same
response for free and costs one line to keep; nothing draws it yet. `PlayerState` gains
`liked` for the now-playing track.

### `player/src/server.rs`

Four routes, all needing `Need::Api` from `hibernate::wake`, since none of them
touches the player element:

| Route | Body | Effect |
|---|---|---|
| `POST /browse` | `{"feed": "liked"}` | Loads one feed asynchronously |
| `POST /playlist` | `{"playlistId": "…"}` | Loads one playlist's or album's tracks |
| `POST /like` | `{"videoId": "…", "liked": true}` | Sets or clears a like |
| `GET /list` | | The current list, with `seq`, `pending`, and `source` |

`GET /search-results` stays as an alias of `GET /list` for one version, because
breaking a documented route to rename it buys nothing. `PROTOCOL_VERSION` goes
to 2, and the client's compatibility check treats a version-1 daemon as too old
rather than probing for routes.

Feed names are validated against a fixed set before they reach the page, the way
transport actions already are, and `playlistId` is validated for shape. A feed
name is never interpolated into script text as data.

### `tui/src/panes.rs`

New. Holds the `Pane` enum, per-pane list state, the selection in each, and the
drill-down stack. This exists so `app.rs` does not grow a sixth responsibility:
it is already the largest file in the client, and per-pane list state is exactly
the kind of thing that would double it.

Each pane caches what it last loaded, so moving between tabs does not refetch.
A pane loads on first visit and on an explicit refresh key, not on every switch.

### `tui/src/ui.rs`

Two new functions and one deletion. `draw_tabs` renders the tab row — the active
tab in `AMBER`, the rest in `ASH`, separators in `SLATE`, matching the existing
palette rule that amber is the only chromatic accent. `draw_list` becomes
generic over what a row is, so the results table is not copied five times; the
column set varies by pane (a grid pane shows name and subtitle, not artist,
album, and duration).

The drill-down replaces the tab row with a breadcrumb — `Playlists › Ambient` —
so there is never any question which list is on screen. A heart column sits
between the gutter and the title, and the now-playing line carries one too.

### `tui/src/app.rs`, `tui/src/client.rs`

`app.rs` delegates list state to `panes.rs` and gains the key handling. The
existing bindings constrain it more than the mockup suggests:

- `1`-`5` and Tab/Shift-Tab switch panes. Digits are unbound today.
- `Enter` is contextual, because it already means play. On a track row it still
  plays; on a playlist or album row — the only rows where playing one thing makes
  no sense — it opens that list. Nothing is rebound, and no row has two meanings.
- `Esc` leaves a drill-down. It is bound in `Mode::Editing` only today, so
  normal-mode `Esc` is free.
- `f` toggles a like on the selected row, or on the now-playing track when the
  list is empty. Not `l`: that is seek-forward in the existing vim pairing with
  `h`, and `L` is sign-in. `f` for favourite is the nearest free mnemonic, and
  the footer shows `f like` so the binding is never guessed at.

`client.rs` gains `Command::Browse`, `Command::OpenPlaylist`, and
`Command::Like`, polls `GET /list` where it polled `/search-results`, and applies
a like optimistically so the heart responds to the keypress rather than to the
next poll, reverting if the route reports failure.

## Lifecycle

1. The user presses `2`. The client sends `POST /browse {"feed":"liked"}` and the
   pane shows its cached list, or a spinner on first visit.
2. The daemon wakes the page if it was unloaded, opens a sequence number, and
   evaluates `__xmBrowse(seq, "liked")`.
3. The page browses `FEmusic_liked_videos`, follows continuations to the cap,
   parses each page, and reports the accumulated list over IPC.
4. `GET /list` serves it on the next 200ms poll. A reply for a pane the user has
   since left is dropped by sequence number.
5. The user presses `♥`. The client paints the heart, sends `POST /like`, and the
   page calls `like/like`. A rejection repaints the old state and says why on the
   status line.

## Error handling

Every failure follows the pattern the search path already established: the page
reports a sentence, the route answers with a status that distinguishes "the page
could not" from "the page never answered", and the interface puts the sentence on
the status line rather than a code.

- An empty feed is not an error. "No liked songs yet" is a state, and reads
  differently from a failure.
- A feed that parses to nothing when it should not is reported as a parse
  failure naming the feed, because that is the signature of a renderer YouTube
  Music has changed, and it should be obvious rather than look like an empty
  library.
- A cap-truncated feed says how many it loaded, so a partial list is never
  presented as a whole one.
- A like on a track YouTube Music refuses reverts the heart. No optimistic paint
  survives a rejection.

## Out of scope

- Creating, deleting, or reordering playlists. Reading them is the ask.
- Adding a track to a playlist.
- Artists and subscriptions as their own tabs. The same pattern reaches them.
- Thumbnail rendering in the terminal. The URLs will be parsed and carried, since
  they arrive in the same response for free, but drawing them needs
  `ratatui-image` and protocol detection, and is its own piece of work.
- Tests. None will be added unless asked for.

## Verification

- The contract probe of step 1 above, recorded in
  `docs/verified-ytm-contract.md`.
- Each route exercised by hand with `curl` against a signed-in daemon, checking
  that a feed returns plausible items and that a like survives a page reload,
  which is what proves it reached the account rather than the DOM.
- The interface driven by hand across all five tabs, a drill-down, and a like,
  with the page unloaded first, so the wake path is exercised by a browse and not
  only by playback.

## Open risks

- **Unverified browseIds and like endpoints.** Settled by the probe before any
  parser is written. Highest risk, cheapest to retire.
- **Grid item shapes.** Playlists and albums are the least certain part of the
  parse, and they are the part five tabs added. If `musicTwoRowItemRenderer`
  turns out not to be what those feeds return, the item parser grows a third
  shape rather than the design changing.
- **Very large libraries.** The page cap bounds memory and time, but a library of
  tens of thousands of liked songs will be truncated. Paging on demand, rather
  than to a cap, is the fix if it becomes real.
- **Like state for the now-playing track.** If neither InnerTube nor the player
  bar exposes it cleanly, the heart on the now-playing line shows only what this
  session set, and says nothing when unknown, rather than guessing.
- **One list slot, two clients.** Two clients pointed at one daemon already
  overwrite each other's searches; they will now overwrite each other's panes.
  Same trade, same reason: this is for one person at one keyboard.

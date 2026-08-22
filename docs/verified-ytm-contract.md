# Verified YT Music JS contract
Verified 2026-08-20 against live music.youtube.com via CDP (Brave 151, UA-spoofed
to desktop Chrome, logged out). All claims below were executed, not inferred.

## Page config
- `window.ytcfg.data_.INNERTUBE_API_KEY` - present, 39 chars
- `INNERTUBE_CLIENT_NAME` = "WEB_REMIX"
- `INNERTUBE_CLIENT_VERSION` = "1.20260818.08.00"
- `INNERTUBE_CONTEXT` - keys: client, user, request, clickTracking
- `INNERTUBE_CONTEXT_CLIENT_NAME` = 67
- `VISITOR_DATA` - present (520 chars)
- `LOGGED_IN` = false in probe; search AND playback both worked anyway

## Search (no navigation, audio uninterrupted)
POST `/youtubei/v1/search?key=<API_KEY>&prettyPrint=false`
credentials: include
headers: Content-Type: application/json, X-Goog-Visitor-Id, X-YouTube-Client-Name,
         X-YouTube-Client-Version
body: { context: INNERTUBE_CONTEXT, query, params: "EgWKAQIIAWoKEAoQCRADEAQQBQ%3D%3D" }
  (params = songs-only filter; verified working)

Response path to results:
  contents.tabbedSearchResultsRenderer.tabs[0].tabRenderer.content
    .sectionListRenderer.contents[]           # find the one with musicShelfRenderer
    .musicShelfRenderer.contents[]
    .musicResponsiveListItemRenderer

Per item:
  .playlistItemData.videoId                                   # "9RfVp-GhKfs"
  .overlay.musicItemThumbnailOverlayRenderer.content
     .musicPlayButtonRenderer.playNavigationEndpoint.watchEndpoint  # full endpoint, same videoId
  .flexColumns[0].musicResponsiveListItemFlexColumnRenderer.text.runs -> ["Creep"]
  .flexColumns[1]...text.runs -> ["Radiohead"," • ","Creep"," • ","3:59"]
     # runs[0]=artist, runs[2]=album, runs[4]=duration
  .flexColumns[2] -> "2.2B plays"
  .thumbnail.musicThumbnailRenderer.thumbnail.thumbnails[-1].url

## Player object: document.querySelector('#movie_player')
Exists on home page and watch page. All confirmed `typeof === "function"`:
  loadVideoById cueVideoById playVideo pauseVideo seekTo setVolume getVolume
  getVideoData getPlayerState getCurrentTime getDuration nextVideo previousVideo
  getPlaylist getPlaylistIndex isMuted mute unMute setLoop getVideoUrl addEventListener

Verified reads:
  getVideoData() -> { video_id, author, title, isPlayable, errorCode, isLive, ... }
  getPlayerState() -> 1 playing, 2 paused (3 buffering per YT docs)
  getDuration() -> 239 ; getCurrentTime() -> 4.65 ; getVolume() -> 100
  seekTo(60,true) -> getCurrentTime() 62 of 228
  setVolume(42) -> getVolume() 42
  getPlaylist() -> null   # raw player holds no queue; queue lives in YTM

## Track start: use resolveCommand, NOT loadVideoById or location.href
DO: document.querySelector('ytmusic-app').resolveCommand({ watchEndpoint: { videoId } })
  Verified: JS context survived (no page reload), #movie_player getVideoData()
  and .title.ytmusic-player-bar BOTH reported the new track, getPlayerState()=1,
  location updated to /watch?v=<id> via SPA routing.
  `ytmusic-app` also has `.navigate` (function). resolveCommand is the tested path.

DO NOT: p.loadVideoById(id)
  Verified desync: API reported "No Surprises" while player bar still showed
  "Creep". A following .next-button click resynced the UI instead of advancing.
  Raw player and YTM queue disagree.

DO NOT: location.href = '...'
  Verified full page reload - JS context destroyed, #movie_player absent after 9s.

## Transport (DOM, all selectors unique - querySelectorAll length 1)
  #play-pause-button      yt-icon-button, title="Pause"
  .previous-button        yt-icon-button, title="Previous"
  .next-button            yt-icon-button, title="Next"
All are `yt-icon-button` (NOT tp-yt-paper-icon-button). `.click()` works.

CRITICAL: aria-label is null on all three. Read play state from
getPlayerState(), never from aria-label. The attribute carrying the text is `title`.

next-button: verified advances the YTM radio queue coherently after resolveCommand
  (API + bar agree, no reload, URL gains &list=RD...).
  The &list=RD is the problem a list has to work around: a lone watchEndpoint
  always gets a radio, so following a real list means resolveCommand-ing the
  next row shortly before the current track ends rather than clicking this.
previous-button: no-op when deep into a track (YTM restarts instead). After
  seekTo(2) it correctly returned to the prior track. Matches YTM's own UX.

## Now-playing DOM (works, but redundant given getVideoData)
  .title.ytmusic-player-bar   -> "Let Down"
  .byline.ytmusic-player-bar  -> "Radiohead • OK Computer OKNOTOK 1997 2017 • 1997"

## User-agent risk (NOT yet resolved)
music.youtube.com served "Your browser is deprecated. Please upgrade." to the
HeadlessChrome UA; ytcfg/movie_player/ytmusic-app were all absent. Overriding to
a desktop Chrome UA fixed it completely. Tauri's WKWebView UA is untested against
YTM. Mitigation: set an explicit known-good UA on WebviewWindowBuilder.

## Tauri integration findings (verified 2026-08-20 in this project)

### Remote pages need an application permission, not just `remote.urls`
A capability granting `remote: { urls: ["https://music.youtube.com/*"] }` is
necessary but NOT sufficient. Every `invoke` from the page failed with:

    report_state not allowed. Plugin not found

Application commands need their own permission file, `player/permissions/*.toml`:

    [[permission]]
    identifier = "allow-reporting"
    commands.allow = ["report_state", "report_list", "report_command"]

referenced from the capability WITHOUT a namespace prefix ("allow-reporting",
not "<plugin>:allow-reporting" - the prefix form is for plugins only). After
adding it, `/state` began reporting `ready: true`.

The capability now grants only this application permission; it deliberately
omits Tauri's broad `core:default` permission set.

### `withGlobalTauri` is required
`window.__TAURI__` only exists when `app.withGlobalTauri` is true in
tauri.conf.json. Without it the injected script has no IPC entry point.

### `frontendDist` resolves relative to tauri.conf.json
With the config at `player/tauri.conf.json`, `"../dist"` resolves to the
workspace root. The correct value for `player/dist/` is `"dist"`.

### An icon is mandatory even with bundling disabled
`generate_context!` panics on a missing `icons/icon.png` regardless of
`bundle.active = false`, and `bundle.icon = []` does not suppress it.

### WKWebView user agent - RESOLVED
Previously an open risk. With the pinned desktop Chrome UA set via
`WebviewWindowBuilder::user_agent`, the hidden WKWebView loads YT Music fully:
`document.title == "YouTube Music"`, and ytcfg, `#movie_player` and
`ytmusic-app` are all present. Confirmed via the daemon's own `GET /diagnose`.

### Verified end to end through the daemon
These results predate the local control-token hardening. The same routes now
require `X-Xmusic-Token` except for `GET /health`; the YouTube Music behavior
recorded here is unchanged.

    GET  /diagnose      tauri=object invoke=function injected=true ytcfg=true
    GET  /state         ready=true, volume read from the live player
    POST /search        20 results, title/artist/album/duration parsed
    POST /play          resolveCommand: title, videoId, duration all correct
    POST /control       pause, play_pause, next, prev - player bar stays coherent
    POST /seek          +60 applied
    POST /volume        absolute and relative, clamped at 0 and 100
    POST /quit          exits and removes the pid file
Hostile query `deftones "sextape" '); window.__pwned=1; //` round-tripped as a
literal string: search ran, nothing executed.

### Daemon detachment (verified 2026-08-20)
A daemon spawned as a plain child of the client inherits its process group and
controlling terminal, and dies of SIGHUP when the terminal goes away. Observed
directly: the pid file survived while the process did not, proving a signal kill
rather than a clean `RunEvent::Exit`.

Fixed with `libc::setsid()` in `Command::pre_exec`. Verified after the fix:
`ps -o pid,ppid,pgid,sess` reports PPID 1, its own process group, and no
controlling terminal; the daemon survives both client exit and terminal close,
stays fully functional, and is still stopped cleanly by `--kill-daemon`.

### Request signing for signed-in search (partially verified 2026-08-20)
Scheme: `Authorization: <SCHEME> <timestamp>_<sha1("<timestamp> <cookie> <origin>")>`
with origin `https://music.youtube.com`. The cookie and scheme name must match:

    SAPISID            -> SAPISIDHASH
    __Secure-3PAPISID  -> SAPISID3PHASH
    __Secure-1PAPISID  -> SAPISID1PHASH

Verified, signed out:
- No auth cookie present, so the unsigned path runs and search returns 20
  results. `/diagnose` reports `auth = "unsigned (no auth cookie)"`.
- A deliberately bogus `SAPISIDHASH` was ACCEPTED, not rejected: search still
  returned 20 results. Signed out, YouTube ignores a bad Authorization header,
  so a bad signature cannot be detected by observing a failure.
- The refusal fallback was verified by forcing the branch: the unsigned retry
  carried the search and returned 20 results, with `/diagnose` reporting
  `auth = "signed request refused (HTTP 200), retried unsigned"`.

NOT verified: that a correct signature produces personalised results for a real
signed-in account. The probe session had no account. Failure mode if the signing
is wrong is generic results, not a broken search.

### Control calls need a reply channel (verified 2026-08-20)
`WebviewWindow::eval` returns as soon as the script is queued, so a route that
only evals cannot tell success from a no-op. Observed: setting the volume in the
first seconds after startup returned HTTP 200 while doing nothing at all,
because `#movie_player` was not there yet.

Fixed by giving each control call an id that the page echoes back through a
`report_command` IPC call. Verified after the fix:

    POST /volume  before the player exists  -> 409 "the player has not finished loading"
    POST /control before the player exists  -> 409 "the player has not finished loading"
    POST /seek    with no track loaded      -> 409 "nothing is loaded - search for a song first"
    POST /volume  once ready                -> 200, and the value takes effect
                                              (absolute 8 and 55, relative -15 -> 40)
    play/pause/resume/seek/next/prev        -> 200, all effective

Note `#movie_player` exists on the YT Music home page before any track is
loaded, so "player missing" is a narrow window; "nothing loaded" is the common
case and is now reported rather than silently succeeding.

Also note the state snapshot is up to 500ms stale, since the page reports on a
500ms timer. Reading /state immediately after a POST can show the old value.

## Library feeds, playlists and likes (verified 2026-08-21)

Verified against a live, signed-in `music.youtube.com` session in a real desktop
Chrome, calling InnerTube from the page with the same signing scheme `inject.js`
uses. Every browseId, container path and endpoint below was executed and its
response read; none of it is inferred.

### Library feeds

All four return HTTP 200 and share one container:

    contents.singleColumnBrowseResultsRenderer.tabs[0].tabRenderer.content
      .sectionListRenderer.contents[]

| Feed | browseId | Shelf under `contents[]` |
|---|---|---|
| Liked | `FEmusic_liked_videos` | `musicShelfRenderer` |
| Playlists | `FEmusic_liked_playlists` | `gridRenderer` |
| Albums | `FEmusic_liked_albums` | `gridRenderer` |
| History | `FEmusic_history` | `musicShelfRenderer`, one per date group |

History returns several shelves in one response - "Today", "Yesterday", "This
week", "Last week", "August 2026" - so a feed is a list of shelves, not one
shelf. All of them must be walked or most of the history is lost.

### List items differ from search items

Both are `musicResponsiveListItemRenderer`, but the columns are laid out
differently, and a parser written for search alone drops the album and the
duration of every library row:

| | Search | Library / playlist / album |
|---|---|---|
| `flexColumns[0]` | title | title |
| `flexColumns[1]` | `["Artist"," • ","Album"," • ","3:59"]` | `["Artist"]` |
| `flexColumns[2]` | `"14M plays"` | `["Album"]` (a play count on an album page) |
| `fixedColumns[0]` | absent | `["3:47"]`, the duration |

So the presence of `fixedColumns` is what distinguishes the two shapes, and it
is read from the item rather than from which feed was asked for.

Album tracks carry an empty `flexColumns[1]`: the artist is on the album header,
not on each row. `flexColumns[2]` there is a play count.

Common to both shapes:
  .playlistItemData.videoId
  .overlay.musicItemThumbnailOverlayRenderer.content
     .musicPlayButtonRenderer.playNavigationEndpoint.watchEndpoint.videoId
  .thumbnail.musicThumbnailRenderer.thumbnail.thumbnails[-1].url

The first row of `FEmusic_liked_videos` is a "Shuffle all" pseudo-row with one
flex column and no `playlistItemData`. Skipping items without a videoId, which
the existing parser already does, is what handles it.

### Like state arrives with the row

    .menu.menuRenderer.topLevelButtons[0].likeButtonRenderer.likeStatus
       -> "LIKE" | "DISLIKE" | "INDIFFERENT"

Also present beside it: `likesAllowed`, and ready-made `serviceEndpoints[]`
each holding a `likeEndpoint` with `{ status, target: { videoId } }`.

Search responses do NOT carry `topLevelButtons`, so a search row has no like
state at all and must show none rather than showing "not liked".

For the now-playing track the state is in the DOM, on the player bar:

    document.querySelector('ytmusic-player-bar ytmusic-like-button-renderer')
      attribute `like-status`  -> "INDIFFERENT"
      .data.likeStatus         -> "INDIFFERENT"
      .data.target.videoId     -> "bdXPhRj10jQ"

### Grid items

`gridRenderer.items[].musicTwoRowItemRenderer`:

    .title.runs                                  -> "Jpop"
    .subtitle.runs                               -> "Robinx Prhynz Aquino • 3 tracks"
                                                 -> "Single • あれくん • 2023" (album)
    .navigationEndpoint.browseEndpoint.browseId  -> "VLPLZTkI3dRKoHc" (playlist)
                                                 -> "MPREb_3RVL6e1Jcgy" (album)
    .navigationEndpoint.browseEndpoint.browseEndpointContextSupportedConfigs
       .browseEndpointContextMusicConfig.pageType
                                                 -> MUSIC_PAGE_TYPE_PLAYLIST / _ALBUM
    .thumbnailRenderer.musicThumbnailRenderer.thumbnail.thumbnails[-1].url

A playlist browseId is ALREADY `VL`-prefixed here; prefixing it again produces a
dead id. An album browseId is used exactly as it arrives.

The first card of `FEmusic_liked_playlists` is a "New playlist" button carrying
`createPlaylistEndpoint` instead of `browseEndpoint`. Items with no
`browseEndpoint.browseId` must be skipped.

### Drilling into a playlist or an album

Both are a plain `browse` on the id above, and both answer in a DIFFERENT
container from the library feeds - two columns, not one:

    contents.twoColumnBrowseResultsRenderer.secondaryContents
      .sectionListRenderer.contents[]
        .musicPlaylistShelfRenderer   (playlist; also carries .playlistId)
        .musicShelfRenderer           (album)

Their items are `musicResponsiveListItemRenderer` in the library shape above.

### Continuations are the old query-string form

Not `continuationItemRenderer`. The token is on the shelf:

    musicShelfRenderer.continuations[0].nextContinuationData.continuation
    gridRenderer.continuations[0].nextContinuationData.continuation

and is resent as query parameters on the same endpoint, with the ordinary body:

    POST /youtubei/v1/browse?key=…&prettyPrint=false
         &ctoken=<token>&continuation=<token>&type=next

The reply is shaped differently again - no `contents`, no tabs:

    continuationContents.musicShelfContinuation.contents[]   (+ .continuations)
    continuationContents.gridContinuation.items[]            (+ .continuations)

Verified: one continuation on `FEmusic_liked_videos` returned 42 further rows
and a further token.

### Likes

    POST /youtubei/v1/like/like        body { target: { videoId } }  -> 200
    POST /youtubei/v1/like/removelike  body { target: { videoId } }  -> 200

Both answer `{ responseContext, actions }`. Verified end to end on a real
account: a track reading `INDIFFERENT` was liked, re-read as `LIKE` in a fresh
`FEmusic_history` browse, then unliked and re-read as `INDIFFERENT` again. The
account was left exactly as it was found.

A refusal is an HTTP error, not a quiet no-op: `like/like` on a nonexistent
videoId answered HTTP 404 with `{"error":{"code":404,"message":"Requested entity
was not found."}}`. So a like that fails can be detected and the optimistic
heart reverted.

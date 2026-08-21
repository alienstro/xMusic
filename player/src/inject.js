// Injected into music.youtube.com on every load; every DOM path and player method here was verified against a live session, so search goes through InnerTube rather than the DOM, playback starts through `resolveCommand`, and play state comes from `getPlayerState()`. See docs/verified-ytm-contract.md.
(() => {
  'use strict';
  const ORIGIN = 'https://music.youtube.com';
  // The daemon parks the webview on a blank document to give the page's memory
  // back while nothing is playing. None of this applies there: there is no
  // player, and IPC is granted to this origin alone in any case.
  if (location.origin !== ORIGIN) return;
  if (window.__xmInstalled) return;
  window.__xmInstalled = true;
  // Search filter restricting results to songs. Verified working.
  const SONGS_FILTER = 'EgWKAQIIAWoKEAoQCRADEAQQBQ%3D%3D';
  // A fallback, not the real cadence: a hidden WKWebView throttles timers to about a second, so the daemon drives __xmReport() from a Rust timer instead.
  const STATE_POLL_MS = 1000;
  const SEARCH_TIMEOUT_MS = 12_000;

  // A library feed is several requests, not one, so it is given longer than a
  // search: the page follows its own continuations and answers once.
  const BROWSE_TIMEOUT_MS = 25_000;

  // How many continuation pages a feed may follow. A library of tens of
  // thousands of liked songs is truncated rather than loaded forever, and a
  // truncated feed says so instead of looking complete.
  const PAGE_CAP = 12;

  // Verified 2026-08-21 against a signed-in account; see docs/verified-ytm-contract.md.
  const FEEDS = {
    liked: { browseId: 'FEmusic_liked_videos', label: 'Liked' },
    playlists: { browseId: 'FEmusic_liked_playlists', label: 'Playlists' },
    albums: { browseId: 'FEmusic_liked_albums', label: 'Albums' },
    history: { browseId: 'FEmusic_history', label: 'History' },
  };

  // How long a track restored after an idle unload is given to load. Shorter
  // than the daemon's own limit for the same call, so a slow restore is reported
  // by the page rather than timing out underneath it. Generous because a cold
  // page has to route to the watch endpoint and pull a stream before there is a
  // duration to seek within, which was measured taking well over ten seconds.
  const RESTORE_TIMEOUT_MS = 20_000;
  const RESTORE_POLL_MS = 100;

  // Chrome caps a cookie at 400 days and shortens anything longer, so asking for
  // more buys nothing; the floor keeps an already-expired value persistent.
  const MAX_COOKIE_AGE = 400 * 24 * 60 * 60;
  const MIN_COOKIE_AGE = 30 * 24 * 60 * 60;

  // getPlayerState() return values.
  const PLAYING = 1;
  const BUFFERING = 3;

  const log = (...args) => console.log('[xmusic]', ...args);
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  const player = () => document.querySelector('#movie_player');
  const app = () => document.querySelector('ytmusic-app');
  const cfg = () => (window.ytcfg && window.ytcfg.data_) || {};

  // ------------------------------------------------------------ artwork ----

  // Artwork is the largest slice of this page's memory that is not audio: every
  // thumbnail fetched is decoded into a bitmap the session then holds on to. The
  // terminal draws its own artwork from the URLs InnerTube already returns, so
  // no image this page loads is ever looked at by anyone. A 1x1 transparent GIF
  // stands in for each one rather than an empty string, because the page's
  // bindings expect `src` to read back as a URL.
  const BLANK_IMAGE =
    'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';

  // Replaces a reflected image attribute at the prototype level, which is how
  // YouTube Music sets thumbnails: through the property, not the markup.
  function substitute(property, replacement) {
    const original = Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, property);
    if (!original || !original.set) return;
    Object.defineProperty(HTMLImageElement.prototype, property, {
      configurable: true,
      enumerable: original.enumerable,
      get() { return original.get.call(this); },
      set() { original.set.call(this, replacement); },
    });
  }

  function blockImages() {
    substitute('src', BLANK_IMAGE);
    // srcset is where the responsive thumbnail sets arrive, and it is read back
    // far less often, so it can simply go empty.
    substitute('srcset', '');

    // Overridden on images only, never on Element, so nothing else on the page
    // pays for this on every attribute it sets.
    const setAttribute = Element.prototype.setAttribute;
    HTMLImageElement.prototype.setAttribute = function (name, value) {
      switch (String(name).toLowerCase()) {
        case 'src': return setAttribute.call(this, name, BLANK_IMAGE);
        case 'srcset': return setAttribute.call(this, name, '');
        default: return setAttribute.call(this, name, value);
      }
    };

    // CSS backgrounds need no interception: an image a stylesheet has computed
    // away to `none` is never fetched in the first place.
    const style = document.createElement('style');
    style.textContent = '*, *::before, *::after { background-image: none !important; }';
    const attach = () => (document.head || document.documentElement).appendChild(style);
    if (document.head || document.documentElement) attach();
    else document.addEventListener('readystatechange', attach, { once: true });
  }

  // Set by the daemon from XMUSIC_KEEP_IMAGES; see inject_script() in main.rs.
  if (!window.__xmKeepImages) blockImages();

  function invoke(command, args) {
    const tauri = window.__TAURI__;
    if (!tauri || !tauri.core) {
      window.__xmLastError = 'Tauri IPC unavailable';
      return Promise.reject(new Error('Tauri IPC unavailable'));
    }
    return tauri.core.invoke(command, args).catch((error) => {
      // Recorded, not just logged: the webview has no visible console, so GET /diagnose is the only way to see IPC failing.
      window.__xmLastError = `${command}: ${error && error.message || error}`;
      throw error;
    });
  }

  // ---------------------------------------------------------------- state ----

  // The now-playing track's like state, which InnerTube does not report with the
  // player: it is on the player bar's own control. Null while the bar has not
  // rendered, so an unknown state is never drawn as "not liked".
  function nowPlayingLike() {
    const control = document.querySelector('ytmusic-player-bar ytmusic-like-button-renderer');
    if (!control) return null;
    return likedFrom(control.data?.likeStatus || control.getAttribute('like-status'));
  }

  function readState() {
    const p = player();
    if (!p || typeof p.getPlayerState !== 'function') {
      // Either the page is booting, or YT Music served no player at all because it did not recognise the user agent; see user_agent() in main.rs.
      const diagnostic = [
        `url=${location.href}`,
        `title=${JSON.stringify(document.title)}`,
        `ytcfg=${window.ytcfg ? 'yes' : 'no'}`,
        `apiKey=${cfg().INNERTUBE_API_KEY ? 'yes' : 'no'}`,
        `ytmusicApp=${document.querySelector('ytmusic-app') ? 'yes' : 'no'}`,
        `moviePlayer=${p ? 'partial' : 'no'}`,
        `search=${typeof window.__xmSearch}`,
      ].join(' ');
      return { ready: false, apiReady: !!cfg().INNERTUBE_API_KEY, videoId: '',
               title: '', artist: '', byline: '', diagnostic, isPlaying: false,
               isBuffering: false, position: 0, duration: 0, volume: 0,
               muted: false, loggedIn: !!cfg().LOGGED_IN, liked: null };
    }
    const data = p.getVideoData() || {};
    const state = p.getPlayerState();
    const bar = document.querySelector('.byline.ytmusic-player-bar');
    return {
      ready: true,
      // The daemon waits on this before searching a page that has come back from an unload: InnerTube needs the key, not the player.
      apiReady: !!cfg().INNERTUBE_API_KEY,
      videoId: data.video_id || '',
      title: data.title || '',
      artist: data.author || '',
      // Richer than `author` and cosmetic only, so a selector change degrades the label rather than breaking state.
      byline: bar ? bar.textContent.trim() : '',
      diagnostic: '',
      isPlaying: state === PLAYING,
      isBuffering: state === BUFFERING,
      position: Math.floor(p.getCurrentTime() || 0),
      duration: Math.floor(p.getDuration() || 0),
      volume: Math.round(p.getVolume() || 0),
      muted: typeof p.isMuted === 'function' ? !!p.isMuted() : false,
      loggedIn: !!cfg().LOGGED_IN,
      liked: nowPlayingLike(),
    };
  }

  function report() {
    invoke('report_state', { state: readState() }).catch(() => {});
  }

  // Called by the daemon's state pump and after every control action, so a change shows up at once rather than on the next tick.
  window.__xmReport = report;
  setInterval(report, STATE_POLL_MS);

  // ------------------------------------------------------------- innertube ----

  // Google signs API calls with a SHA-1 over "<timestamp> <cookie> <origin>", and the scheme must match the cookie it was derived from or the request is rejected.
  const AUTH_COOKIES = [
    ['SAPISID', 'SAPISIDHASH'],
    ['__Secure-3PAPISID', 'SAPISID3PHASH'],
    ['__Secure-1PAPISID', 'SAPISID1PHASH'],
  ];

  function cookie(name) {
    const escaped = name.replace(/[.*+?^${}()|[\]\\-]/g, '\\$&');
    const match = document.cookie.match(new RegExp('(?:^|;\\s*)' + escaped + '=([^;]*)'));
    return match ? match[1] : null;
  }

  async function sha1Hex(text) {
    const digest = await crypto.subtle.digest('SHA-1', new TextEncoder().encode(text));
    return Array.from(new Uint8Array(digest))
      .map((b) => b.toString(16).padStart(2, '0'))
      .join('');
  }

  async function authorization() {
    for (const [name, scheme] of AUTH_COOKIES) {
      const value = cookie(name);
      if (!value) continue;
      const timestamp = Math.floor(Date.now() / 1000);
      return `${scheme} ${timestamp}_${await sha1Hex(`${timestamp} ${value} ${ORIGIN}`)}`;
    }
    return null;
  }

  /// One InnerTube call.
  ///
  /// `options.params` becomes query string rather than body, which is the only
  /// form the continuation endpoint accepts. `options.signedOnly` suppresses the
  /// unsigned retry below, for a call that changes something on the account: an
  /// unsigned like cannot work, and retrying one only replaces the real refusal
  /// with a 401 that says nothing about why.
  async function innertube(endpoint, body, signal, options = {}) {
    const c = cfg();
    if (!c.INNERTUBE_API_KEY) throw new Error('ytcfg not ready');

    const query = new URLSearchParams({ key: c.INNERTUBE_API_KEY, prettyPrint: 'false', ...options.params });
    const url = `/youtubei/v1/${endpoint}?${query}`;
    const payload = JSON.stringify({ context: c.INNERTUBE_CONTEXT, ...body });
    const headers = {
      'Content-Type': 'application/json',
      'X-Goog-Visitor-Id': c.VISITOR_DATA || '',
      'X-Goog-AuthUser': '0',
      'X-YouTube-Client-Name': String(c.INNERTUBE_CONTEXT_CLIENT_NAME ?? 67),
      'X-YouTube-Client-Version': c.INNERTUBE_CLIENT_VERSION || '',
    };

    const send = (extra) => fetch(url, {
      method: 'POST',
      credentials: 'include',
      headers: { ...headers, ...extra },
      body: payload,
      signal,
    });

    const auth = await authorization();
    if (auth) {
      const signed = await send({ Authorization: auth });
      if (signed.ok) {
        window.__xmAuth = `signed (${auth.split(' ')[0]})`;
        return signed.json();
      }
      if (options.signedOnly) {
        throw new Error(`innertube/${endpoint} HTTP ${signed.status}`);
      }
      // Signing personalises results but an unsigned search still works, so falling back keeps a refused signature from leaving a signed-in user worse off than a signed-out one.
      window.__xmAuth = `signed request refused (HTTP ${signed.status}), retried unsigned`;
    } else {
      window.__xmAuth = 'unsigned (no auth cookie)';
    }

    const plain = await send({});
    if (!plain.ok) throw new Error(`innertube/${endpoint} HTTP ${plain.status}`);
    return plain.json();
  }

  const columnRuns = (column) => {
    const runs = column?.musicResponsiveListItemFlexColumnRenderer?.text?.runs;
    return Array.isArray(runs) ? runs.map((run) => run.text) : [];
  };

  const fixedRuns = (column) => {
    const runs = column?.musicResponsiveListItemFixedColumnRenderer?.text?.runs;
    return Array.isArray(runs) ? runs.map((run) => run.text) : [];
  };

  const runsText = (node) => (node?.runs || []).map((run) => run.text).join('');

  const DURATION_RE = /^\d{1,2}:\d{2}(:\d{2})?$/;

  // An album page puts a play count where a library row puts the album, and a
  // history row appends a view count to the artist.
  const COUNT_RE = /(plays|views)$/;

  // Parsed because it arrives in the same response for free. Nothing in this
  // process ever looks at it: the terminal draws its own artwork from this URL.
  const largestThumbnail = (renderer) => {
    const thumbnails = renderer?.musicThumbnailRenderer?.thumbnail?.thumbnails;
    return Array.isArray(thumbnails) && thumbnails.length
      ? thumbnails[thumbnails.length - 1].url
      : '';
  };

  // Null where the response says nothing, which is not the same as "not liked":
  // search results carry no like state at all, and a heart that guessed would be
  // wrong on every search row.
  function likedFrom(status) {
    if (status === 'LIKE') return true;
    if (status === 'DISLIKE' || status === 'INDIFFERENT') return false;
    return null;
  }

  /// One row of a list, whatever produced it.
  ///
  /// Search lays its metadata out as a single "artist • album • duration"
  /// column; a library, playlist or album row uses one flex column per field and
  /// a fixed column for the duration. Which shape this is gets read from the item
  /// rather than from the feed that was asked for, so a feed that changes shape
  /// loses fields instead of filling them with the wrong ones.
  function parseListItem(item) {
    const videoId =
      item.playlistItemData?.videoId ||
      item.overlay?.musicItemThumbnailOverlayRenderer?.content
        ?.musicPlayButtonRenderer?.playNavigationEndpoint?.watchEndpoint?.videoId;
    // Also what skips the "Shuffle all" row at the head of the liked feed.
    if (!videoId) return null;

    const fixed = fixedRuns(item.fixedColumns?.[0]).find((text) => DURATION_RE.test(text)) || '';
    let artist = '';
    let album = '';
    let duration = fixed;

    if (fixed) {
      // One field per column, except that a history row carries a view count
      // beside the artist and an album page a play count where the album goes.
      const meta = columnRuns(item.flexColumns?.[1]).filter((text) => text.trim() !== '•');
      artist = meta.find((text) => !COUNT_RE.test(text)) || '';
      const third = columnRuns(item.flexColumns?.[2]).join('');
      album = COUNT_RE.test(third) ? '' : third;
    } else {
      // Singles omit the album and shift every index, so find the duration by shape.
      const meta = columnRuns(item.flexColumns?.[1]).filter((text) => text.trim() !== '•');
      const durationAt = meta.findIndex((text) => DURATION_RE.test(text));
      const rest = meta.filter((_, index) => index !== durationAt);
      artist = rest[0] || '';
      album = rest[1] || '';
      duration = durationAt >= 0 ? meta[durationAt] : '';
    }

    return {
      videoId,
      browseId: '',
      title: columnRuns(item.flexColumns?.[0]).join(''),
      artist,
      album,
      duration,
      liked: likedFrom(
        item.menu?.menuRenderer?.topLevelButtons?.[0]?.likeButtonRenderer?.likeStatus,
      ),
      thumbnail: largestThumbnail(item.thumbnail),
    };
  }

  /// One playlist or album card.
  ///
  /// Carries an id to open rather than a videoId and a duration. The id is used
  /// exactly as it arrives: a playlist's already carries its own `VL` prefix, and
  /// prefixing it again produces a dead id. Cards with no id — the "New playlist"
  /// button at the head of the playlists grid — are skipped, which is also what
  /// keeps anything else the shelf grows out of the list.
  function parseGridItem(item) {
    const browseId = item.navigationEndpoint?.browseEndpoint?.browseId;
    if (!browseId) return null;
    return {
      videoId: '',
      browseId,
      title: runsText(item.title),
      artist: runsText(item.subtitle),
      album: '',
      duration: '',
      liked: null,
      thumbnail: largestThumbnail(item.thumbnailRenderer),
    };
  }

  /// Every container a search, a browse or a continuation can answer in.
  ///
  /// They differ in where their shelves sit, not in what their items are, so the
  /// walk ends here and one item parser serves all of them.
  function sections(json) {
    const out = [];
    const push = (list) => out.push(...(list || []));

    for (const tab of json?.contents?.tabbedSearchResultsRenderer?.tabs || []) {
      push(tab?.tabRenderer?.content?.sectionListRenderer?.contents);
    }
    // A library feed. History answers with one shelf per date group, so all of
    // them are walked; taking the first would lose most of the history.
    for (const tab of json?.contents?.singleColumnBrowseResultsRenderer?.tabs || []) {
      push(tab?.tabRenderer?.content?.sectionListRenderer?.contents);
    }
    // A playlist or an album, which answer in two columns rather than one.
    push(json?.contents?.twoColumnBrowseResultsRenderer?.secondaryContents
      ?.sectionListRenderer?.contents);

    // A continuation answers with the shelf alone and no container around it.
    for (const [key, shelf] of Object.entries(json?.continuationContents || {})) {
      out.push(key === 'gridContinuation' ? { gridRenderer: shelf } : { musicShelfRenderer: shelf });
    }
    return out;
  }

  const continuationToken = (shelf) =>
    shelf?.continuations?.[0]?.nextContinuationData?.continuation || '';

  /// Walks one response into rows, with the token that asks for the rest.
  function parseMusicList(json) {
    const items = [];
    let continuation = '';
    for (const section of sections(json)) {
      const list = section.musicShelfRenderer || section.musicPlaylistShelfRenderer;
      if (list) {
        for (const entry of list.contents || []) {
          const row = entry.musicResponsiveListItemRenderer && parseListItem(entry.musicResponsiveListItemRenderer);
          if (row) items.push(row);
        }
        continuation = continuationToken(list) || continuation;
        continue;
      }
      const grid = section.gridRenderer;
      if (grid) {
        for (const entry of grid.items || []) {
          const card = entry.musicTwoRowItemRenderer && parseGridItem(entry.musicTwoRowItemRenderer);
          if (card) items.push(card);
        }
        continuation = continuationToken(grid) || continuation;
      }
    }
    return { items, continuation };
  }

  /// Loads one browse id whole, following its continuations here in the page.
  ///
  /// Rust asks for a feed and receives a list; it never learns what a
  /// continuation token is. Stops at `PAGE_CAP` and says that it did.
  async function loadList(browseId, signal) {
    const { items, continuation } = parseMusicList(
      await innertube('browse', { browseId }, signal),
    );
    let token = continuation;
    let pages = 1;
    while (token && pages < PAGE_CAP) {
      const page = parseMusicList(
        await innertube('browse', {}, signal, {
          params: { ctoken: token, continuation: token, type: 'next' },
        }),
      );
      // A page that adds nothing is the end of the feed, whatever token it
      // still offers: the playlists grid keeps handing one back forever. Clearing
      // the token is what stops that being reported as a truncated feed.
      if (!page.items.length) {
        token = '';
        break;
      }
      items.push(...page.items);
      token = page.continuation;
      pages += 1;
    }
    return { items, truncated: !!token };
  }

  /// Runs one list-producing call and reports it, whatever produced it.
  ///
  /// `seq` is echoed back so the daemon can discard results for a list the user
  /// has already moved past, and `source` names which pane asked.
  async function reportList(seq, source, label, limit, load) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), limit);
    try {
      const { items, truncated } = await load(controller.signal);
      log(`${source} "${label}" -> ${items.length} items${truncated ? ' (truncated)' : ''}`);
      await invoke('report_list', { seq, label, items, truncated, error: null });
    } catch (error) {
      log(`${source} failed`, error);
      const message = error && error.name === 'AbortError'
        ? `${source} timed out after ${limit / 1000}s`
        : String((error && error.message) || error);
      await invoke('report_list',
        { seq, label, items: [], truncated: false, error: message })
        .catch(() => {});
    } finally {
      clearTimeout(timeout);
    }
  }

  window.__xmSearch = (seq, query) =>
    reportList(seq, 'search', query, SEARCH_TIMEOUT_MS, async (signal) => ({
      items: parseMusicList(
        await innertube('search', { query, params: SONGS_FILTER }, signal),
      ).items,
      truncated: false,
    }));

  window.__xmBrowse = (seq, feed) => {
    const known = FEEDS[feed];
    return reportList(seq, feed, known ? known.label : feed, BROWSE_TIMEOUT_MS, (signal) => {
      if (!known) throw new Error(`unknown feed "${feed}"`);
      return loadList(known.browseId, signal);
    });
  };

  // One playlist's or album's tracks. The id is whatever the grid card carried.
  window.__xmPlaylist = (seq, browseId) =>
    reportList(seq, 'playlist', browseId, BROWSE_TIMEOUT_MS, (signal) => loadList(browseId, signal));

  // ---------------------------------------------------------------- control ----

  function click(selector) {
    const element = document.querySelector(selector);
    if (!element) return `YouTube Music has no ${selector} control on this page`;
    element.click();
    return null;
  }

  // Each returns null on success or a sentence explaining why not, so the daemon can pass a real reason back.
  function play(videoId) {
    const a = app();
    if (a && typeof a.resolveCommand === 'function') {
      a.resolveCommand({ watchEndpoint: { videoId } });
      return null;
    }
    return 'YouTube Music command routing is unavailable';
  }

  const loaded = (p) => !!(p.getVideoData() || {}).video_id;

  // The daemon unloads this page when it goes idle, which loses whatever was
  // loaded with it. Putting the track back paused and where it was keeps
  // "pause, walk away, press play" meaning what it says. Only the track comes
  // back: the queue it belonged to lived in the document that is gone.
  async function restore(videoId, position) {
    const failure = play(videoId);
    if (failure) return failure;
    // Measured against the clock rather than by counting polls: a hidden
    // WKWebView throttles `setTimeout` to about a second, so a hundred nominal
    // 100ms waits are a hundred real seconds, and the daemon gives up on this
    // long before the loop does.
    const deadline = Date.now() + RESTORE_TIMEOUT_MS;
    while (Date.now() < deadline) {
      await sleep(RESTORE_POLL_MS);
      const p = player();
      // resolveCommand starts playing as soon as the stream arrives, so wait for
      // a loaded player before seeking, then hand it back paused.
      const duration = p && loaded(p) ? p.getDuration() || 0 : 0;
      if (!duration) continue;
      if (position > 0) p.seekTo(Math.min(position, duration), true);
      p.pauseVideo();
      return null;
    }
    return 'the restored track did not load in time';
  }

  async function transport(action) {
    const p = player();
    if (!p) return 'the player has not finished loading';
    // Transport controls are no-ops on an empty player, and a no-op reporting success looks exactly like a broken button.
    if (!loaded(p)) return 'nothing is loaded — search for a song first';
    switch (action) {
      case 'play':
        p.playVideo();
        return null;
      case 'pause':
        p.pauseVideo();
        return null;
      case 'play_pause':
        if (p.getPlayerState() === PLAYING) p.pauseVideo();
        else p.playVideo();
        return null;
      // The queue belongs to YouTube Music, not the raw player, so click the real buttons: getPlaylist() is null and nextVideo() leaves the player bar behind.
      case 'next':
        return click('.next-button');
      case 'prev':
        // A few seconds in, YouTube Music restarts the track instead of stepping back, so rewind first and let "prev" mean prev.
        if ((p.getCurrentTime() || 0) > 3) {
          p.seekTo(0, true);
          await sleep(150);
        }
        return click('.previous-button');
      default:
        return `unknown transport action "${action}"`;
    }
  }

  function seek(value, relative) {
    const p = player();
    if (!p) return 'the player has not finished loading';
    if (!loaded(p)) return 'nothing is loaded — search for a song first';
    const duration = p.getDuration() || 0;
    const target = relative ? (p.getCurrentTime() || 0) + value : value;
    p.seekTo(Math.max(0, duration ? Math.min(target, duration) : target), true);
    return null;
  }

  function volume(value, relative) {
    const p = player();
    if (!p) return 'the player has not finished loading';
    const target = relative ? (p.getVolume() || 0) + value : value;
    p.setVolume(Math.max(0, Math.min(100, Math.round(target))));
    return null;
  }

  // These lose the HttpOnly flag their browser set, which only stops scripts reading a cookie and has no bearing on whether Google accepts one.
  function adopt(cookies) {
    if (!Array.isArray(cookies) || cookies.length === 0) {
      return 'no cookies were supplied';
    }
    // A cookie set without a lifetime is a session cookie, and WebKit drops
    // those when the process ends, so an imported session would not survive a
    // single restart of the daemon. Carry the browser's own expiry across.
    const now = Date.now() / 1000;
    let written = 0;
    for (const cookie of cookies) {
      if (!cookie || !cookie.name) continue;
      const remaining = cookie.expires ? cookie.expires - now : 0;
      // Google rotates most of these itself, so an expiry it has already passed
      // is no reason to import a cookie as one that dies on exit.
      const age = Math.round(Math.min(MAX_COOKIE_AGE, Math.max(MIN_COOKIE_AGE, remaining)));
      document.cookie =
        `${cookie.name}=${cookie.value || ''}; domain=.youtube.com; path=/` +
        `; max-age=${age}; Secure; SameSite=None`;
      written += 1;
    }
    // Setting a cookie fails silently if the page rejects the attributes, so confirm one survived rather than reporting success on faith.
    if (!document.cookie.includes('SAPISID')) {
      return 'the page would not keep the imported cookies';
    }
    log(`adopted ${written} cookies`);
    return null;
  }

  // One more endpoint on the same signed, same-origin path the feeds use. A
  // refusal is an HTTP error rather than a quiet no-op, so `innertube` throwing
  // is what lets the interface put its optimistic heart back.
  async function like(videoId, liked) {
    if (!videoId) return 'nothing to like';
    await innertube(
      liked ? 'like/like' : 'like/removelike',
      { target: { videoId } },
      undefined,
      { signedOnly: true },
    );
    return null;
  }

  // Single entry point for every control call; `id` is echoed back so the waiting request learns what happened rather than that a script was queued.
  window.__xmDispatch = async (id, action, args) => {
    let error = null;
    try {
      switch (action) {
        case 'play':
          error = play(args.videoId);
          break;
        case 'transport':
          error = await transport(args.action);
          break;
        case 'seek':
          error = seek(args.value, args.relative);
          break;
        case 'volume':
          error = volume(args.value, args.relative);
          break;
        case 'adopt_cookies':
          error = adopt(args.cookies);
          break;
        case 'restore':
          error = await restore(args.videoId, args.position);
          break;
        case 'like':
          error = await like(args.videoId, args.liked);
          break;
        default:
          error = `unknown action "${action}"`;
      }
    } catch (thrown) {
      error = String((thrown && thrown.message) || thrown);
    }
    if (error) log(`${action} failed: ${error}`);
    // Whoever is waiting on this call is about to redraw, and should redraw with the state it just produced.
    report();
    invoke('report_command', { id, ok: !error, error }).catch(() => {});
  };

  log('injected');
})();

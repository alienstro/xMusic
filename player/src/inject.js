// Injected into music.youtube.com on every load; every DOM path and player method here was verified against a live session, so search goes through InnerTube rather than the DOM, playback starts through `resolveCommand`, and play state comes from `getPlayerState()`. See docs/verified-ytm-contract.md.
(() => {
  'use strict';
  if (window.__xmInstalled) return;
  window.__xmInstalled = true;

  const ORIGIN = 'https://music.youtube.com';
  // Search filter restricting results to songs. Verified working.
  const SONGS_FILTER = 'EgWKAQIIAWoKEAoQCRADEAQQBQ%3D%3D';
  // A fallback, not the real cadence: a hidden WKWebView throttles timers to about a second, so the daemon drives __xmReport() from a Rust timer instead.
  const STATE_POLL_MS = 1000;
  const SEARCH_TIMEOUT_MS = 12_000;

  // getPlayerState() return values.
  const PLAYING = 1;
  const BUFFERING = 3;

  const log = (...args) => console.log('[xmusic]', ...args);
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  const player = () => document.querySelector('#movie_player');
  const app = () => document.querySelector('ytmusic-app');
  const cfg = () => (window.ytcfg && window.ytcfg.data_) || {};

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
      return { ready: false, videoId: '', title: '', artist: '', byline: '',
               diagnostic, isPlaying: false, isBuffering: false, position: 0,
               duration: 0, volume: 0, muted: false, loggedIn: !!cfg().LOGGED_IN };
    }
    const data = p.getVideoData() || {};
    const state = p.getPlayerState();
    const bar = document.querySelector('.byline.ytmusic-player-bar');
    return {
      ready: true,
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

  async function innertube(endpoint, body, signal) {
    const c = cfg();
    if (!c.INNERTUBE_API_KEY) throw new Error('ytcfg not ready');

    const url = `/youtubei/v1/${endpoint}?key=${c.INNERTUBE_API_KEY}&prettyPrint=false`;
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

  const DURATION_RE = /^\d{1,2}:\d{2}(:\d{2})?$/;

  function parseSearchResponse(json) {
    const tabs = json?.contents?.tabbedSearchResultsRenderer?.tabs || [];
    const results = [];
    for (const tab of tabs) {
      const sections = tab?.tabRenderer?.content?.sectionListRenderer?.contents || [];
      for (const section of sections) {
        for (const entry of section?.musicShelfRenderer?.contents || []) {
          const item = entry.musicResponsiveListItemRenderer;
          if (!item) continue;
          const videoId =
            item.playlistItemData?.videoId ||
            item.overlay?.musicItemThumbnailOverlayRenderer?.content
              ?.musicPlayButtonRenderer?.playNavigationEndpoint?.watchEndpoint?.videoId;
          if (!videoId) continue;

          // The second column is "artist • album • duration", so find the duration by shape: singles omit the album and shift every index.
          const meta = columnRuns(item.flexColumns?.[1]).filter((t) => t.trim() !== '•');
          const durationAt = meta.findIndex((t) => DURATION_RE.test(t));
          const rest = meta.filter((_, i) => i !== durationAt);

          results.push({
            videoId,
            title: columnRuns(item.flexColumns?.[0]).join(''),
            artist: rest[0] || '',
            album: rest[1] || '',
            duration: durationAt >= 0 ? meta[durationAt] : '',
          });
        }
      }
    }
    return results;
  }

  // `seq` is echoed back so the daemon can discard results for a search the user has already replaced.
  window.__xmSearch = async (seq, query) => {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), SEARCH_TIMEOUT_MS);
    try {
      const json = await innertube(
        'search',
        { query, params: SONGS_FILTER },
        controller.signal,
      );
      const results = parseSearchResponse(json);
      log(`search "${query}" -> ${results.length} results`);
      await invoke('report_search_results', { seq, query, results, error: null });
    } catch (error) {
      log('search failed', error);
      const message = error && error.name === 'AbortError'
        ? `search timed out after ${SEARCH_TIMEOUT_MS / 1000}s`
        : String(error && error.message || error);
      await invoke('report_search_results',
        { seq, query, results: [], error: message })
        .catch(() => {});
    } finally {
      clearTimeout(timeout);
    }
  };

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
    const attributes = '; domain=.youtube.com; path=/; Secure; SameSite=None';
    let written = 0;
    for (const cookie of cookies) {
      if (!cookie || !cookie.name) continue;
      document.cookie = `${cookie.name}=${cookie.value || ''}${attributes}`;
      written += 1;
    }
    // Setting a cookie fails silently if the page rejects the attributes, so confirm one survived rather than reporting success on faith.
    if (!document.cookie.includes('SAPISID')) {
      return 'the page would not keep the imported cookies';
    }
    log(`adopted ${written} cookies`);
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

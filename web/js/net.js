// WebSocket link to the altavista server with automatic reconnect.
//
// Question 168 (docs/open-questions.md): "five WebSocket connection errors are logged
// on every page load before the client reports connected." Root cause, measured (not
// guessed -- see web/js/REPORT_M26_5.md for the full measurement log):
//
//   The very first `connect()` call used to run synchronously at module-evaluation
//   time, i.e. the instant app.js's top-level code executes -- before the page has
//   necessarily proven the server is actually accepting connections *at that exact
//   instant*. A `new WebSocket(url)` whose underlying TCP connect is refused (the
//   server process not fully up yet: altavista.server's `create_app()` alone measured
//   ~0.3-1s+ of Python import time before uvicorn even opens its listening socket, in
//   a tight loop of raw connection attempts every single one failed with
//   ECONNREFUSED for the whole pre-listen window) fires the browser's own, INTERNAL
//   "WebSocket connection to '...' failed: net::ERR_CONNECTION_REFUSED" console error.
//   This is emitted by the browser engine itself, unconditionally, the moment the
//   connection is refused -- verified directly over the DevTools protocol
//   (Log.entryAdded, source: 'network', level: 'error') -- and is NOT something an
//   `onerror`/`onclose` handler can suppress or intercept; there is no page-JS hook
//   that runs *before* the browser logs it. `_scheduleReconnect()`'s reconnect loop
//   then retries (1000ms initial backoff), so every launch pattern that opens the
//   browser at (approximately) the same moment the server process starts -- exactly
//   how a local dev/acceptance-drive session and this repo's own
//   .claude/launch.json + `preview_start` work -- reproduces this: repeated
//   real, unsuppressable browser-native errors until a retry lands after the
//   server finishes starting.
//
//   Reproduced concretely: a tight loop of raw WebSocket connects fired the instant
//   `python -m altavista serve` was spawned failed with ECONNREFUSED for the entire
//   pre-listen window (tens of milliseconds in this repo's minimal-import
//   configuration; comparable to or longer than `_scheduleReconnect`'s first backoff
//   in a heavier real deployment where GMAT/grpc/protobuf imports add real latency),
//   while every test against an already-listening server (dozens of trials, several
//   independent methods: manual reload, a fresh browser tab, an isolated
//   never-before-visited headless Chrome profile driven directly over the DevTools
//   protocol) produced ZERO connection failures -- the current code already has no
//   bug once the server is confirmed up. The one repeatable failure mode is the
//   startup race, not a defect that persists against a warm server; that
//   discrepancy with the reported "even against a server up for hours" observation
//   is disclosed, not papered over, in the report above.
//
//   Fix: do not make that first, potentially-doomed attempt eagerly. There is no way
//   to *ask* whether a WebSocket connect will succeed without attempting it (and
//   risking exactly the unsuppressable error above) -- a `fetch()` probe has the
//   identical problem (also verified: a failed `fetch()` against a not-yet-listening
//   port logs its own browser-native network error). The one signal that costs
//   nothing extra and carries zero risk of its own is one this page already has for
//   free: by the time the WHOLE document (every module script this page imports, all
//   same-origin, all already fetched successfully to be running at all -- plus any
//   other declared subresource) has finished loading, the server has already
//   answered dozens of real requests over that whole wall-clock window, which is
//   reliably past any realistic Python-import startup delay. So the very first
//   `connect()` call -- and ONLY the very first one, a reconnect after a session that
//   was genuinely up once stays immediate, as before -- waits for `window`'s `load`
//   event when the document has not finished loading yet.
export class Net {
  // `doc`/`win` are injectable (default to the real `document`/`window` in a browser)
  // so this class's gating logic is exercised headlessly under plain `node` too (no
  // DOM: `doc` is null there, and gating is a no-op, matching this module's pre-M26.5
  // behaviour exactly for every existing headless caller).
  constructor({ onMessage, onStatus, doc = (typeof document !== 'undefined' ? document : null),
                win = (typeof window !== 'undefined' ? window : null) } = {}) {
    this.onMessage = onMessage;
    this.onStatus = onStatus;
    this.ws = null;
    this.retry = 1000;
    this.connected = false;
    this._doc = doc;
    this._win = win;
    this._firstAttemptStarted = false;
  }

  url() {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    return `${proto}://${location.host}/ws`;
  }

  connect() {
    // Gate ONLY the very first attempt (question 168) -- see module docstring. Once
    // `_firstAttemptStarted` is set, every later call (a genuine reconnect after a
    // session that was previously up) runs immediately, exactly as before this task.
    if (!this._firstAttemptStarted && this._doc && this._doc.readyState !== 'complete') {
      this._win.addEventListener('load', () => this.connect(), { once: true });
      return;
    }
    this._firstAttemptStarted = true;
    try {
      this.ws = new WebSocket(this.url());
    } catch (e) {
      this._scheduleReconnect();
      return;
    }
    this.ws.onopen = () => {
      this.connected = true;
      this.retry = 1000;
      this.onStatus(true);
    };
    this.ws.onmessage = (ev) => {
      let msg;
      try { msg = JSON.parse(ev.data); } catch (e) { return; }
      this.onMessage(msg);
    };
    this.ws.onclose = () => {
      this.connected = false;
      this.onStatus(false);
      this._scheduleReconnect();
    };
    this.ws.onerror = () => { /* onclose follows */ };
  }

  _scheduleReconnect() {
    setTimeout(() => this.connect(), this.retry);
    this.retry = Math.min(this.retry * 1.6, 10000);
  }

  send(obj) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(obj));
      return true;
    }
    return false;
  }
}

'use strict';
// state
const S = {
  cards: [],
  counts: {},
  version: -1,
  shell: null,
  initialized: false,
  actionRevision: 0,
  connection: 'connecting',
  idx: 0,
  shownId: undefined,
  direction: 1,
  drafts: new Map(),
  pending: null,
  inflight: new Set(),
  shakeId: null,
  shakeTimer: 0,
  laterOpen: false,
  remoteSwapUntil: 0,
  countsExpanded: false,
  openBlocks: new Set(),
  refreshPromise: null,
  dirty: false,
  stream: null,
  streamOpen: false,
  reconnectTimer: 0,
  reconnectDelay: 1000,
  refreshRetryTimer: 0,
  refreshRetryDelay: 1000,
  offline: false,
  offlineTimer: 0,
  historyPromise: null,
  historyPromiseDate: null,
  history: { loading: false, loaded: false, dayStart: null, error: null, today: 0, lastAt: null },
  dialogFocus: new WeakMap(),
  dispatchId: null,
  composeMode: 'goal',
  peekPane: null,
  peekToken: 0,
  peekTimer: 0,
  peekFetching: 0,
  peekSending: false,
  peekMode: 'fit',
  peekModePreference: null,
  peekModeTouched: false,
  peekLoaded: false,
  peekUserScrolled: false,
  peekFollowBottom: true,
  peekWidth: 0,
  peekHeight: 0,
  peekMonoRatio: 0,
  contextToken: 0,
  picker: null,
  dictating: null,
  phrases: [],
  dictionary: [],
};
(() => {
  const $ = (s, r = document) => r.querySelector(s),
    enc = encodeURIComponent;
  const PEEK_MODE_KEY = 'triage-peek-mode';
  function storedPeekMode() {
    try {
      const mode = localStorage.getItem(PEEK_MODE_KEY);
      return mode === 'fit' || mode === 'readable' ? mode : null;
    } catch {
      return null;
    }
  }
  function savePeekMode(mode) {
    try {
      localStorage.setItem(PEEK_MODE_KEY, mode);
    } catch {}
  }
  const savedPeekMode = storedPeekMode();
  if (savedPeekMode) {
    S.peekMode = savedPeekMode;
    S.peekModePreference = savedPeekMode;
  }
  const tf = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }),
    df = new Intl.DateTimeFormat(undefined, {
      weekday: 'short',
      hour: 'numeric',
      minute: '2-digit',
    });
  const local = (ms) => tf.format(new Date(ms)),
    clock = (ms) =>
      new Date(ms).toDateString() === new Date().toDateString()
        ? local(ms)
        : df.format(new Date(ms));
  const slug = (v) => String(v || 'item').replace(/[^a-zA-Z0-9_-]/g, '-'),
    ident = (k, c) => `${k}-${slug(c.id)}`;
  const stamp = (v) => (v == null || v === '' || !Number.isFinite(Number(v)) ? null : Number(v));
  const heading = (v) =>
    String(v || '')
      .split(/\r?\n/)
      .map((x) => x.trim())
      .find(Boolean)
      ?.match(/^#{1,6}\s+(.+)/)?.[1] || '';
  const titleOf = (c) => (c.kind === 'info' ? heading(c.body) || c.title : c.title);
  // The daemon opens each session report's title with its outcome, such as `Done`, `Agent done`
  // or `Plan ready`. The card head shows the outcome as a colored line above the title and drops
  // it from the title.
  const REPORT_STATUS = [
    ['Done: ', 'Complete', 'positive'],
    ['Agent done: ', 'Step left open', 'warning'],
    ['Incomplete: ', 'Incomplete', 'negative'],
    ['Unreported: ', 'No final report', 'warning'],
    ['Plan ready: ', 'Plan ready'],
  ];
  const reportStatus = (c) => {
    const title = String(c.title || ''),
      hit = c.kind === 'info' && REPORT_STATUS.find(([prefix]) => title.startsWith(prefix));
    return hit ? { label: hit[1], tone: hit[2], title: heading(c.body) || title.slice(hit[0].length) } : null;
  };
  const answerable = (c) => (c.kind !== 'ask' && c.kind !== 'plan') || c.answerable !== false,
    dshOf = (c) => (c.engine === 'dsh' && c.source?.uri?.startsWith('dsh://session/') ? c.source.uri.slice(14) : null),
    paneOf = (c) => c.source?.pane || (dshOf(c) ? `dsh:${dshOf(c)}` : null),
    paneApi = (pane) => (pane.startsWith('dsh:') ? `/v1/dsh/${enc(pane.slice(4))}` : `/v1/panes/${enc(pane)}`),
    restorable = (c) => c.kind === 'info' && /^(exited|reload:exited):/.test(c.id);
  const current = () => S.cards[S.idx] || null;
  const recIndex = (c) => {
    const i = (c.options || []).findIndex((o) => o.recommended);
    return i < 0 ? 0 : i;
  };
  function optionMode(option) {
    return option.label.trimStart().toLowerCase().startsWith('investigate') ? 'investigate' : 'act';
  }
  const byPriority = (a, b) =>
    Number(a.priority || 5) - Number(b.priority || 5) ||
    Number(a.created_ms || 0) - Number(b.created_ms || 0) ||
    (String(a.id) < String(b.id) ? -1 : String(a.id) > String(b.id) ? 1 : 0);
  let serial = 0;
  function h(tag, p = {}, ...kids) {
    if (typeof p === 'string') p = { class: p };
    if (!p['data-od-id']) p = { ...p, 'data-od-id': `triage-${++serial}` };
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(p)) {
      if (v == null || v === false) continue;
      if (k === 'class') e.className = v;
      else if (k.startsWith('on') && typeof v === 'function') e.addEventListener(k.slice(2), v);
      else if (k === 'value' && 'value' in e) e.value = v;
      else e.setAttribute(k, v === true ? '' : String(v));
    }
    e.append(...kids.flat(Infinity).filter((x) => x != null && x !== false && x !== ''));
    return e;
  }
  const el = (t, c, ...k) => h(t, c || {}, ...k),
    chip = (k) => el('span', `chip chip-${k}`, k);
  function draft(c) {
    if (!S.drafts.has(c.id))
      S.drafts.set(c.id, {
        note: '',
        other: '',
        open: false,
        dispatch: '',
        dispatchMode: 'investigate',
        dispatchOption: null,
        dispatchAnswers: null,
        step: 0,
        answers: [],
        executor: '',
        executorThinking: '',
      });
    return S.drafts.get(c.id);
  }
  function announce(t) {
    const e = $('#announcer');
    if (e) e.textContent = t;
  }
  // Toasts report an error or a launch still running on the daemon; announce() carries every
  // successful result. `keep` leaves the toast up until the caller removes it.
  function toast(t, error = false, keep = false) {
    const e = el('div', `toast${error ? ' error' : ''}`, error && el('strong', null, '! '), t);
    e.setAttribute('role', error ? 'alert' : 'status');
    $('#toasts').append(e);
    if (keep) return e;
    setTimeout(() => e.remove(), error ? 6000 : 2800);
  }
  function setConnection(state) {
    const changed = S.connection !== state;
    S.connection = state;
    const name = { connecting: 'Connecting', connected: 'Connected', reconnecting: 'Reconnecting' }[
      state
    ];
    $('#connection').dataset.state = state;
    $('#connection').setAttribute('aria-label', name);
    $('.connection-label').textContent = name;
    if (!changed) return;
    clearTimeout(S.offlineTimer);
    S.offlineTimer = 0;
    // A phone drops the stream on every trip to the background, so only a drop that outlasts the grace period takes over the screen.
    if (state === 'reconnecting')
      S.offlineTimer = setTimeout(() => {
        S.offlineTimer = 0;
        S.offline = true;
        if (S.initialized && !S.cards.length) render();
      }, 4000);
    else if (S.offline) {
      S.offline = false;
      if (S.initialized && !S.cards.length) render();
    }
  }
  const total = () =>
    ['ask', 'plan', 'finding', 'info'].reduce((n, k) => n + Math.max(0, Number(S.counts[k]) || 0), 0);
  function updateCounts() {
    const list = $('#counts'),
      toggle = $('#count-toggle');
    list.replaceChildren(
      ...['ask', 'plan', 'finding', 'info'].map((k) =>
        el('li', S.counts[k] ? null : 'zero', k, el('b', null, String(S.counts[k] || 0))),
      ),
    );
    list.classList.toggle('expanded', S.countsExpanded);
    toggle.setAttribute('aria-expanded', String(S.countsExpanded));
    toggle.setAttribute(
      'aria-label',
      `${S.countsExpanded ? 'Hide' : 'Show'} counts; ${total()} waiting`,
    );
    toggle.textContent = String(total());
  }
  function updateHeader() {
    updateCounts();
    setConnection(S.connection);
    document.title = S.cards.length ? `(${S.cards.length}) triage` : 'triage — decision inbox';
  }
  // Desktop scrolls inside .card-scroll; narrow screens scroll the page instead.
  function scroller() {
    const s = $('.card-scroll');
    return s && getComputedStyle(s).overflowY !== 'visible' ? s : document.scrollingElement;
  }
  function snapshot() {
    const d = S.shownId && S.drafts.get(S.shownId),
      note = $('#note'),
      other = $('#other-text'),
      active = document.activeElement,
      inside = Boolean($('.card')?.contains(active));
    if (d) {
      if (note) d.note = note.value;
      if (other) d.other = other.value;
    }
    const selection =
      inside && typeof active?.selectionStart === 'number'
        ? [active.selectionStart, active.selectionEnd, active.selectionDirection]
        : null;
    return {
      id: inside ? active.id || null : null,
      selection,
      scroll: scroller()?.scrollTop || 0,
      report: $('.report-wrap .body')?.scrollTop || 0,
      inside,
    };
  }
  function restore(v, changed) {
    const scroll = scroller(),
      body = $('.report-wrap .body');
    if (scroll) scroll.scrollTop = changed ? 0 : v.scroll || 0;
    if (body) body.scrollTop = changed ? 0 : v.report || 0;
    if (document.querySelector('dialog[open]')) return;
    if (changed) {
      focusPrimary();
      return;
    }
    if (v.id) {
      const e = document.getElementById(v.id);
      if (e && !e.disabled) {
        e.focus({ preventScroll: true });
        if (v.selection && e.setSelectionRange) e.setSelectionRange(...v.selection);
      } else if (v.inside) $('.card')?.focus({ preventScroll: true });
    }
  }

  // api
  async function api(path, body) {
    const init =
      body === undefined
        ? {}
        : {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body),
          };
    const res = await fetch(path, init),
      data = await res.json().catch(() => null);
    if (!res.ok) {
      const e = new Error(data?.error || data?.message || `${res.status} ${res.statusText}`);
      e.status = res.status;
      throw e;
    }
    return data;
  }
  function countsFrom(cards) {
    return cards.reduce(
      (a, c) => {
        a[c.kind] = (a[c.kind] || 0) + 1;
        return a;
      },
      { ask: 0, plan: 0, finding: 0, info: 0 },
    );
  }
  function applyQueue(q) {
    if (!Array.isArray(q?.cards)) throw new Error('Unexpected queue response');
    const view = snapshot(),
      version = S.version,
      empty = !S.cards.length,
      shownId = S.shownId,
      shown = S.cards.find((c) => c.id === shownId),
      incomingIds = new Set(q.cards.map((c) => c.id)),
      remoteRemoved = Boolean(shown && !incomingIds.has(shownId) && !S.pending),
      shownDraft = S.drafts.get(shownId);
    S.version = q.version ?? S.version;
    const oldTop = S.cards[0] || null;
    // A card whose action is in flight stays hidden until the daemon answers.
    S.cards = q.cards.filter((c) => !S.inflight.has(c.id)).sort(byPriority);
    // A card that outranks the deck leader takes the view to the top unless the shown card is active.
    const lead = S.cards[0] || null,
      takeTop = Boolean(
        oldTop &&
          lead &&
          byPriority(lead, oldTop) < 0 &&
          !S.pending &&
          !S.laterOpen &&
          !(shownDraft && (shownDraft.open || shownDraft.note.trim() || shownDraft.other.trim())),
      );
    S.counts = countsFrom(S.cards);
    for (const id of S.drafts.keys()) {
      if (!incomingIds.has(id)) S.drafts.delete(id);
    }
    const keep = S.cards.findIndex((c) => c.id === S.shownId);
    S.idx = keep >= 0 ? keep : Math.min(S.idx, Math.max(0, S.cards.length - 1));
    if (takeTop && S.idx > 0) {
      S.idx = 0;
      S.direction = -1;
    }
    S.initialized = true;
    if (!S.cards.length && (!empty || version !== S.version)) {
      S.history.loaded = false;
      S.history.error = null;
    }
    if (remoteRemoved) {
      const message = `Handled elsewhere · ${titleOf(shown)}`;
      S.remoteSwapUntil = Date.now() + 500;
      $('.card')?.classList.add('leaving-right');
      S.direction = 1;
      if (!window.matchMedia?.('(prefers-reduced-motion: reduce)').matches)
        setTimeout(() => render(view, message), 150);
      else render(view, message);
    } else if (!takeTop && shownId && keep >= 0) {
      // Card data never changes under an id, so the shown card keeps its DOM, focus and scroll.
      updateHeader();
      updateQueueLine();
    } else render(view);
    if (!S.cards.length) emptyHistory();
  }
  function updateQueueLine() {
    const line = $('.queue-line'),
      c = current();
    if (!line || !c) return;
    const focused = line.contains(document.activeElement) ? document.activeElement.id : null;
    line.replaceWith(queueLine(c));
    const target = focused && document.getElementById(focused);
    if (target && !target.disabled) target.focus({ preventScroll: true });
  }
  // A queue from a newer shell means the daemon serves code this page does not run.
  function reloadIfStale(q) {
    if (S.shell == null) {
      S.shell = q.shell ?? '';
      return false;
    }
    if (q.shell === S.shell || S.inflight.size || S.dictating || $('dialog[open]')) return false;
    const typed = [...S.drafts.values()].some((d) => d.note.trim() || d.other.trim() || d.dispatch.trim());
    if (typed || $('#compose-prompt')?.value.trim()) return false;
    location.reload();
    return true;
  }
  function scheduleRefreshRetry() {
    if (!S.streamOpen || S.refreshRetryTimer) return;
    const delay = S.refreshRetryDelay;
    S.refreshRetryDelay = Math.min(30000, delay * 2);
    S.refreshRetryTimer = setTimeout(() => {
      S.refreshRetryTimer = 0;
      if (S.streamOpen) void refresh();
    }, delay);
  }
  function refresh() {
    if (S.pending) {
      S.dirty = true;
      return Promise.resolve();
    }
    if (S.refreshPromise) {
      S.dirty = true;
      return S.refreshPromise;
    }
    if (S.refreshRetryTimer) {
      clearTimeout(S.refreshRetryTimer);
      S.refreshRetryTimer = 0;
    }
    S.refreshPromise = (async () => {
      do {
        S.dirty = false;
        if (S.pending) {
          S.dirty = true;
          break;
        }
        const revision = S.actionRevision;
        try {
          const q = await api('/v1/queue');
          if (S.pending || revision !== S.actionRevision) {
            S.dirty = true;
            break;
          }
          if (reloadIfStale(q)) return;
          applyQueue(q);
          S.refreshRetryDelay = 1000;
          if (S.streamOpen) setConnection('connected');
        } catch (e) {
          if (S.pending) {
            S.dirty = true;
            break;
          }
          setConnection('reconnecting');
          if (!S.dirty) scheduleRefreshRetry();
          if (S.initialized) {
            toast(`Queue refresh failed: ${e.message}`, true);
            announce(`Queue refresh failed: ${e.message}`);
          } else {
            render();
          }
        }
      } while (S.dirty && !S.pending);
    })().finally(() => {
      S.refreshPromise = null;
      if (S.dirty && !S.pending) void refresh();
    });
    return S.refreshPromise;
  }
  function reconnect() {
    if (S.reconnectTimer) return;
    setConnection('reconnecting');
    const wait = S.reconnectDelay;
    S.reconnectDelay = Math.min(30000, wait * 2);
    S.reconnectTimer = setTimeout(() => {
      S.reconnectTimer = 0;
      connectStream();
    }, wait);
  }
  function retryNow() {
    if (!S.reconnectTimer) return void refresh();
    clearTimeout(S.reconnectTimer);
    S.reconnectTimer = 0;
    S.reconnectDelay = 1000;
    connectStream();
  }
  function connectStream() {
    if (S.stream || S.reconnectTimer) return;
    if (S.connection !== 'reconnecting') setConnection('connecting');
    try {
      const stream = new EventSource('/v1/stream');
      S.stream = stream;
      stream.onopen = () => {
        if (S.stream !== stream) return;
        S.streamOpen = true;
        S.reconnectDelay = 1000;
        setConnection('connected');
        void refresh();
      };
      stream.addEventListener('queue', () => {
        void refresh();
      });
      stream.onerror = () => {
        if (S.stream !== stream) return;
        stream.close();
        S.stream = null;
        S.streamOpen = false;
        reconnect();
      };
    } catch {
      S.stream = null;
      S.streamOpen = false;
      reconnect();
    }
  }
  function historyDayStart() {
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    return today.getTime();
  }
  async function historyRows() {
    const todayStart = historyDayStart();
    const today = new Date(todayStart);
    if (!S.historyPromise || S.historyPromiseDate !== todayStart) {
      const tomorrow = new Date(today);
      tomorrow.setDate(tomorrow.getDate() + 1);
      const params = new URLSearchParams({
        limit: '50',
        today_start_ms: String(todayStart),
        today_end_ms: String(tomorrow.getTime()),
      });
      const promise = api(`/v1/history?${params}`)
        .then((q) => {
          if (!Array.isArray(q?.cards) || !Number.isInteger(q?.today_count))
            throw new Error('Unexpected history response');
          const currentDay = todayStart === historyDayStart();
          if (currentDay) {
            S.history.today = q.today_count;
            S.history.error = null;
          }
          if (!currentDay || S.historyPromise !== promise) return null;
          return q.cards;
        })
        .catch((e) => {
          if (S.historyPromise !== promise || todayStart !== historyDayStart()) return null;
          throw e;
        })
        .finally(() => {
          if (S.historyPromise === promise) {
            S.historyPromise = null;
            S.historyPromiseDate = null;
          }
        });
      S.historyPromiseDate = todayStart;
      S.historyPromise = promise;
    }
    return S.historyPromise;
  }
  function summarize(rows) {
    S.history.lastAt = null;
    for (const row of rows) {
      const at = stamp(row?.swipe?.at_ms);
      if (at == null) continue;
      if (S.history.lastAt == null || at > S.history.lastAt) S.history.lastAt = at;
    }
  }
  async function emptyHistory() {
    const todayStart = historyDayStart();
    if (S.history.dayStart !== todayStart) {
      S.history.dayStart = todayStart;
      S.history.loaded = false;
      S.history.error = null;
    }
    if (S.cards.length || S.history.loading || S.history.loaded) return;
    S.history.loading = true;
    S.history.error = null;
    render();
    try {
      const rows = await historyRows();
      if (rows && todayStart === historyDayStart()) {
        summarize(rows);
        S.history.loaded = true;
      }
    } catch (e) {
      if (todayStart === historyDayStart()) S.history.error = e.message;
    } finally {
      S.history.loading = false;
      if (!S.cards.length) render();
      if (!S.cards.length && todayStart !== historyDayStart()) emptyHistory();
    }
  }

  // render
  function focusPrimary() {
    ($('[data-primary]') || $('.card') || $('#main'))?.focus({ preventScroll: true });
  }
  function countToggle() {
    S.countsExpanded = !S.countsExpanded;
    updateCounts();
  }
  function closeCounts() {
    if (S.countsExpanded) {
      S.countsExpanded = false;
      updateCounts();
    }
  }
  function button(c, key, cls, attrs, fn, ...kids) {
    const busy = S.pending?.id === c.id,
      chosen = busy && S.pending.control === key;
    return h(
      'button',
      {
        'data-od-id': ident(`button-${key}`, c),
        type: 'button',
        ...attrs,
        class: `${cls}${chosen ? ' chosen' : ''}`,
        disabled: busy || attrs.disabled,
        'aria-busy': chosen ? 'true' : null,
        onclick: fn,
      },
      ...kids,
    );
  }
  function queueLine(c) {
    const pos = S.idx + 1,
      total = S.cards.length;
    return el(
      'div',
      'queue-line',
      h('span', { class: 'pos', 'aria-current': 'step' }, el('b', null, String(pos)), ` of ${total}`),
      h(
        'div',
        { class: 'rail', 'aria-hidden': 'true' },
        S.cards.map((x, i) =>
          h('span', { class: i === S.idx ? 'tick on' : 'tick', 'data-p': Number(x.priority) || 5 }),
        ),
      ),
      h(
        'nav',
        { class: 'queue-nav', 'aria-label': 'Move between cards' },
        button(
          c,
          'prev',
          'btn ghost prev-btn',
          { id: 'prev-btn', 'aria-label': 'Previous card', disabled: S.idx === 0 },
          () => move(-1),
          '‹',
        ),
        button(
          c,
          'next',
          'btn ghost next-btn',
          { id: 'next-btn', 'aria-label': 'Next card', disabled: S.idx >= total - 1 },
          () => move(1),
          '›',
        ),
      ),
    );
  }
  // tmux labels read "<session:window> <session title>". The pane target is shown on its own, so
  // the name keeps only the title. Its leading emoji goes too, because session titles already name
  // their category in words.
  function sourceName(s) {
    const label = String(s.label || ''),
      win = s.target ? String(s.target).replace(/\.[^.]*$/, '') : '',
      title = (win && label.startsWith(`${win} `) ? label.slice(win.length + 1) : label)
        .replace(/^(?:\p{Extended_Pictographic}|\uFE0F|\u200D)+\s*/u, '')
        .split(/\s*·\s*/)[0];
    return (
      title ||
      (s.cwd &&
        String(s.cwd)
          .replace(/[\\/]+$/, '')
          .split(/[\\/]/)
          .pop()) ||
      s.uri ||
      'unknown'
    );
  }
  function age(ms) {
    const sec = Math.max(0, (Date.now() - ms) / 1000);
    return sec < 60
      ? 'now'
      : sec < 3600
        ? `${Math.floor(sec / 60)}m`
        : sec < 86400
          ? `${Math.floor(sec / 3600)}h`
          : `${Math.floor(sec / 86400)}d`;
  }
  function facts(c) {
    const s = c.source || {},
      at = stamp(c.created_ms) ?? Date.now(),
      p = Number(c.priority) || 5,
      dir = s.cwd && String(s.cwd).replace(/^\/(?:home|Users)\/[^/]+(?=\/|$)/, '~'),
      fact = (name, ...value) =>
        el('div', `fact fact-${name.toLowerCase()}`, el('dt', null, name), el('dd', null, ...value));
    return el(
      'dl',
      'facts',
      fact('From', h('span', { title: s.label || null }, sourceName(s))),
      dir && fact('Directory', h('code', { title: s.cwd }, dir)),
      s.target && fact('Pane', h('code', { title: s.pane ? `Pane ${s.pane}` : null }, s.target)),
      fact('Kind', chip(c.kind), c.level === 'error' && chip('error')),
      fact('Priority', h('span', { title: 'Priority, 1 is most urgent' }, `P${p}`)),
      fact(
        'Waiting',
        h(
          'time',
          {
            datetime: new Date(at).toISOString(),
            title: new Date(at).toLocaleString(),
            'data-age': at,
          },
          age(at),
        ),
      ),
    );
  }
  const prose = (v) => (v ? el('div', 'md', md(v)) : null);
  // Card text is agent-written Markdown. marked only tokenizes it and mdNodes builds the elements, so
  // raw HTML shows as literal text and only http, https and mailto links become anchors. Titles and
  // option text render inline only and never link, because they sit inside headings and buttons.
  function md(text) {
    const src = String(text ?? '');
    // A JSON body, such as an Apply Watch alert, shows as one code block so its indentation survives.
    let json = /^\s*[[{]/.test(src);
    if (json)
      try {
        JSON.parse(src);
      } catch {
        json = false;
      }
    return mdNodes(
      json ? [{ type: 'code', text: src.trim() }] : marked.lexer(src, { gfm: true, breaks: true }),
      true,
    );
  }
  function mdInline(text) {
    return mdNodes(marked.Lexer.lexInline(String(text ?? ''), { gfm: true }), false);
  }
  function mdNodes(tokens, links) {
    const kids = (t) => mdNodes(t.tokens || [], links),
      cell = (tag) => (c) =>
        h(tag, { style: c.align ? `text-align: ${c.align}` : null }, mdNodes(c.tokens, links));
    return tokens.map((t) => {
      switch (t.type) {
        case 'paragraph':
          return el('p', null, kids(t));
        case 'heading':
          return el('h4', `md-h${Math.min(t.depth, 3)}`, kids(t));
        case 'text':
          return t.tokens ? kids(t) : t.text;
        case 'escape':
          return t.text;
        case 'strong':
        case 'em':
        case 'del':
          return el(t.type, null, kids(t));
        case 'codespan':
          return el('code', null, t.text);
        case 'br':
        case 'hr':
          return el(t.type);
        case 'code':
          return el('pre', null, el('code', null, t.text));
        case 'blockquote':
          return el('blockquote', null, kids(t));
        case 'list':
          return h(
            t.ordered ? 'ol' : 'ul',
            { start: t.ordered && t.start !== 1 ? t.start : null },
            t.items.map((item) => el('li', null, mdNodes(item.tokens, links))),
          );
        case 'table':
          return el(
            'div',
            'md-table',
            el(
              'table',
              null,
              el('thead', null, el('tr', null, t.header.map(cell('th')))),
              el('tbody', null, t.rows.map((row) => el('tr', null, row.map(cell('td'))))),
            ),
          );
        case 'link':
        case 'image': {
          // An image shows as a link to it; the inbox never loads remote images.
          const label = kids(t),
            href = links && /^(https?|mailto):/i.test(t.href) ? t.href : null;
          return href
            ? h('a', { href, title: t.title || null, target: '_blank', rel: 'noreferrer' }, label.length ? label : href)
            : label;
        }
        case 'html':
          return t.block ? el('pre', null, el('code', null, t.text)) : t.text;
        case 'space':
        case 'def':
          return null;
        default:
          return t.raw;
      }
    });
  }
  // Clamp Situation, Task, Plan and context-thread Report prose to keep the rest in reach.
  const clampable = (label) =>
    label === 'Situation' || label === 'Task' || label === 'Plan' || label === 'Report';
  function block(c, label, ...kids) {
    const content = kids.filter(Boolean);
    if (!content.length) return null;
    const id = ident(label.toLowerCase(), c),
      key = `${c.id}:${label}`,
      clamp = clampable(label),
      open = S.openBlocks.has(key);
    return h(
      'section',
      {
        class: `block${label === 'Action' ? ' action-block' : ''}${clamp ? ' clamp' : ''}${open ? ' open' : ''}`,
        'data-od-id': ident(`block-${label.toLowerCase()}`, c),
        'aria-labelledby': id,
      },
      h('h3', { id }, label),
      h('div', { class: 'block-content', id: `${id}-content` }, ...content),
      clamp &&
        h(
          'button',
          {
            type: 'button',
            class: 'btn ghost more',
            'data-od-id': ident(`more-${label.toLowerCase()}`, c),
            'aria-expanded': String(open),
            'aria-controls': `${id}-content`,
            hidden: true,
            onclick: (e) => toggleBlock(key, e.currentTarget.closest('.block')),
          },
          open ? 'Show less' : 'Show more',
        ),
    );
  }
  // Titles clamp to two lines so the dock and options stay in reach; Show more reveals the rest.
  function titleRow(c) {
    const key = `${c.id}:Title`,
      open = S.openBlocks.has(key),
      status = reportStatus(c);
    return [
      status &&
        h(
          'p',
          { class: 'report-status', 'data-tone': status.tone, 'data-od-id': ident('report-status', c) },
          status.label,
        ),
      h(
        'h2',
        { id: 'card-title', class: `title${open ? ' open' : ''}`, 'data-od-id': ident('title', c) },
        mdInline(c.questions?.length && answerable(c) ? stepTitle(c) : (status?.title ?? titleOf(c))),
      ),
      h(
        'button',
        {
          type: 'button',
          class: 'btn ghost more title-more',
          'data-od-id': ident('title-more', c),
          'aria-expanded': String(open),
          'aria-controls': 'card-title',
          hidden: true,
          onclick: (e) => toggleTitle(key, e.currentTarget.closest('.card-head')),
        },
        open ? 'Show less' : 'Show more',
      ),
    ];
  }
  function toggleTitle(key, head) {
    const open = !S.openBlocks.has(key);
    if (open) S.openBlocks.add(key);
    else S.openBlocks.delete(key);
    const title = $('h2.title', head);
    title.classList.toggle('open', open);
    const more = $('.title-more', head);
    more.setAttribute('aria-expanded', String(open));
    more.textContent = open ? 'Show less' : 'Show more';
    updateClamps();
  }
  function toggleBlock(key, section) {
    const open = !S.openBlocks.has(key);
    if (open) S.openBlocks.add(key);
    else S.openBlocks.delete(key);
    section.classList.toggle('open', open);
    const more = $('.more', section);
    more.setAttribute('aria-expanded', String(open));
    more.textContent = open ? 'Show less' : 'Show more';
    updateClamps();
  }
  function updateClamps() {
    document.querySelectorAll('.block.clamp').forEach((section) => {
      const p = $('.block-content > .md', section);
      $('.more', section).hidden = !(
        section.classList.contains('open')
          ? matchMedia('(max-width: 639px)').matches || section.closest('#context') != null
          : p && p.scrollHeight > p.clientHeight + 1
      );
    });
    const title = $('.card-head h2.title'),
      more = $('.card-head .title-more');
    if (title && more)
      more.hidden = title.classList.contains('open')
        ? false
        : !(title.scrollHeight > title.clientHeight + 1);
  }
  async function copyReport(c) {
    announce('Copying reply…');
    try {
      if (!navigator.clipboard?.writeText) throw new Error('Clipboard access is unavailable.');
      await navigator.clipboard.writeText(String(c.body || ''));
      announce('Reply copied.');
    } catch (e) {
      toast(`Copy failed: ${e.message}`, true);
      announce(`Copy failed: ${e.message}`);
    }
  }
  function report(c) {
    return el(
      'div',
      { class: 'report-wrap', 'data-od-id': ident('reply-wrap', c) },
      el(
        'div',
        'report-toolbar',
        button(c, 'copy', 'btn ghost', {}, () => copyReport(c), 'Copy'),
      ),
      h(
        'div',
        {
          class: 'body md',
          id: ident('reply', c),
          'data-od-id': ident('reply-body', c),
          tabindex: '0',
          'aria-label': 'Final reply',
        },
        md(c.body),
      ),
      h('p', { class: 'report-fade', hidden: true, 'aria-hidden': 'true' }, 'More below'),
    );
  }
  function updateReport(w) {
    if (!w) return;
    const b = $('.body', w),
      clipped = Boolean(b && b.scrollHeight - b.scrollTop > b.clientHeight + 1),
      cue = $('.report-fade', w);
    w.classList.toggle('clipped', clipped);
    if (cue) {
      cue.hidden = !clipped;
      cue.setAttribute('aria-hidden', String(!clipped));
    }
  }
  const observer =
    typeof ResizeObserver === 'undefined'
      ? null
      : new ResizeObserver((es) =>
          es.forEach((e) => updateReport(e.target.closest('.report-wrap'))),
        );
  function observeReports() {
    observer?.disconnect();
    document.querySelectorAll('.report-wrap').forEach((w) => {
      const b = $('.body', w);
      if (b) {
        observer?.observe(b);
        b.addEventListener('scroll', () => updateReport(w), { passive: true });
      }
      updateReport(w);
    });
  }
  function paneButtons(c, primary = false) {
    return el(
      'div',
      'pane-actions',
      button(
        c,
        'peek',
        `btn${primary ? ' primary' : ''}`,
        {
          id: primary ? 'peek-main' : 'peek-btn',
          'data-primary': primary ? 'true' : null,
          'aria-keyshortcuts': 'v',
        },
        () => openPeek(c),
        'Peek',
        h('kbd', { class: 'hint', 'aria-hidden': 'true' }, 'v'),
      ),
      dshOf(c)
        ? button(c, 'open', 'btn', { id: 'open-btn' }, openDsh, 'Open')
        : button(
            c,
            'jump',
            'btn',
            { id: 'jump-btn', 'aria-keyshortcuts': 'Shift+J' },
            () => jump(c),
            'Jump',
            h('kbd', { class: 'hint', 'aria-hidden': 'true' }, 'J'),
          ),
    );
  }
  async function openDsh() {
    try {
      await api('/v1/dsh/open', {});
      toast('Opened dsh on the desktop.');
    } catch (e) {
      toast(`Open failed: ${e.message}`, true);
    }
  }
  function planExecutorView(c) {
    const engine = c.engine || 'omp',
      pair = (id, label, value) => {
        const field = h('button', {
          type: 'button',
          class: 'field picker-field',
          id,
          value,
          'aria-labelledby': `${id}-label ${id}`,
          'aria-haspopup': 'dialog',
          'aria-expanded': 'false',
          'aria-controls': 'picker',
          'data-od-id': ident(id, c),
        });
        setPickerValue(id, value, field);
        bindPicker(id, field);
        return el('div', 'pair', h('label', { id: `${id}-label`, for: id }, label), field);
      };
    planExecutorLabels(engine);
    // An omp Plan Review slider lists roles only, so its role keeps its own thinking level.
    return el(
      'div',
      'plan-executor',
      pair('plan-executor', 'Execution model', draft(c).executor),
      engine === 'dsh' && pair('plan-executor-effort', 'Execution effort', draft(c).executorThinking),
    );
  }
  // Roles loaded for another harness's plan card are dropped until loadModels refills the list.
  function planExecutorLabels(engine) {
    const cfg = PICKERS['plan-executor'];
    if (cfg.engine !== engine) cfg.options = [];
    cfg.engine = engine;
    cfg.placeholder = engine === 'dsh' ? 'Search models' : 'Search roles';
    cfg.fallback = engine === 'dsh' ? 'current model' : 'current Plan Review role';
  }

  function otherAllowed(c) {
    const questions = c.questions?.length ? c.questions : null,
      question = questions ? questions[draft(c).step] : null;
    // A plain finding takes no custom answer, and past the last question the review step takes over.
    if (c.kind === 'finding' && !questions) return false;
    if (questions && !question) return false;
    return Boolean(c.allow_other && question?.allow_other !== false && (c.kind === 'ask' || c.kind === 'plan' || c.kind === 'finding'));
  }
  function optionsView(c) {
    if (c.kind === 'info') return null;
    const live = answerable(c),
      step = live && c.questions?.length ? draft(c).step : null;
    if (step != null) {
      if (step >= c.questions.length) return reviewView(c);
      return questionView(c, step);
    }
    const opts = (c.options || [])
      .map((option, i) => ({ option, i }))
      .sort(
        (a, b) =>
          Number(Boolean(b.option.recommended)) - Number(Boolean(a.option.recommended)) ||
          a.i - b.i,
      );
    const rows = opts.map(({ option: o, i }) => {
      const label = c.kind === 'finding' ? `Do: ${o.label}` : o.label,
        copy = el(
          'span',
          'opt-copy',
          el('span', 'opt-label', mdInline(label)),
          o.description && el('span', 'opt-desc', mdInline(o.description)),
          o.direction && el('span', 'opt-dir', mdInline(o.direction)),
        );
      const kids = [
          h('kbd', { class: 'num', 'aria-hidden': 'true' }, i < 9 ? String(i + 1) : '·'),
          copy,
          o.recommended && el('span', 'tag', 'Recommended'),
        ],
        cls = `option${o.recommended || i === recIndex(c) ? ' rec' : ''}`;
      return h(
        'li',
        { 'data-od-id': ident(`option-${i}`, c) },
        live
          ? button(
              c,
              `option-${i}`,
              cls,
              { id: `opt-${i}`, 'data-primary': i === recIndex(c) ? 'true' : null },
              () => pick(c, i),
              ...kids,
            )
          : el('div', `${cls} readonly`, ...kids),
      );
    });
    if (live && otherAllowed(c)) rows.push(otherRow(c));
    const out = [
      c.kind === 'plan' && live && planExecutorView(c),
      h('ol', { class: 'options', 'aria-label': live ? 'Options' : 'Options, read only' }, ...rows),
    ];
    if (!live)
      out.push(
        el(
          'div',
          'notice',
          el(
            'p',
            null,
            el('strong', null, c.engine === 'dsh' ? 'Answer in the dsh web UI. ' : 'Answer in the pane. '),
            'This prompt is read only here.',
          ),
          paneOf(c) && paneButtons(c, true),
        ),
      );
    return out;
  }
  // A multi-question ask is answered one question per step, then confirmed on a review step.
  function questionView(c, step) {
    const question = c.questions[step],
      found = question.options.findIndex((option) => option.recommended),
      rec = found < 0 ? 0 : found,
      rows = (question.options || [])
        .map((option, i) => ({ option, i }))
        .sort(
          (a, b) =>
            Number(Boolean(b.option.recommended)) - Number(Boolean(a.option.recommended)) ||
            a.i - b.i,
        )
        .map(({ option: o, i }) => {
          const kids = [
            h('kbd', { class: 'num', 'aria-hidden': 'true' }, i < 9 ? String(i + 1) : '·'),
            el(
              'span',
              'opt-copy',
              el('span', 'opt-label', mdInline(o.label)),
              o.description && el('span', 'opt-desc', mdInline(o.description)),
            ),
            o.recommended && el('span', 'tag', 'Recommended'),
          ];
          return h(
            'li',
            { 'data-od-id': ident(`option-${i}`, c) },
            button(
              c,
              `option-${i}`,
              `option${o.recommended || i === rec ? ' rec' : ''}`,
              { id: `opt-${i}`, 'data-primary': i === rec ? 'true' : null },
              () => pickStep(c, i),
              ...kids,
            ),
          );
        });
    if (otherAllowed(c)) rows.push(otherRow(c));
    return [
      el('p', 'question-step', `Question ${step + 1} of ${c.questions.length}`),
      h('ol', { class: 'options', 'aria-label': 'Options' }, ...rows),
    ];
  }
  function reviewView(c) {
    const d = draft(c),
      answered = c.questions.every((_, i) => d.answers[i] != null),
      rows = c.questions.map((question, k) => {
        const answer = d.answers[k],
          label =
            answer == null
              ? 'Unanswered'
              : answer.text != null
                ? `“${answer.text}”`
                : mdInline(question.options[answer.option]?.label || 'Unanswered');
        return h(
          'li',
          { 'data-od-id': ident(`answer-${k}`, c) },
          button(
            c,
            `answer-${k}`,
            'option',
            { id: `answer-${k}` },
            () => goStep(c, k),
            h('kbd', { class: 'num', 'aria-hidden': 'true' }, String(k + 1)),
            el(
              'span',
              'opt-copy',
              el('span', 'opt-label', label),
              el('span', 'opt-desc', mdInline(question.star ? question.label : question.question)),
            ),
            el('span', 'tag', 'Change'),
          ),
        );
      });
    return [
      h('ol', { class: 'options review', 'aria-label': 'Answers' }, ...rows),
      el(
        'div',
        'pane-actions',
        button(
          c,
          'send-answers',
          'btn primary',
          { id: 'send-answers', 'data-primary': 'true', disabled: !answered },
          () => sendAnswers(c),
          c.kind === 'finding' ? 'Dispatch answers' : 'Send answers',
          h('kbd', { class: 'hint', 'aria-hidden': 'true' }, '⏎'),
        ),
      ),
    ];
  }
  function stepTitle(c) {
    const question = c.questions[draft(c).step];
    if (!question) return 'Review answers';
    // A question with its own STAR shows its text in the blocks, so its label is the title.
    return question.star ? question.label : question.question;
  }
  function pickStep(c, i) {
    if (S.pending) return;
    const d = draft(c),
      question = c.questions[d.step];
    if (!question) {
      if (i < c.questions.length) goStep(c, i);
      return;
    }
    const options = question.options || [];
    if (options[i] != null) return answerStep(c, { option: i });
    // The Other row is the next button after the last option, so its digit opens it too.
    if (i === options.length && otherAllowed(c)) toggleOther(c);
  }
  function answerStep(c, answer) {
    const d = draft(c),
      questions = c.questions;
    d.answers[d.step] = answer;
    const next = questions.findIndex((_, i) => i > d.step && d.answers[i] == null);
    goStep(c, next < 0 ? questions.length : next);
  }
  function goStep(c, step) {
    const v = snapshot(),
      d = draft(c);
    d.step = step;
    d.open = false;
    d.other = d.answers[step]?.text || '';
    // A question with its own STAR brings new text, so it opens at the top.
    render(c.questions[step]?.star ? { ...v, scroll: 0 } : v);
    focusPrimary();
    announce(
      step < c.questions.length
        ? `Question ${step + 1} of ${c.questions.length}`
        : 'Review answers',
    );
  }
  function sendAnswers(c) {
    const d = draft(c);
    if (c.questions.some((_, i) => d.answers[i] == null)) return;
    if (c.kind === 'finding') openDispatch(c, 'act', true, null, d.answers);
    else act(c, 'answer', { answers: d.answers, note: noteValue(c) }, 'send');
  }
  function primaryStep(c) {
    const d = draft(c);
    if (d.step >= c.questions.length) {
      sendAnswers(c);
      return;
    }
    const found = c.questions[d.step].options.findIndex((option) => option.recommended);
    pickStep(c, found < 0 ? 0 : found);
  }
  function otherRow(c) {
    const d = draft(c),
      busy = S.pending?.id === c.id;
    if (!d.open)
      return h(
        'li',
        { 'data-od-id': ident('other', c) },
        button(
          c,
          'other',
          'option',
          { id: 'other-btn' },
          () => toggleOther(c),
          h('kbd', { class: 'num', 'aria-hidden': 'true' }, 'o'),
          el(
            'span',
            'opt-copy',
            el('span', 'opt-label', 'Other…'),
            el('span', 'opt-desc', 'Write a custom answer'),
          ),
        ),
      );
    return el(
      'li',
      'other-expanded',
      h(
        'form',
        {
          class: 'other-form',
          onsubmit: (e) => {
            e.preventDefault();
            sendOther(c);
          },
        },
        h('label', { for: 'other-text', class: 'sr-only' }, 'Your answer'),
        h('input', {
          id: 'other-text',
          class: 'field',
          type: 'text',
          autocomplete: 'off',
          enterkeyhint: 'send',
          placeholder: 'Your answer',
          value: d.other,
          disabled: busy,
          oninput: (e) => {
            draft(c).other = e.target.value;
          },
        }),
        micButton('other-text', 'Dictate your answer'),
        button(c, 'other', 'btn secondary', { type: 'submit' }, null, 'Submit'),
      ),
    );
  }
  function noteRow(c) {
    const d = draft(c),
      busy = S.pending?.id === c.id;
    return el(
      'div',
      'note',
      h('label', { for: 'note' }, 'Note', h('kbd', { class: 'hint', 'aria-hidden': 'true' }, 'n')),
      el(
        'div',
        'field-row',
        h('input', {
          id: 'note',
          class: 'field',
          type: 'text',
          autocomplete: 'off',
          enterkeyhint: 'done',
          placeholder: 'Optional note for this action',
          value: d.note,
          disabled: busy,
          oninput: (e) => {
            draft(c).note = e.target.value;
          },
        }),
        micButton('note', 'Dictate the note'),
      ),
    );
  }
  function laterChoices() {
    const now = Date.now(),
      tomorrow = new Date(now);
    tomorrow.setDate(tomorrow.getDate() + 1);
    tomorrow.setHours(9, 0, 0, 0);
    return [
      { label: '1 hour', minutes: 60, at: now + 3600000 },
      { label: '4 hours', minutes: 240, at: now + 14400000 },
      {
        label: 'Tomorrow',
        minutes: Math.max(1, Math.ceil((tomorrow - now) / 60000)),
        at: tomorrow.getTime(),
      },
    ].map((x) => ({ ...x, local: local(x.at) }));
  }
  function dock(c, withPane) {
    const key = (k) => h('kbd', { class: 'hint', 'aria-hidden': 'true' }, k),
      recommended = c.kind === 'info' && Boolean(c.star?.action),
      menu =
        S.laterOpen &&
        h(
          'div',
          {
            id: 'later-menu',
            class: 'menu',
            role: 'menu',
            'aria-label': 'Later options',
            'data-od-id': ident('later-menu', c),
          },
          ...laterChoices().map((x, i) =>
            button(
              c,
              `later-${i}`,
              'btn',
              { id: `later-${i}`, role: 'menuitem' },
              () => later(c, x),
              h('span', { class: 'later-label' }, x.label),
              h('span', { class: 'later-time' }, x.local),
              key(String(i + 1)),
            ),
          ),
        );
    return el(
      'footer',
      'dock',
      c.kind === 'info' &&
        (recommended
          ? button(
              c,
              'plan',
              'btn primary',
              { 'data-primary': 'true', 'aria-keyshortcuts': 'a Enter' },
              () => planRecommended(c),
              'Plan recommended step',
              key('a'),
            )
          : paneOf(c)
            ? button(
                c,
                'close',
                'btn primary',
                { 'data-primary': 'true', 'aria-keyshortcuts': 'a Enter x' },
                () => closePane(c),
                'Acknowledge & close',
                key('a'),
              )
            : button(
                c,
                'ack',
                'btn primary',
                { 'data-primary': 'true', 'aria-keyshortcuts': 'a Enter' },
                () => acknowledge(c),
                'Acknowledge',
                key('a'),
              )),
      el(
        'div',
        'dock-group',
        button(
          c,
          'dispatch',
          'btn',
          { 'aria-keyshortcuts': 'i Shift+A p Shift+P' },
          () => openDispatch(c),
          'Dispatch',
          key('i'),
        ),
        paneOf(c) &&
          (c.kind !== 'info' || recommended) &&
          button(
            c,
            'close',
            'btn',
            { 'aria-keyshortcuts': 'x' },
            () => closePane(c),
            'Acknowledge & close',
            key('x'),
          ),
        c.kind === 'info' &&
          c.report_due &&
          paneOf(c) &&
          button(c, 'request', 'btn', { 'aria-keyshortcuts': 'r' }, () => requestReport(c), 'Request final report', key('r')),
        restorable(c) &&
          button(c, 'restore', 'btn', { 'aria-keyshortcuts': 'R' }, () => restoreRun(c), 'Restore run', key('R')),
        (recommended ? !paneOf(c) : paneOf(c)) &&
          button(c, 'ack', 'btn', {}, () => acknowledge(c), 'Acknowledge'),
        button(c, 'skip', 'btn', { 'aria-keyshortcuts': 's' }, () => skip(c), 'Skip', key('s')),
        button(
          c,
          'context',
          'btn',
          { 'aria-keyshortcuts': 't' },
          () => openContext(c.id),
          'Context',
          key('t'),
        ),
        el(
          'div',
          'later',
          button(
            c,
            'later',
            'btn',
            {
              id: 'later-btn',
              'aria-keyshortcuts': 'l',
              'aria-expanded': String(S.laterOpen),
              'aria-controls': S.laterOpen ? 'later-menu' : null,
              'aria-haspopup': 'menu',
            },
            toggleLater,
            'Later',
            key('l'),
          ),
          menu,
        ),
        withPane && paneButtons(c),
      ),
    );
  }
  function cardView(c, changed) {
    const busy = S.pending?.id === c.id,
      live = answerable(c),
      pane = paneOf(c),
      // A question that carries its own STAR shows it in place of the card's, which covers every
      // finding and returns on the review step.
      star = (c.questions?.length && c.questions[draft(c).step]?.star) || c.star || {},
      cls = ['card'];
    if (changed) cls.push('enter', ...(S.direction < 0 ? ['back'] : []));
    if (busy) cls.push('pending');
    if (S.shakeId === c.id) cls.push('shake');
    const result = [prose(star.result)];
    if (c.kind === 'info' && c.body != null) result.push(report(c));
    // Only plans carry their own text as the body. An info body is the final reply under Result.
    // An ask's STAR task repeats its question, which the title already shows.
    const evidence = [
        block(c, 'Situation', prose(star.situation)),
        c.kind === 'plan' && block(c, 'Plan', prose(c.body)),
        star.task !== c.title && block(c, 'Task', prose(star.task)),
        block(c, 'Result', ...result),
      ].filter(Boolean),
      action = block(
        c,
        'Action',
        prose(star.action),
        optionsView(c),
        (live || c.kind === 'info') && noteRow(c),
      ),
      split = c.kind !== 'info' && evidence.length > 0;
    return h(
      'article',
      {
        id: ident('card', c),
        'data-od-id': ident('card', c),
        class: cls.join(' '),
        'data-kind': c.kind,
        'data-level': c.level || 'normal',
        'aria-labelledby': 'card-title',
        'aria-busy': busy ? 'true' : null,
        tabindex: '-1',
      },
      el('header', 'card-head', queueLine(c), ...titleRow(c), facts(c)),
      el(
        'div',
        split ? 'card-scroll split' : 'card-scroll',
        split ? el('div', 'context', evidence) : evidence,
        action,
      ),
      dock(c, Boolean(pane && live)),
    );
  }
  function emptyView() {
    if (S.connection === 'reconnecting' && (S.offline || !S.initialized))
      return h(
        'section',
        { class: 'empty offline', 'aria-labelledby': 'empty-title' },
        h('h2', { id: 'empty-title' }, 'Daemon unreachable'),
        h('p', { class: 'empty-lead', role: 'status' }, 'Retrying automatically.'),
        h('button', { type: 'button', class: 'btn', 'data-retry': true }, 'Retry now'),
      );
    const kids = [
      h('h2', { id: 'empty-title' }, 'Inbox zero'),
      el('p', 'empty-lead', 'Nothing is waiting on you.'),
    ];
    if (S.history.loading) kids.push(el('p', null, 'Loading recent history…'));
    else if (S.history.error)
      kids.push(h('p', { role: 'status' }, `History unavailable: ${S.history.error}`));
    else
      kids.push(
        el('p', null, `Handled today: ${S.history.today}`),
        el(
          'p',
          null,
          'Last handled: ',
          S.history.lastAt == null
            ? 'No recent history.'
            : h(
                'time',
                {
                  datetime: new Date(S.history.lastAt).toISOString(),
                  title: new Date(S.history.lastAt).toLocaleString(),
                },
                clock(S.history.lastAt),
              ),
        ),
      );
    kids.push(
      h(
        'ul',
        { class: 'empty-kinds', 'aria-label': 'Card types' },
        el('li', null, chip('ask'), 'Choose an answer'),
        el('li', null, chip('plan'), 'Review a plan'),
        el('li', null, chip('finding'), 'Review suggested work'),
        el('li', null, chip('info'), 'Read a result'),
      ),
    );
    return h('section', { class: 'empty', 'aria-labelledby': 'empty-title' }, kids);
  }
  function render(view = snapshot(), result = null) {
    const c = current(),
      changed = (c?.id || null) !== S.shownId;
    if (changed && S.picker?.id === 'plan-executor') $('#picker').close();
    if (changed) S.laterOpen = false;
    updateHeader();
    // The first paint shows the card in place. Only a card that replaces another view slides in.
    if (c) $('#main').replaceChildren(cardView(c, changed && S.shownId !== undefined));
    else $('#main').replaceChildren(emptyView());
    S.shownId = c?.id || null;
    observeReports();
    updateClamps();
    restore(view, changed);
    if (changed) {
      const next = c
        ? `${c.kind}, ${S.idx + 1} of ${S.cards.length}, ${titleOf(c)}`
        : !S.initialized && S.connection === 'reconnecting'
          ? 'Daemon unreachable. Reconnecting to daemon.'
          : 'Inbox zero. Nothing is waiting on you.';
      announce(result ? `${result}. ${next}` : next);
    }
  }

  // actions
  function move(n) {
    if (S.pending || !S.cards.length) return;
    const i = Math.max(0, Math.min(S.cards.length - 1, S.idx + n));
    if (i === S.idx) return;
    S.direction = n;
    S.idx = i;
    render();
  }
  function begin(c, control) {
    const v = snapshot();
    S.pending = { id: c.id, control };
    S.laterOpen = false;
    render(v);
    return v;
  }
  function failed(c, e, v) {
    S.pending = null;
    S.shakeId = c.id;
    render(v);
    toast(e.message, true);

    announce(`Error: ${e.message}`);
    clearTimeout(S.shakeTimer);
    S.shakeTimer = setTimeout(() => {
      S.shakeId = null;
      $('.card')?.classList.remove('shake');
    }, 180);
    if (S.dirty) void refresh();
  }
  function resultMessage(c, verb, body, choice) {
    if (verb === 'skip') return `Skipped · ${c.title}`;
    if (verb === 'dispatch' || (verb === 'answer' && body.answers && c.kind === 'finding'))
      return `${body.mode === 'act' ? 'Acting' : body.mode === 'plan' || body.mode === 'plan-into' ? 'Planning' : 'Investigating'} · ${c.title}`;
    if (verb === 'close') return `Closed pane · ${c.title}`;
    if (verb === 'report') return `Requested report · ${c.title}`;
    if (verb === 'restore') return `Restoring run · ${c.title}`;
    if (c.kind === 'info') return `Acknowledged · ${c.title}`;
    if (body.answers)
      return `Sent ${body.answers
        .map((answer, i) => answer.text || c.questions?.[i]?.options?.[answer.option]?.label || 'answer')
        .join(', ')} · ${c.title}`;
    return `${c.kind === 'finding' ? 'Launched' : 'Sent'} ${body.text || c.options?.[body.option]?.label || 'Custom answer'} · ${c.title}`;
  }
  async function act(c, verb, body, control, choice = null) {
    if (Date.now() < S.remoteSwapUntil || S.pending || !S.cards.some((x) => x.id === c.id)) return;
    S.pending = { id: c.id, control };
    S.laterOpen = false;
    S.inflight.add(c.id);
    // The card leaves now; settle reports the daemon's result, which can take seconds of tmux typing.
    const request = api(`/v1/cards/${enc(c.id)}/${verb}`, body);
    request.catch(() => {});
    const dir = verb === 'skip' ? 'left' : verb === 'later' ? 'down' : 'right',
      text = resultMessage(c, verb, body, choice);
    $('.card')?.classList.add(`leaving-${dir}`);
    if (!window.matchMedia?.('(prefers-reduced-motion: reduce)').matches)
      await new Promise((r) => setTimeout(r, 180));
    const after = snapshot();
    S.pending = null;
    const i = S.cards.findIndex((x) => x.id === c.id);
    if (i >= 0) S.cards.splice(i, 1);
    S.counts = { ...S.counts, [c.kind]: Math.max(0, (Number(S.counts[c.kind]) || 1) - 1) };
    // After an answer, show the highest-ranked waiting card.
    S.idx = i > 0 ? 0 : Math.min(S.idx, Math.max(0, S.cards.length - 1));
    S.direction = i > 0 ? -1 : 1;
    if (!S.cards.length) {
      S.history.loaded = false;
      S.history.error = null;
    }
    render(after, text);
    if (S.dirty) void refresh();
    void settle(c, request);
    return true;
  }
  async function settle(c, request) {
    try {
      await request;
      S.drafts.delete(c.id);
    } catch (e) {
      const conflict = e.status === 409 || e.status === 404,
        message = conflict ? `Already handled elsewhere · ${titleOf(c)}` : `${e.message} · ${titleOf(c)}`;
      if (conflict) S.drafts.delete(c.id);
      if (!conflict) toast(message, true);
      announce(conflict ? message : `Error: ${message}`);
    } finally {
      S.inflight.delete(c.id);
      S.actionRevision++;
      void refresh();
    }
  }
  const noteValue = (c) => draft(c).note.trim() || null;
  function pick(c, i) {
    if (Date.now() < S.remoteSwapUntil) return;
    if (c.questions?.length && answerable(c)) return pickStep(c, i);
    const options = c.options || [];
    // The Other row follows the last option; its digit opens it for an ask or plan card.
    if (i === options.length && otherAllowed(c) && answerable(c)) return toggleOther(c);
    const option = options[i];
    if (!option || !answerable(c) || c.kind === 'info') return;
    if (c.kind === 'finding') return openDispatch(c, optionMode(option), true, i);
    const needsOther = c.kind === 'plan' && option.label === 'Refine',
      text = needsOther ? draft(c).other.trim() : null;
    if (needsOther && !text) {
      if (!draft(c).open) toggleOther(c);
      else $('#other-text')?.focus({ preventScroll: true });
      const message = 'Add refinement text in Other before choosing Refine.';
      toast(message, true);
      announce(`Error: ${message}`);
      return;
    }
    const body = { option: i, text, note: noteValue(c) };
    if (c.kind === 'plan' && option.label === 'Approve') {
      body.executor = draft(c).executor || null;
      body.executor_thinking = draft(c).executorThinking || null;
    }
    act(c, 'answer', body, `option-${i}`);
  }
  function toggleOther(c) {
    // The row is only rendered when Other is offered, so a click always opens it. An action in
    // flight or a card swap must not swallow the click and leave the button looking dead.
    if (!otherAllowed(c)) return;
    const d = draft(c);
    d.open = !d.open;
    render();
    if (!d.open) {
      $('#other-btn')?.focus({ preventScroll: true });
      return;
    }
    const field = $('#other-text');
    // The field can open below the fold on a phone or a short window. Bring it into view, then
    // focus without scrolling again so the on-screen keyboard does not cover it.
    field?.scrollIntoView?.({ block: 'nearest' });
    field?.focus({ preventScroll: true });
  }
  // An empty Other is a slip, not a submit: keep the field open and say what it wants.
  function sendOther(c) {
    const text = draft(c).other.trim();
    if (!otherAllowed(c) || !answerable(c)) return;
    if (!text) {
      if (!draft(c).open) toggleOther(c);
      else $('#other-text')?.focus({ preventScroll: true });
      const message = 'Type an answer first.';
      toast(message, true);
      announce(`Error: ${message}`);
      return;
    }
    if (c.questions?.length) answerStep(c, { text });
    else act(c, 'answer', { option: null, text, note: noteValue(c) }, 'other');
  }
  const acknowledge = (c) =>
      act(c, 'answer', { option: null, text: null, note: noteValue(c) }, 'ack'),
    planRecommended = (c) => openDispatch(c, 'plan', true),
    requestReport = (c) => { if (c.kind === 'info' && c.report_due && paneOf(c)) act(c, 'report', {}, 'request'); },
    restoreRun = (c) => { if (restorable(c)) act(c, 'restore', {}, 'restore'); },
    skip = (c) => act(c, 'skip', {}, 'skip'),
    closePane = (c) => paneOf(c) && act(c, 'close', { note: noteValue(c) }, 'close'),
    later = (c, x) => act(c, 'later', { minutes: x.minutes }, 'later', x);
  function primary(c) {
    if (Date.now() < S.remoteSwapUntil) return;
    if (c.kind === 'info' && c.star?.action) planRecommended(c);
    else if (c.kind === 'info' && paneOf(c)) closePane(c);
    else if (c.kind === 'info') acknowledge(c);
    else if (c.questions?.length && answerable(c)) primaryStep(c);
    else if (answerable(c)) pick(c, recIndex(c));
    else openPeek(c);
  }
  function toggleLater() {
    if (S.pending) return;
    const view = snapshot();
    S.laterOpen = !S.laterOpen;
    render({ ...view, id: S.laterOpen ? 'later-0' : 'later-btn' });
  }
  function closeLater(focus = false) {
    if (!S.laterOpen) return;
    S.laterOpen = false;
    render();
    if (focus) $('#later-btn')?.focus({ preventScroll: true });
  }
  async function jump(c) {
    const pane = paneOf(c);
    if (!pane) {
      toast('This card has no tmux pane.', true);
      return;
    }
    if (pane.startsWith('dsh:')) {
      toast('dsh sessions open in the dsh web UI.', true);
      return;
    }
    if (S.pending) return;
    const v = begin(c, 'jump');
    try {
      await api(`/v1/panes/${enc(pane)}/jump`, {});
      S.pending = null;
      render(v);
      const text = `Jumped to ${c.source?.target || pane}`;
      announce(text);
    } catch (e) {
      failed(c, e, v);
    }
    if (S.dirty && !S.pending) void refresh();
  }
  function showDialog(d) {
    if (!d.open) {
      S.dialogFocus.set(d, document.activeElement);
      d.showModal();
    }
  }
  function closeDialog(d) {
    const prior = S.dialogFocus.get(d);
    if (prior?.isConnected) prior.focus({ preventScroll: true });
    else if (!document.querySelector('dialog[open]')) focusPrimary();
  }
  // Dictation runs in the browser and the daemon only stores the dictionary and the phrases. A browser that can
  // recognize speech on this device is told to keep the audio there. iPhone and iPad Safari offers only
  // webkitSpeechRecognition, which sends the audio to Apple's speech service, so those microphones still work.
  const DICTATION_STATUS = {
    available:
      'Your browser turns speech into text on this device. Please press the microphone beside a text field, speak, then press it again.',
    downloadable:
      "Your browser turns speech into text on this device. The first press of a microphone sets up the browser's speech model for this site, which can take a few minutes.",
    downloading: 'Your browser is downloading its speech model. The microphones work once it finishes.',
    online:
      "Your browser turns speech into text with its own speech service, so the words you dictate leave this device for that service. Please press the microphone beside a text field and speak. It keeps listening across short pauses until you press it again.",
    unavailable:
      "The microphones stay hidden because this browser has no speech recognition. Any dictation your keyboard or operating system provides still types into triage's text fields.",
    insecure: 'The microphones stay hidden because dictation requires the inbox over HTTPS or on localhost.',
  };
  const DICTATION_READY = ['available', 'downloadable', 'downloading', 'online'];
  const DICTATION_ERRORS = {
    'audio-capture': 'The browser could not read the microphone.',
    'language-not-supported': 'This browser cannot recognize the language of this page.',
    network: "The browser's speech service could not be reached.",
    'no-speech': 'The microphone heard no speech.',
    'not-allowed': 'Please allow the microphone for this site to dictate.',
    'service-not-allowed': 'Please allow the microphone for this site to dictate.',
  };
  // Safari reports its silence timeout as one of these, which ends a run without failing it.
  const DICTATION_PAUSES = ['aborted', 'no-speech'];
  function speechApi() {
    return window.SpeechRecognition ?? window.webkitSpeechRecognition ?? null;
  }
  // The recognition to run and whether it can keep the audio on this device. Safari names it webkitSpeechRecognition
  // and has neither available() nor processLocally, so it takes the browser's speech service instead.
  function speechEngine() {
    const Recognition = speechApi();
    if (!window.isSecureContext || typeof Recognition !== 'function') return null;
    return { Recognition, local: typeof Recognition.available === 'function' && 'processLocally' in Recognition.prototype };
  }
  function speechLang() {
    return navigator.language || 'en-US';
  }
  function speechOptions() {
    return { langs: [speechLang()], processLocally: true };
  }
  function micButton(id, label) {
    return paintMic(h('button', { type: 'button', class: 'btn ghost square mic', 'data-dictate': id, 'aria-label': label }));
  }
  function paintMic(b) {
    const phase = S.dictating?.id === b.dataset.dictate ? S.dictating.phase : '';
    b.dataset.phase = phase;
    b.setAttribute('aria-pressed', String(phase === 'listening'));
    b.disabled = phase === 'starting';
    return b;
  }
  function paintMics() {
    document.querySelectorAll('.mic').forEach(paintMic);
  }
  function endDictation() {
    S.dictating = null;
    paintMics();
  }
  async function toggleDictation(id) {
    const active = S.dictating;
    if (active) {
      // Safari answers a press with another press, so its run ends where the words already went in.
      if (active.id === id && active.phase === 'listening') stopDictation(active);
      return;
    }
    const field = $(`#${id}`),
      engine = speechEngine();
    if (!field || field.disabled || !engine) return;
    const run = { id, card: field.closest('.card') ? current()?.id : null, phase: 'starting', recognition: null, cancelled: false, stopped: false, announced: false, error: '' };
    S.dictating = run;
    paintMics();
    // Safari starts recognition only inside the press that asked for it, so its engine starts before any await.
    if (!engine.local) return startDictation(run, engine);
    try {
      if ((await engine.Recognition.available(speechOptions())) !== 'available') return await installSpeech(engine.Recognition);
      if (run.cancelled) return endDictation();
      startDictation(run, engine);
    } catch (e) {
      endDictation();
      toast(`Dictation failed (${e.message}).`, true);
    }
  }
  function startDictation(run, engine) {
    try {
      const recognition = new engine.Recognition();
      Object.assign(recognition, { lang: speechLang(), interimResults: false, continuous: engine.local });
      if (engine.local) recognition.processLocally = true;
      recognition.onresult = (e) => {
        const words = [...e.results]
          .slice(e.resultIndex)
          .filter((result) => result.isFinal)
          .map((result) => result[0].transcript.trim())
          .filter(Boolean)
          .join(' ');
        if (!words || run.cancelled) return;
        if (placeDictation(run, applyDictionary(words, S.dictionary))) run.heard = true;
        else {
          run.cancelled = true;
          recognition.abort();
        }
      };
      recognition.onerror = (e) => {
        run.error = e.error;
      };
      recognition.onend = () => afterDictation(run, engine, recognition);
      recognition.start();
      Object.assign(run, { recognition, heard: false, phase: 'listening' });
      paintMics();
      // A restarted run keeps the one announcement, so Safari's pause does not repeat it.
      if (!run.announced) {
        run.announced = true;
        announce('The microphone is listening. Please press it again to stop.');
      }
    } catch (e) {
      endDictation();
      toast(`Dictation failed (${e.message}).`, true);
    }
  }
  // A local engine keeps listening until it is stopped. Safari ends a run at the first pause, so a run that heard
  // words starts again and one press can collect a whole sentence. A run that heard nothing ends the press, so a
  // quiet room cannot keep the microphone open.
  function afterDictation(run, engine, recognition) {
    if (S.dictating !== run || run.recognition !== recognition) return;
    const pause = DICTATION_PAUSES.includes(run.error);
    if (run.stopped || run.cancelled || (run.error && !pause) || engine.local || !run.heard) return finishDictation(run);
    if (pause) run.error = '';
    startDictation(run, engine);
  }
  function stopDictation(run) {
    run.stopped = true;
    try {
      run.recognition?.stop();
    } catch {
      finishDictation(run);
    }
  }
  function finishDictation(run) {
    const error = run.error;
    if (S.dictating === run) endDictation();
    if (!run.cancelled && error) toast(DICTATION_ERRORS[error] ?? `Dictation stopped (${error}).`, true);
  }
  // The first press on each site installs the browser's speech model. Press again to dictate.
  async function installSpeech(Recognition) {
    const wait = toast('Your browser is setting up speech recognition…', false, true);
    try {
      const ready = await Recognition.install(speechOptions());
      toast(
        ready
          ? 'Speech recognition is ready. Please press the microphone again to dictate.'
          : 'This browser could not set up speech recognition.',
        !ready,
      );
    } catch {
      toast('This browser could not set up speech recognition.', true);
    } finally {
      wait.remove();
      endDictation();
      void checkSpeech();
    }
  }
  // A field outside the view sends its words to a toast, never to another card.
  function placeDictation(run, text) {
    const field = $(`#${run.id}`),
      dialog = field?.closest('dialog');
    if (!field || field.disabled || (dialog && !dialog.open) || (run.card && run.card !== current()?.id)) {
      toast(`The field closed before the words arrived: ${text}`, true);
      return false;
    }
    insertText(field, text);
    return true;
  }
  function insertText(field, text) {
    const start = field.selectionStart ?? field.value.length,
      end = field.selectionEnd ?? start,
      before = field.value.slice(0, start),
      after = field.value.slice(end),
      words = `${before && !/\s$/.test(before) ? ' ' : ''}${text}${after && !/^\s/.test(after) ? ' ' : ''}`;
    field.value = before + words + after;
    field.selectionStart = field.selectionEnd = before.length + words.length;
    field.dispatchEvent(new Event('input', { bubbles: true }));
    field.focus({ preventScroll: true });
  }
  // One "heard = written" entry per line, blank lines skipped. The longest heard form sorts first so it wins.
  function parseDictionary(text) {
    const entries = [];
    text.split('\n').forEach((line, index) => {
      const at = line.indexOf('='),
        heard = line.slice(0, at).trim(),
        written = line.slice(at + 1).trim();
      if (!line.trim()) return;
      if (at < 0 || !heard || !written) throw new Error(`Please write dictionary line ${index + 1} as heard = written.`);
      entries.push([heard, written]);
    });
    return entries.sort((a, b) => b[0].length - a[0].length);
  }
  // Matches whole words and ignores letter case. One pass means replaced text never matches again.
  function applyDictionary(text, entries) {
    if (!entries.length) return text;
    const heard = entries.map(([form]) => form.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|'),
      written = new Map(entries.map(([form, word]) => [form.toLowerCase(), word]));
    return text.replace(
      new RegExp(`(?<![\\p{L}\\p{N}])(?:${heard})(?![\\p{L}\\p{N}])`, 'giu'),
      (match) => written.get(match.toLowerCase()) ?? match,
    );
  }
  function renderPhrases() {
    for (const box of document.querySelectorAll('.phrases')) {
      box.replaceChildren(
        ...S.phrases.map((phrase) => h('button', { type: 'button', class: 'btn phrase', 'data-phrase': phrase }, phrase)),
      );
      box.hidden = !S.phrases.length;
    }
  }
  async function checkSpeech() {
    const engine = speechEngine();
    let status = window.isSecureContext ? 'unavailable' : 'insecure';
    if (engine && !engine.local) status = 'online';
    else if (engine)
      try {
        status = (await engine.Recognition.available(speechOptions())) ?? status;
      } catch {}
    if (!Object.hasOwn(DICTATION_STATUS, status)) status = 'unavailable';
    document.documentElement.dataset.dictation = DICTATION_READY.includes(status) ? 'on' : 'off';
    $('#dictation-status').textContent = DICTATION_STATUS[status];
  }
  async function loadDictation() {
    try {
      const data = await api('/v1/dictation');
      $('#dictation-dictionary').value = data.dictionary;
      $('#dictation-phrases').value = data.phrases;
      S.phrases = data.phrases
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean);
      renderPhrases();
      S.dictionary = parseDictionary(data.dictionary);
    } catch (e) {
      $('#dictation-hint').textContent = `Could not load dictation settings (${e.message}).`;
    }
  }
  async function saveDictation(e) {
    e.preventDefault();
    const save = $('#dictation-save'),
      hint = $('#dictation-hint'),
      dictionary = $('#dictation-dictionary').value;
    try {
      parseDictionary(dictionary);
    } catch (err) {
      hint.textContent = err.message;
      return;
    }
    save.disabled = true;
    hint.textContent = 'Saving…';
    try {
      await api('/v1/dictation', { dictionary, phrases: $('#dictation-phrases').value });
      hint.textContent = 'Saved.';
      await loadDictation();
    } catch (err) {
      hint.textContent = err.message;
    } finally {
      save.disabled = false;
    }
  }
  function openSettings() {
    $('#dictation-hint').textContent = '';
    showDialog($('#settings'));
    void checkSpeech();
    void loadDictation();
  }
  function peekAtBottom(pre) {
    return pre.scrollHeight - pre.scrollTop - pre.clientHeight <= 1;
  }
  function peekAdvanceRatio(pre) {
    if (S.peekMonoRatio) return S.peekMonoRatio;
    const style = getComputedStyle(pre),
      canvas = document.createElement('canvas'),
      context = canvas.getContext('2d');
    if (context) context.font = `100px ${style.fontFamily}`;
    S.peekMonoRatio = (context?.measureText('0').width || 60) / 100;
    return S.peekMonoRatio;
  }
  function peekFitSize(pre, width = S.peekWidth) {
    const style = getComputedStyle(pre),
      padding = (parseFloat(style.paddingLeft) || 0) + (parseFloat(style.paddingRight) || 0),
      available = Math.max(0, pre.clientWidth - padding - 1);
    return Math.min(16, available / (width * peekAdvanceRatio(pre)));
  }
  function peekLandscapeFits(pre) {
    const longSide = Math.max(innerWidth, innerHeight);
    return innerHeight > innerWidth && longSide / (S.peekWidth * peekAdvanceRatio(pre)) >= 8;
  }
  function applyPeekLayout(pre, bottom, scrollTop = pre.scrollTop) {
    const fitSize = S.peekWidth ? peekFitSize(pre) : 12;
    if (S.peekMode === 'fit' && fitSize < 4) {
      S.peekMode = 'readable';
      updatePeekModeButton();
    }
    pre.style.fontSize = `${S.peekMode === 'fit' ? fitSize : 12}px`;
    const content = pre.firstElementChild;
    if (content?.classList.contains('peek-content'))
      content.style.minHeight = S.peekHeight > 0 ? `${S.peekHeight * 1.2}em` : '';
    if (bottom) {
      pre.scrollTop = pre.scrollHeight;
      S.peekFollowBottom = true;
    } else {
      pre.scrollTop = scrollTop;
      S.peekFollowBottom = false;
    }
  }
  function resizePeek() {
    const dialog = $('#peek');
    if (!dialog.open || !S.peekWidth) return;
    const pre = $('#peek-text'),
      scrollTop = pre.scrollTop;
    applyPeekLayout(pre, S.peekFollowBottom, scrollTop);
  }
  function updatePeekModeButton() {
    const button = $('#peek-mode'),
      mode = S.peekMode === 'fit' ? 'Fit' : 'Readable',
      next = S.peekMode === 'fit' ? 'Readable' : 'Fit';
    button.textContent = mode;
    button.setAttribute('aria-label', `Switch to ${next} mode`);
  }
  function setPeekMode(mode) {
    S.peekMode = mode;
    S.peekModePreference = mode;
    S.peekModeTouched = true;
    savePeekMode(mode);
    $('#peek-fit-hint').hidden = true;
    updatePeekModeButton();
    resizePeek();
  }
  function togglePeekMode() {
    setPeekMode(S.peekMode === 'fit' ? 'readable' : 'fit');
  }
  function peekContent(pre) {
    let content = pre.firstElementChild;
    if (!content?.classList.contains('peek-content')) {
      content = document.createElement('span');
      content.className = 'peek-content';
      content.style.display = 'block';
      content.style.whiteSpace = 'pre';
      pre.replaceChildren(content);
    }
    return content;
  }
  async function openPeek(c) {
    const pane = paneOf(c);
    if (!pane) {
      toast('This card has no tmux pane.', true);
      return;
    }
    clearInterval(S.peekTimer);
    S.peekPane = pane;
    S.peekFollowBottom = true;
    S.peekUserScrolled = false;
    S.peekWidth = 0;
    S.peekHeight = 0;
    S.peekSending = false;
    S.peekFetching = 0;
    S.peekLoaded = false;
    S.peekModeTouched = false;
    S.peekMode = S.peekModePreference || 'fit';
    const token = ++S.peekToken,
      d = $('#peek'),
      pre = $('#peek-text');
    $('#peek-target').textContent = c.source?.target || (dshOf(c) ? c.source.label : pane);
    pre.textContent = 'Loading…';
    pre.style.fontSize = S.peekMode === 'readable' ? '12px' : '';
    $('#peek-fit-hint').hidden = true;
    updatePeekModeButton();
    setPeekState('connecting');
    showDialog(d);
    void refreshPeek(token);
    S.peekTimer = setInterval(() => {
      if (d.open) void refreshPeek(token);
    }, 2000);
  }
  async function refreshPeek(token = S.peekToken) {
    const pane = S.peekPane,
      d = $('#peek');
    if (!pane || !d.open || token !== S.peekToken || S.peekFetching === token) return;
    S.peekFetching = token;
    const pre = $('#peek-text');
    try {
      const data = await api(`${paneApi(pane)}/peek`);
      if (token !== S.peekToken || pane !== S.peekPane || !d.open) return;
      const bottom = (!S.peekLoaded && !S.peekUserScrolled) || S.peekFollowBottom,
        scrollTop = pre.scrollTop,
        width = Number(data.width),
        height = Number(data.height),
        content = peekContent(pre),
        text = String(data.text ?? '');
      if (content.textContent !== text) content.textContent = text;
      if (Number.isFinite(width) && width > 0) S.peekWidth = width;
      if (Number.isFinite(height) && height > 0) S.peekHeight = height;
      if (!S.peekLoaded && !S.peekModeTouched && S.peekWidth && peekFitSize(pre) < 8) {
        S.peekMode = 'readable';
        $('#peek-fit-hint').hidden = !peekLandscapeFits(pre);
        updatePeekModeButton();
      }
      S.peekLoaded = true;
      applyPeekLayout(pre, bottom, scrollTop);
      setPeekState(data.state);
    } catch (e) {
      if (token === S.peekToken && pane === S.peekPane && d.open)
        setPeekState('unreachable', `Peek failed: ${e.message}`);
    } finally {
      if (S.peekFetching === token) S.peekFetching = 0;
    }
  }
  function setPeekState(state, detail = null) {
    $('#peek-state').textContent = state;
    $('#peek-state').dataset.state = state;
    const waiting = state === 'waiting';
    $('#peek-input').disabled = !waiting || S.peekSending;
    $('#peek-send').disabled = !waiting || S.peekSending;
    $('#peek-send').setAttribute('aria-busy', String(S.peekSending));
    $('#peek-hint').textContent =
      detail ||
      (waiting
        ? 'The pane is waiting for input.'
        : 'Sending unlocks when the pane is waiting for input.');
  }
  function openCompose() {
    const d = $('#compose');
    $('#compose-hint').textContent = '';
    setComposeMode(S.composeMode);
    showDialog(d);
    $('#compose-prompt').focus({ preventScroll: true });
    loadComposeSessions();
    loadModels();
  }
  // New session, Dispatch and Plan approval fields share the picker.
  const PICKERS = {
    'compose-cwd': { title: 'Working directory', placeholder: 'Search or type a path', free: true, fallback: 'Home directory', options: [] },
    'compose-session': { title: 'tmux session', placeholder: 'Search sessions', free: false, fallback: 'Daemon default', options: [] },
    'compose-engine': { title: 'Engine', placeholder: 'Search engines', free: false, fallback: 'Daemon default', options: [pickerOption('omp'), pickerOption('dsh')] },
    'compose-model': { title: 'Model', placeholder: 'Search or type a selector', free: true, fallback: 'Daemon default', options: [] },
    'compose-thinking': { title: 'Thinking', placeholder: 'Search levels', free: false, fallback: 'Daemon default', options: [] },
    'compose-executor': { title: 'Execution model', placeholder: 'Search or type a selector', free: true, fallback: 'omp smol role', options: [] },
    'compose-executor-effort': { title: 'Execution effort', placeholder: 'Search levels', free: false, fallback: 'Planner effort', options: [] },
    'dispatch-engine': { title: 'Engine', placeholder: 'Search engines', free: false, fallback: 'Daemon default', options: [pickerOption('omp'), pickerOption('dsh')] },
    'dispatch-model': { title: 'Model', placeholder: 'Search or type a selector', free: true, fallback: 'Daemon default', options: [] },
    'dispatch-thinking': { title: 'Thinking', placeholder: 'Search levels', free: false, fallback: 'Daemon default', options: [] },
    'dispatch-executor': { title: 'Execution model', placeholder: 'Search or type a selector', free: true, fallback: 'omp smol role', options: [] },
    'dispatch-executor-effort': { title: 'Execution effort', placeholder: 'Search levels', free: false, fallback: 'Planner effort', options: [] },
    'plan-executor': { title: 'Execution model', placeholder: 'Search roles', free: false, fallback: 'current Plan Review role', options: [] },
    'plan-executor-effort': { title: 'Execution effort', placeholder: 'Search levels', free: false, fallback: 'Planner effort', options: [] },
  };
  const MODEL_PICKERS = [
    'compose-model',
    'compose-executor',
    'compose-executor-effort',
    'dispatch-model',
    'dispatch-executor',
    'dispatch-executor-effort',
    'plan-executor',
    'plan-executor-effort',
  ];
  function pickerOption(value, label = value, hint = null) {
    return { value, label, hint, search: `${label} ${value}`.toLowerCase() };
  }
  // Rows for a query. An empty query lists Default first, a free field offers typed text as a
  // Use row, and then come the options holding every query word, in catalog order. An exact
  // match, or else the first match, starts active ahead of the Use row, so Enter picks from the
  // list unless nothing matches.
  function pickerRows(cfg, query, value) {
    const q = query.trim(),
      words = q.toLowerCase().split(/\s+/).filter(Boolean),
      matches = cfg.options.filter((option) => words.every((word) => option.search.includes(word))),
      exact = matches.findIndex((option) => option.value === q),
      rows = [];
    if (!q) rows.push({ value: '', label: 'Default', hint: cfg.fallback });
    else if (cfg.free && exact < 0) rows.push({ value: q, label: `Use “${q}”`, hint: null });
    rows.push(...matches);
    const lead = rows.length - matches.length,
      active = !q
        ? Math.max(0, rows.findIndex((row) => row.value === value))
        : exact >= 0
          ? exact
          : matches.length
            ? lead
            : rows.length - 1;
    return { rows, active };
  }
  const pickerContent = (row) => [
    el('span', 'opt-label', row.label),
    row.hint && ' ',
    row.hint && el('span', 'opt-desc', row.hint),
  ];
  function setPickerValue(id, value, field = $(`#${id}`)) {
    if (!field) return;
    const cfg = PICKERS[id],
      row = value
        ? cfg.options.find((option) => option.value === value) || { label: value, hint: null }
        : { label: 'Default', hint: cfg.fallback };
    field.value = value;
    field.toggleAttribute('data-default', !value);
    field.replaceChildren(el('span', 'picker-value', pickerContent(row)));
  }
  function refreshPicker(id) {
    const field = $(`#${id}`);
    if (!field) return;
    setPickerValue(id, field.value, field);
    if (S.picker?.id === id) renderPicker();
  }
  function bindPicker(id, field) {
    field.addEventListener('click', () => openPicker(id));
    field.addEventListener('keydown', (e) => {
      if (e.ctrlKey || e.metaKey || e.isComposing) return;
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        openPicker(id);
      } else if (e.key.length === 1 && e.key !== ' ' && !e.altKey) {
        e.preventDefault();
        openPicker(id, e.key);
      }
    });
  }
  function openPicker(id, query) {
    const dialog = $('#picker'),
      field = $(`#${id}`),
      cfg = PICKERS[id],
      search = $('#picker-search'),
      typed = cfg.free && field.value && !cfg.options.some((option) => option.value === field.value);
    if (dialog.open) return;
    S.picker = { id, query: query ?? (typed ? field.value : ''), rows: [], active: -1 };
    $('#picker-title').textContent = cfg.title;
    search.placeholder = cfg.placeholder;
    search.value = S.picker.query;
    field.setAttribute('aria-expanded', 'true');
    // Explicit, because Safari does not focus a tapped button, so activeElement is not the field.
    S.dialogFocus.set(dialog, field);
    dialog.showModal();
    placePicker();
    renderPicker();
    // A short list on a touch screen needs no keyboard; a long one opens ready to type.
    if (!matchMedia('(pointer: coarse)').matches || cfg.options.length > 12 || S.picker.query)
      search.focus({ preventScroll: true });
    // A plan picker reads the shown card's catalog again, because the levels of the card the
    // user just left stay in place otherwise.
    if (MODEL_PICKERS.includes(id) && (!cfg.options.length || id.startsWith('plan-'))) void loadModels();
  }
  function renderPicker() {
    const p = S.picker,
      cfg = PICKERS[p.id],
      value = $(`#${p.id}`).value,
      { rows, active } = pickerRows(cfg, p.query, value),
      empty = $('#picker-empty');
    p.rows = rows;
    p.active = -1;
    $('#picker-list').replaceChildren(
      ...rows.map((row, i) =>
        h(
          'li',
          {
            id: `picker-row-${i}`,
            class: 'picker-row',
            role: 'option',
            'aria-selected': 'false',
            'data-index': i,
            'data-current': row.value === value,
          },
          pickerContent(row),
        ),
      ),
    );
    empty.hidden = rows.length > 0 && cfg.options.length > 0;
    empty.textContent = cfg.options.length ? 'No match' : 'Nothing to choose from yet';
    setPickerActive(active, 'center');
  }
  function setPickerActive(i, block = 'nearest') {
    const p = S.picker,
      list = $('#picker-list'),
      search = $('#picker-search');
    list.children[p.active]?.setAttribute('aria-selected', 'false');
    p.active = p.rows.length ? Math.max(0, Math.min(p.rows.length - 1, i)) : -1;
    const row = list.children[p.active];
    if (!row) return search.removeAttribute('aria-activedescendant');
    row.setAttribute('aria-selected', 'true');
    search.setAttribute('aria-activedescendant', row.id);
    row.scrollIntoView({ block });
  }
  function choosePickerRow(i) {
    const p = S.picker,
      row = p?.rows[i];
    if (!row) return;
    const changed = p.id.endsWith('-engine') && $(`#${p.id}`).value !== row.value;
    setPickerValue(p.id, row.value);
    if (p.id === 'plan-executor' && current()?.kind === 'plan') draft(current()).executor = row.value;
    if (p.id === 'plan-executor-effort' && current()?.kind === 'plan') draft(current()).executorThinking = row.value;
    $('#picker').close();
    if (changed) clearModels(p.id.replace(/-engine$/, ''));
    else if (/-model$|-executor$/.test(p.id)) refreshEfforts(sheetOf(p.id));
  }
  // Model selectors belong to one engine. A new engine clears its sheet's model and executor.
  function clearModels(sheet) {
    setPickerValue(`${sheet}-model`, '');
    setPickerValue(`${sheet}-executor`, '');
    setPickerValue(`${sheet}-executor-effort`, '');
    void loadModels();
  }
  // Phones fill the visual viewport, so the on-screen keyboard never hides rows. Wider screens
  // hang the list under its field, or above it when the room below is short.
  function placePicker() {
    const dialog = $('#picker');
    if (!S.picker || !dialog.open) return;
    const vv = window.visualViewport,
      top = vv?.offsetTop ?? 0,
      bottom = vv && vv.height > 0 ? top + vv.height : innerHeight;
    dialog.removeAttribute('style');
    if (matchMedia('(max-width: 639px)').matches) {
      dialog.style.top = `${top}px`;
      dialog.style.height = `${bottom - top}px`;
      return;
    }
    const r = $(`#${S.picker.id}`).getBoundingClientRect(),
      width = Math.min(Math.max(r.width, 360), innerWidth - 16),
      below = bottom - r.bottom - 12,
      above = r.top - top - 12,
      up = below < 240 && above > below;
    dialog.style.left = `${Math.max(8, Math.min(r.left, innerWidth - width - 8))}px`;
    dialog.style.width = `${width}px`;
    dialog.style.maxHeight = `${Math.min(400, up ? above : below)}px`;
    if (up) dialog.style.bottom = `${innerHeight - r.top + 4}px`;
    else dialog.style.top = `${r.bottom + 4}px`;
  }
  // Blank fields fall back to the server defaults the Default row names.
  async function loadComposeSessions() {
    try {
      const data = await api('/v1/tmux-sessions');
      PICKERS['compose-session'].fallback = data.default;
      PICKERS['compose-session'].options = data.sessions.map((name) => pickerOption(name));
      PICKERS['compose-cwd'].options = data.directories.map((path) => pickerOption(path));
      refreshPicker('compose-session');
      refreshPicker('compose-cwd');
    } catch {
      // The server falls back to its default session.
    }
  }
  // The sheet a model or executor field belongs to, for the effort list it refits.
  function sheetOf(id) {
    return ['compose', 'dispatch', 'plan'].find((sheet) => id.startsWith(`${sheet}-`)) || null;
  }
  // Each sheet lists the models of the engine its Engine field picks, or the daemon's when Default.
  // Its model field keeps the catalog data, so an effort list can follow the model picked.
  async function loadModels() {
    try {
      const base = await api('/v1/models'),
        daemon = base.engine || 'omp',
        lists = { [daemon]: base },
        load = async (engine) => (lists[engine] ??= await api(`/v1/models?engine=${engine}`)),
        options = (list) =>
          (list || []).map(({ selector, name }) => pickerOption(selector, name, name === selector ? null : selector));
      for (const sheet of ['compose', 'dispatch']) {
        const engine = $(`#${sheet}-engine`)?.value || daemon,
          data = await load(engine),
          model = PICKERS[`${sheet}-model`],
          thinking = PICKERS[`${sheet}-thinking`],
          executor = PICKERS[`${sheet}-executor`];
        model.data = data;
        PICKERS[`${sheet}-engine`].fallback = daemon;
        model.options = executor.options = options(data.models);
        model.fallback = data.default
          ? model.options.find((option) => option.value === data.default)?.label || data.default
          : `${engine} default`;
        thinking.fallback = data.default_thinking ?? `${engine} default`;
        executor.fallback = engine === 'dsh' ? 'planner model' : 'omp smol role';
        if (sheet === 'compose') $('#compose-session-pair').hidden = engine === 'dsh';
        refreshEfforts(sheet);
        for (const id of ['engine', 'model', 'thinking', 'executor', 'executor-effort'])
          refreshPicker(`${sheet}-${id}`);
      }
      // A plan card from the other harness offers that harness's execution models.
      const plan = current(),
        planEngine = plan?.kind === 'plan' ? plan.engine || 'omp' : daemon;
      planExecutorLabels(planEngine);
      const planModels = await load(planEngine);
      PICKERS['plan-executor'].data = planModels;
      PICKERS['plan-executor'].options = options(planModels.execution_models);
      refreshEfforts('plan');
      refreshPicker('plan-executor');
      refreshPicker('plan-executor-effort');
    } catch {
      // The server falls back to its launch flags or the engine's defaults.
    }
  }
  // A model decides which thinking levels it accepts. dsh reports them per model, an
  // engine that names none offers its whole list, and a model with no reasoning control
  // offers only Default, which sends no effort.
  // An execution effort belongs to the model it changes. dsh changes one only along a model,
  // so its picker waits for one and says so; omp applies an effort alone to its smol role.
  function refreshEfforts(sheet) {
    const data =
      sheet === 'plan' ? PICKERS['plan-executor'].data : PICKERS[`${sheet}-model`]?.data;
    if (!data) return;
    const dsh = data.engine === 'dsh',
      levels = (selector) => effortLevels(data, selector),
      value = (id) => $(`#${id}`)?.value.trim() || '';
    const fit = (id, allowed) => {
      const cfg = PICKERS[id];
      if (!cfg) return;
      cfg.options = allowed.map((level) => pickerOption(level));
      const field = $(`#${id}`);
      if (field?.value && !cfg.options.some((option) => option.value === field.value))
        setPickerValue(id, '');
      refreshPicker(id);
    };
    const fitExecutorEffort = (id, executor, ompLabel) => {
      const cfg = PICKERS[id],
        waiting = dsh && !executor;
      if (!cfg) return;
      cfg.fallback = waiting ? 'needs an execution model' : dsh ? 'Planner effort' : ompLabel;
      fit(id, waiting ? [] : levels(executor || ''));
    };
    if (sheet === 'plan') {
      fitExecutorEffort('plan-executor-effort', value('plan-executor'), 'Role level');
      return;
    }
    fit(`${sheet}-thinking`, levels(value(`${sheet}-model`)));
    fitExecutorEffort(`${sheet}-executor-effort`, value(`${sheet}-executor`), 'Selector level');
  }
  // The thinking levels a model accepts. An engine that reports none for a model means
  // it has no reasoning control, so only Default is left; an engine that reports no
  // per-model list at all offers its whole list.
  function effortLevels(data, selector) {
    const models = data?.models || [],
      model =
        models.find((entry) => entry.selector === selector) ||
        models.find((entry) => entry.selector === data?.default);
    return model && Array.isArray(model.efforts) ? model.efforts : data?.thinking || [];
  }
  // An image attached in the New session or Dispatch sheet. It is read as base64 and sent
  // with the prompt; the daemon stores one copy for both engines.
  const IMAGE_TYPES = new Set(['image/png', 'image/jpeg', 'image/webp', 'image/gif']),
    IMAGE_LIMIT = 10 * 1024 * 1024,
    sheetImages = { compose: null, dispatch: null };

  function showSheetImage(prefix) {
    const image = sheetImages[prefix],
      preview = $(`#${prefix}-image-preview`),
      thumb = $(`#${prefix}-image-thumb`),
      name = $(`#${prefix}-image-name`);
    if (!preview || !thumb) return;
    if (image?.url) URL.revokeObjectURL(image.url);
    if (image) image.url = null;
    if (!image?.file) {
      preview.hidden = true;
      thumb.removeAttribute('src');
      name.textContent = '';
      return;
    }
    image.url = URL.createObjectURL(image.file);
    thumb.src = image.url;
    name.textContent = image.file.name;
    preview.hidden = false;
  }
  function attachSheetImage(prefix, file) {
    const hint = $(`#${prefix}-hint`);
    if (!file) return;
    if (!IMAGE_TYPES.has(file.type)) {
      hint.textContent = 'Attach a PNG, JPEG, WebP or GIF image.';
      return;
    }
    if (file.size > IMAGE_LIMIT) {
      hint.textContent = 'That image is over 10 MB.';
      return;
    }
    sheetImages[prefix] = { file, url: null };
    showSheetImage(prefix);
    hint.textContent = '';
  }
  function clearSheetImage(prefix) {
    const image = sheetImages[prefix];
    if (image?.url) URL.revokeObjectURL(image.url);
    sheetImages[prefix] = null;
    const input = $(`#${prefix}-image`);
    if (input) input.value = '';
    showSheetImage(prefix);
  }
  function sheetImageBody(prefix) {
    const file = sheetImages[prefix]?.file;
    if (!file) return Promise.resolve(null);
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        const result = String(reader.result || ''),
          comma = result.indexOf(',');
        if (comma < 0) {
          reject(new Error('Could not read the image.'));
          return;
        }
        resolve({ media_type: file.type, data: result.slice(comma + 1), name: file.name });
      };
      reader.onerror = () => reject(new Error('Could not read the image.'));
      reader.readAsDataURL(file);
    });
  }
  function wireSheetImage(prefix) {
    const input = $(`#${prefix}-image`),
      add = $(`#${prefix}-image-add`),
      remove = $(`#${prefix}-image-remove`);
    if (!input || !add) return;
    add.addEventListener('click', () => input.click());
    input.addEventListener('change', () => attachSheetImage(prefix, input.files?.[0]));
    remove?.addEventListener('click', () => clearSheetImage(prefix));
    const text = $(prefix === 'compose' ? '#compose-prompt' : '#dispatch-note');
    text?.addEventListener('paste', (e) => {
      const file = [...(e.clipboardData?.files || [])].find((f) => IMAGE_TYPES.has(f.type));
      if (file && !e.clipboardData.getData('text')) {
        e.preventDefault();
        attachSheetImage(prefix, file);
      }
    });
  }

  async function submitCompose(e) {
    e.preventDefault();
    const prompt = $('#compose-prompt').value.trim(),
      cwd = $('#compose-cwd').value.trim(),
      start = $('#compose-start'),
      hint = $('#compose-hint');
    if (!prompt || start.disabled) return;
    // Disable before reading an attached image: a second submit during that read would
    // otherwise start the same session twice.
    start.disabled = true;
    let image;
    try {
      image = await sheetImageBody('compose');
    } catch (err) {
      hint.textContent = err.message;
      start.disabled = false;
      return;
    }
    const body = {
      prompt,
      cwd: cwd || null,
      session: $('#compose-session').value.trim() || null,
      engine: $('#compose-engine').value || null,
      model: $('#compose-model').value.trim() || null,
      thinking: $('#compose-thinking').value.trim() || null,
      mode: S.composeMode,
    };
    if (image) body.image = image;
    if (S.composeMode === 'plan-into') {
      body.executor = $('#compose-executor').value.trim() || null;
      body.executor_thinking = $('#compose-executor-effort').value.trim() || null;
    }
    // The daemon does the launch, which can take it a while. The sheet closes now and the
    // toast reports the result. A busy host never holds the inbox.
    hint.textContent = '';
    $('#compose-prompt').value = '';
    $('#compose').close();
    announce('Starting a session.');
    const pending = toast('Starting…', false, true);
    try {
      const data = await api('/v1/sessions', body);
      // A launch the daemon accepted but has not finished answers `starting`. It keeps running
      // there, and a failure reaches the inbox as a card.
      if (data.starting) {
        announce('Starting the session.');
        toast(body.engine === 'dsh' ? 'Starting a dsh session.' : 'Starting the session.');
      } else {
        announce(`Started · ${data.pane}`);
        if (data.pane?.startsWith('dsh:')) toast('Started a dsh session.');
        else toast(`Started · ${data.pane}`);
      }
    } catch (err) {
      // Put the draft back so a retry is one Start, unless a newer draft sits in the sheet.
      const message = err.message || 'Could not start the session.';
      if (!$('#compose-prompt').value.trim()) $('#compose-prompt').value = prompt;
      hint.textContent = message;
      if (!$('dialog[open]')) showDialog($('#compose'));
      toast(message, true);
    } finally {
      pending?.remove();
      start.disabled = false;
    }
  }

  function radioKeydown(e, select) {
    const bs = [...e.currentTarget.children],
      i = bs.findIndex((b) => b.getAttribute('aria-checked') === 'true'),
      step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (!step) return;
    e.preventDefault();
    const next = bs[(i + step + bs.length) % bs.length];
    select(next);
    next.focus();
  }
  function selectSegment(group, mode) {
    for (const b of group.children) {
      const on = b.dataset.mode === mode;
      b.setAttribute('aria-checked', String(on));
      b.tabIndex = on ? 0 : -1;
    }
  }
  function setComposeMode(mode) {
    S.composeMode = mode;
    selectSegment($('#compose-mode'), mode);
    $('#compose-mode-help').textContent = {
      goal: 'The session works toward the prompt as a goal until it reports done.',
      plan: 'Read-only planning. The plan arrives here for review before anything runs.',
      'plan-into':
        'The Model above drafts a plan read-only, the harness approves it without review, and the execution model carries it out at the execution effort chosen below.',
    }[mode];
    $('#compose-models').hidden = mode !== 'plan-into';
  }
  function setDispatchMode(mode) {
    const d = S.drafts.get(S.dispatchId);
    if (d?.dispatchAnswers && mode === 'investigate') mode = 'act';
    selectSegment($('#dispatch-mode'), mode);
    const investigate = $('#dispatch-mode-investigate');
    if (investigate) investigate.hidden = Boolean(d?.dispatchAnswers);
    $('#dispatch-mode-help').textContent = {
      investigate: 'Read-only. The session looks into it and reports back.',
      act: 'The session carries out the action end to end.',
      plan: 'The session prepares a plan for review before acting.',
      'plan-into':
        'The planner drafts a plan read-only, the harness approves it without review, and the execution model carries it out in a new session at the execution effort chosen below.',
    }[mode];
    $('#dispatch-models').hidden = mode !== 'plan-into';
  }
  function openDispatch(c, mode, fromCard = false, option = null, answers = null) {
    if (!c || S.pending || Date.now() < S.remoteSwapUntil) return;
    const d = draft(c);
    S.dispatchId = c.id;
    d.dispatchOption = option;
    d.dispatchAnswers = answers;
    d.dispatchMode = mode || d.dispatchMode || 'investigate';
    if (fromCard) d.dispatch = d.note;
    if (dshOf(c) && $('#dispatch-engine').value !== 'dsh') {
      setPickerValue('dispatch-engine', 'dsh');
      clearModels('dispatch');
    } else if (!PICKERS['dispatch-model'].options.length) void loadModels();
    setDispatchMode(d.dispatchMode);
    $('#dispatch-note').value = d.dispatch;
    $('#dispatch-hint').textContent = '';
    $('#dispatch-start').disabled = false;
    showDialog($('#dispatch'));
    $('#dispatch-note').focus({ preventScroll: true });
  }
  async function submitDispatch(e) {
    e.preventDefault();
    const c = S.cards.find((x) => x.id === S.dispatchId),
      start = $('#dispatch-start'),
      hint = $('#dispatch-hint');
    if (start.disabled) return;
    if (!c) {
      hint.textContent = 'This card is no longer in the queue.';
      return;
    }
    const d = draft(c),
      mode = d.dispatchMode,
      note = $('#dispatch-note').value.trim() || null,
      model = $('#dispatch-model').value.trim() || null,
      thinking = $('#dispatch-thinking').value.trim() || null,
      engine = $('#dispatch-engine').value || null;
    d.dispatch = $('#dispatch-note').value;
    let image;
    try {
      image = await sheetImageBody('dispatch');
    } catch (err) {
      hint.textContent = err.message;
      return;
    }
    start.disabled = true;
    hint.textContent = 'Launching…';
    const body = { mode, note, engine, model, thinking };
    if (image) body.image = image;
    if (mode === 'plan-into') {
      body.executor = $('#dispatch-executor').value.trim() || null;
      body.executor_thinking = $('#dispatch-executor-effort').value.trim() || null;
    }
    let ok;
    if (d.dispatchAnswers) {
      d.note = $('#dispatch-note').value;
      const answer = { answers: d.dispatchAnswers, note, mode, engine, model, thinking };
      if (image) answer.image = image;
      if (mode === 'plan-into') {
        answer.executor = body.executor;
        answer.executor_thinking = body.executor_thinking;
      }
      ok = await act(c, 'answer', answer, 'send');
    } else if (d.dispatchOption != null) {
      d.note = $('#dispatch-note').value;
      const answer = { option: d.dispatchOption, text: null, note, mode, engine, model, thinking };
      if (image) answer.image = image;
      if (mode === 'plan-into') {
        answer.executor = body.executor;
        answer.executor_thinking = body.executor_thinking;
      }
      ok = await act(c, 'answer', answer, `option-${d.dispatchOption}`);
    } else {
      ok = await act(c, 'dispatch', body, 'dispatch');
    }
    start.disabled = false;
    if (ok || !S.cards.some((x) => x.id === c.id)) {
      hint.textContent = '';
      $('#dispatch').close();
    } else if (hint.textContent === 'Launching…')
      hint.textContent = 'Could not dispatch.';
  }
  async function sendPeek(e) {
    e.preventDefault();
    const input = $('#peek-input'),
      pane = S.peekPane,
      token = S.peekToken,
      text = input.value;
    if (!text.trim() || !pane || S.peekSending || $('#peek-state').dataset.state !== 'waiting')
      return;
    S.peekSending = true;
    setPeekState('waiting');
    try {
      await api(`${paneApi(pane)}/send`, { text });
      if (token !== S.peekToken || pane !== S.peekPane || !$('#peek').open) return;
      input.value = '';
      const sent = pane.startsWith('dsh:') ? 'Sent to the dsh session.' : 'Sent to the pane.';
      $('#peek-hint').textContent = sent;
      announce(sent);
      void refreshPeek(token);
    } catch (e) {
      if (token === S.peekToken && pane === S.peekPane && $('#peek').open) {
        $('#peek-hint').textContent = `Send failed: ${e.message}`;
        announce(`Send failed: ${e.message}`);
      }
    } finally {
      if (token === S.peekToken && pane === S.peekPane && $('#peek').open) {
        S.peekSending = false;
        const waiting = $('#peek-state').dataset.state === 'waiting';
        $('#peek-input').disabled = !waiting;
        $('#peek-send').disabled = !waiting;
        $('#peek-send').setAttribute('aria-busy', 'false');
      }
    }
  }
  async function reloadHistoryCard(card, button) {
    if (button.disabled) return;
    button.disabled = true;
    const name = card.title || 'Untitled card';
    try {
      await api(`/v1/cards/${enc(card.id)}/reload`, {});
      announce(`Reloaded · ${name}`);
      void refresh();
    } catch (e) {
      const text = e.status === 409 ? `Already queued · ${name}` : `Reload failed: ${e.message}`;
      toast(text, true);
      announce(text);
    } finally {
      button.disabled = false;
    }
  }
  function historyItem(row) {
    const c = row?.card || {},
      s = row?.swipe || {},
      at = stamp(s.at_ms),
      opt = Number.isInteger(s.option) ? c.options?.[s.option]?.label : null,
      status =
        s.action === 'done'
          ? `done${s.reason ? ` (${s.reason})` : ''}`
          : String(s.action || 'unknown'),
      details = [opt, s.text !== opt && s.text, s.note && `Note: ${s.note}`].filter(Boolean);
    return el(
      'li',
      'history-item',
      el(
        'div',
        'h-meta',
        chip(c.kind || 'unknown'),
        el('span', 'h-source', sourceName(c.source || {})),
        at == null
          ? el('span', null, 'Time unknown')
          : h('time', { datetime: new Date(at).toISOString() }, clock(at)),
        s.action !== 'later' &&
          c.id &&
          h(
            'button',
            {
              type: 'button',
              class: 'btn ghost h-context',
              'aria-keyshortcuts': 't',
              onclick: () => openContext(c.id),
            },
            'Context',
          ),
        s.action !== 'later' &&
          c.id &&
          h(
            'button',
            {
              type: 'button',
              class: 'btn ghost h-reload',
              onclick: (e) => reloadHistoryCard(c, e.currentTarget),
            },
            'Reload',
          ),
      ),
      h('p', { class: 'h-title', title: c.title || null }, c.title || 'Untitled card'),
      el(
        'p',
        'h-result',
        el('span', 'status', status),
        details.length ? ` · ${details.join(' · ')}` : '',
      ),
    );
  }
  function openHistory() {
    const list = $('#history-list');
    list.replaceChildren(el('li', 'muted', 'Loading history…'));
    showDialog($('#history'));
    const loadRows = () => historyRows().then((rows) => rows ?? loadRows());
    return loadRows()
      .then((rows) => {
        if (!rows) return;
        list.replaceChildren(
          ...(rows.length
            ? rows.map(historyItem)
            : [el('li', 'muted', 'No handled cards in recent history.')]),
        );
        summarize(rows);
        if (!S.cards.length) render();
      })
      .catch((e) =>
        list.replaceChildren(
          h('li', { class: 'muted', role: 'status' }, `History could not be loaded: ${e.message}`),
        ),
      );
  }

  // The context thread: one card's whole story, oldest first.
  async function openContext(id) {
    const token = ++S.contextToken,
      d = $('#context'),
      list = $('#context-list'),
      hint = $('#context-hint');
    list.replaceChildren(el('li', 'muted', 'Loading context…'));
    hint.textContent = '';
    showDialog(d);
    try {
      const data = await api(`/v1/cards/${enc(id)}/context`);
      if (token !== S.contextToken || !d.open) return;
      const items = data.items || [];
      list.replaceChildren(
        ...(items.length
          ? items.map((item) => contextItem(item, data.anchor))
          : [el('li', 'muted', 'No cards in this thread.')]),
      );
      // Consecutive cards of one transcript hang together under a left rule.
      [...list.children].forEach((row, i) => {
        const refs = items[i]?.card?.refs || [],
          previous = items[i - 1]?.card?.refs || [];
        row.classList.toggle('in-session', Boolean(refs[0]) && refs[0] === previous[0]);
      });
      hint.textContent = data.truncated ? `${data.truncated} older cards not shown.` : '';
      updateClamps();
      const row = $('.context-item.anchor', list) ?? list.firstElementChild;
      // scrollIntoView scrolls the overflow-hidden dialog too and buries its header,
      // so only the list moves, and the anchor starts at its top edge.
      if (row) list.scrollTop = row.getBoundingClientRect().top - list.getBoundingClientRect().top + list.scrollTop;
      row?.focus({ preventScroll: true });
      announce('Context opened');
    } catch (e) {
      if (token === S.contextToken && d.open)
        list.replaceChildren(
          h('li', { class: 'muted', role: 'status' }, `Context could not be loaded: ${e.message}`),
        );
    }
  }
  function contextItem(item, anchorId) {
    const c = item?.card || {},
      swipes = item?.swipes || [],
      s = swipes[swipes.length - 1] || {},
      at = stamp(c.created_ms),
      opt = Number.isInteger(s.option) ? c.options?.[s.option]?.label : null,
      status =
        s.action === 'done'
          ? `done${s.reason ? ` (${s.reason})` : ''}`
          : String(s.action || 'queued'),
      details = [opt, s.text !== opt && s.text, s.note && `Note: ${s.note}`].filter(Boolean),
      anchor = c.id === anchorId,
      superseded = s.reason === 'superseded';
    return h(
      'li',
      {
        class: `context-item${anchor ? ' anchor' : ''}${superseded ? ' superseded' : ''}`,
        tabindex: anchor ? '-1' : null,
      },
      el(
        'div',
        'h-meta',
        chip(c.kind || 'unknown'),
        el('span', 'h-source', sourceName(c.source || {})),
        at == null
          ? el('span', null, 'Time unknown')
          : h('time', { datetime: new Date(at).toISOString() }, clock(at)),
        anchor && el('span', 'tag', 'This card'),
      ),
      h('p', { class: 'h-title', title: c.title || null }, c.title || 'Untitled card'),
      el(
        'p',
        'h-result',
        el('span', 'status', status),
        details.length ? ` · ${details.join(' · ')}` : '',
      ),
      c.body ? block(c, 'Report', el('div', 'md', md(c.body))) : null,
    );
  }
  function updateAges() {
    document.querySelectorAll('[data-age]').forEach((el) => {
      el.textContent = age(Number(el.dataset.age));
    });
    if (S.initialized && !S.cards.length && S.history.dayStart !== historyDayStart()) emptyHistory();
  }

  // keys
  const textField = (t) =>
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    t.isContentEditable;
  function menuKey(e) {
    const bs = [...$('#later-menu').querySelectorAll('button:not(:disabled)')],
      i = bs.indexOf(document.activeElement),
      focus = (n) => {
        e.preventDefault();
        bs[Math.max(0, Math.min(bs.length - 1, n))]?.focus();
      };
    if (e.key === 'Escape' || e.key === 'l') {
      e.preventDefault();
      closeLater(true);
      return true;
    }
    if (e.key === 'ArrowDown') {
      focus(i < 0 ? 0 : i + 1);
      return true;
    }
    if (e.key === 'ArrowUp') {
      focus(i < 0 ? bs.length - 1 : i - 1);
      return true;
    }
    if (e.key === 'Home') {
      focus(0);
      return true;
    }
    if (e.key === 'End') {
      focus(bs.length - 1);
      return true;
    }
    if (/^[1-3]$/.test(e.key)) {
      e.preventDefault();
      bs[Number(e.key) - 1]?.click();
      return true;
    }
    return false;
  }
  document.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey || e.isComposing) return;
    const t = e.target,
      peek = $('#peek');
    if (peek.open) {
      if (e.key === 'f' && !textField(t)) {
        e.preventDefault();
        togglePeekMode();
      }
      return;
    }
    if (document.querySelector('dialog[open]')) return;
    if (textField(t)) {
      if (e.key === 'Escape') {
        e.preventDefault();
        const c = current();
        if (t.id === 'other-text' && c) toggleOther(c);
        else {
          t.blur();
          focusPrimary();
        }
      }
      return;
    }
    const key = e.key,
      run = (fn) => {
        e.preventDefault();
        fn();
      };
    if (Date.now() < S.remoteSwapUntil && (/^[1-9]$/.test(key) || key === 'Enter')) {
      e.preventDefault();
      return;
    }
    if (S.laterOpen) {
      if (menuKey(e) || key === 'Enter') return;
      return;
    }
    if (key === 'Escape' && S.countsExpanded)
      return run(() => {
        closeCounts();
        $('#count-toggle')?.focus({ preventScroll: true });
      });
    if (key === '?') return run(() => showDialog($('#keys')));
    if (key === 'h') return run(openHistory);
    if (key === 'c') return run(openCompose);
    const c = current();
    if (!c) return;
    if (/^[1-9]$/.test(key)) return run(() => pick(c, Number(key) - 1));
    if (t.closest?.('.report-wrap .body') && ['ArrowUp', 'ArrowDown'].includes(key)) return;
    if (key === 'Enter') {
      if (!t.closest?.('button,a,select')) run(() => primary(c));
      return;
    }
    if (key === 'a') return run(() => primary(c));
    switch (key) {
      case 'Escape':
        if (otherAllowed(c) && draft(c).open) toggleOther(c);
        else closeLater(true);
        break;
      case 'o':
        if (otherAllowed(c)) run(() => toggleOther(c));
        break;
      case 'n':
        run(() => $('#note')?.focus());
        break;
      case 's':
        run(() => skip(c));
        break;
      case 'i':
        run(() => openDispatch(c, 'investigate'));
        break;
      case 'A':
        run(() => openDispatch(c, 'act'));
        break;
      case 'x':
        run(() => closePane(c));
        break;
      case 'l':
        run(toggleLater);
        break;
      case 'p':
        run(() => openDispatch(c, 'plan'));
        break;
      case 'P':
        run(() => openDispatch(c, 'plan-into'));
        break;
      case 'r':
        run(() => requestReport(c));
        break;
      case 'R':
        run(() => restoreRun(c));
        break;
      case 't':
        run(() => openContext(c.id));
        break;
      case 'v':
        run(() => openPeek(c));
        break;
      case 'J':
        run(() => jump(c));
        break;
      case 'j':
      case 'ArrowDown':
      case 'ArrowRight':
        run(() => move(1));
        break;
      case 'k':
      case 'ArrowUp':
      case 'ArrowLeft':
        run(() => move(-1));
        break;
    }
  });
  document.addEventListener('click', (e) => {
    if (S.countsExpanded && !e.target.closest?.('#counts, #count-toggle')) closeCounts();
    if (e.target.closest?.('[data-retry]')) return retryNow();
    const close = e.target.closest?.('[data-close]');
    if (close) {
      close.closest('dialog')?.close();
      return;
    }
    if (e.target instanceof HTMLDialogElement) {
      e.target.close();
      return;
    }
    if (S.laterOpen && !e.target.closest?.('.later')) closeLater(false);
    const mic = e.target.closest?.('[data-dictate]'),
      phrase = e.target.closest?.('[data-phrase]');
    if (mic) void toggleDictation(mic.dataset.dictate);
    else if (phrase) insertText($(`#${phrase.parentElement.dataset.for}`), phrase.dataset.phrase);
  });
  document.addEventListener('focusin', (e) => {
    const menu = $('#later-menu');
    // Safari does not focus a pressed button, so the press focuses the card around the menu.
    // Closing the menu on that focus would rebuild the card before the click reaches the button.
    if (S.laterOpen && !menu?.contains(e.target) && !e.target.contains?.(menu)) closeLater(false);
    if (S.countsExpanded && !e.target.closest?.('#count-toggle, #counts')) closeCounts();
  });
  // Closing the sheet that holds the field being dictated stops listening and drops the words.
  document.addEventListener(
    'close',
    (e) => {
      const run = S.dictating;
      if (!run || !e.target.contains($(`#${run.id}`))) return;
      run.cancelled = true;
      run.recognition?.abort();
    },
    true,
  );
  $('#btn-history').addEventListener('click', openHistory);
  $('#btn-compose').addEventListener('click', openCompose);
  $('#btn-keys').addEventListener('click', () => showDialog($('#keys')));
  // The worker reloads a tab that cannot answer its ping, which is how a tab running an older
  // inbox picks up a deploy it could not notice by itself.
  if ('serviceWorker' in navigator) {
    navigator.serviceWorker.addEventListener('message', (e) => {
      if (e.data?.type === 'triage-shell-ping') e.source?.postMessage({ type: 'triage-shell-pong' });
    });
  }
  $('#count-toggle')?.addEventListener('click', countToggle);
  $('#btn-settings').addEventListener('click', openSettings);
  $('#dictation-form').addEventListener('submit', saveDictation);
  // Theme: the inline head script sets data-theme first; the toggle swaps and persists it.
  const icon = (d) =>
      `<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${d}</svg>`,
    SUN = icon(
      '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/>',
    ),
    MOON = icon('<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>');
  function applyTheme(theme) {
    document.documentElement.dataset.theme = theme;
    const meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.content = theme === 'dark' ? '#0a0a0a' : '#f2f4f7';
    const btn = $('#btn-theme');
    if (btn) {
      btn.innerHTML = theme === 'dark' ? SUN : MOON;
      btn.setAttribute('aria-label', `Switch to ${theme === 'dark' ? 'light' : 'dark'} mode`);
    }
  }
  applyTheme(document.documentElement.dataset.theme || 'light');
  $('#btn-theme')?.addEventListener('click', () => {
    const theme = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark';
    applyTheme(theme);
    try {
      localStorage.setItem('triage-theme', theme);
    } catch {}
  });
  $('#peek-mode').addEventListener('click', togglePeekMode);
  $('#peek-text').addEventListener(
    'scroll',
    () => {
      const pre = $('#peek-text');
      S.peekFollowBottom = peekAtBottom(pre);
      if ($('#peek').open && !S.peekLoaded && !S.peekFollowBottom) S.peekUserScrolled = true;
    },
    { passive: true },
  );
  window.addEventListener('resize', resizePeek);
  window.addEventListener('resize', updateClamps);
  window.addEventListener('orientationchange', resizePeek);
  $('#peek').addEventListener('close', () => {
    clearInterval(S.peekTimer);
    S.peekPane = null;
    S.peekToken++;
    S.peekSending = false;
    S.peekFetching = 0;
    S.peekLoaded = false;
    $('#peek-fit-hint').hidden = true;
    closeDialog($('#peek'));
  });
  $('#context').addEventListener('close', () => {
    S.contextToken++;
    closeDialog($('#context'));
  });
  $('#history').addEventListener('close', () => closeDialog($('#history')));
  $('#keys').addEventListener('close', () => closeDialog($('#keys')));
  $('#compose').addEventListener('close', () => {
    clearSheetImage('compose');
    closeDialog($('#compose'));
  });
  $('#dispatch').addEventListener('close', () => {
    clearSheetImage('dispatch');
    closeDialog($('#dispatch'));
  });
  $('#dispatch-form').addEventListener('submit', submitDispatch);
  $('#dispatch-note').addEventListener('input', (e) => {
    const d = S.drafts.get(S.dispatchId);
    if (d) d.dispatch = e.target.value;
  });
  $('#dispatch').addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && !e.isComposing) {
      e.preventDefault();
      $('#dispatch-form').requestSubmit();
    }
  });
  for (const [group, set] of [[$('#dispatch-mode'), setDispatchMode], [$('#compose-mode'), setComposeMode]]) {
    group.addEventListener('click', (e) => {
      const b = e.target.closest('[data-mode]');
      if (b) set(b.dataset.mode);
    });
    group.addEventListener('keydown', (e) => {
      const bs = [...group.children],
        i = bs.findIndex((b) => b.getAttribute('aria-checked') === 'true'),
        step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
      if (!step) return;
      e.preventDefault();
      const next = bs[(i + step + bs.length) % bs.length];
      set(next.dataset.mode);
      next.focus();
    });
  }
  $('#compose-form').addEventListener('submit', submitCompose);
  wireSheetImage('compose');
  wireSheetImage('dispatch');
  $('#compose').addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && !e.isComposing) {
      e.preventDefault();
      $('#compose-form').requestSubmit();
    }
  });
  for (const id of Object.keys(PICKERS)) {
    const field = $(`#${id}`);
    if (!field) continue;
    setPickerValue(id, '');
    bindPicker(id, field);
  }
  $('#picker-search').addEventListener('input', (e) => {
    if (!S.picker) return;
    S.picker.query = e.target.value;
    renderPicker();
  });
  $('#picker-search').addEventListener('keydown', (e) => {
    const p = S.picker;
    if (!p || e.isComposing) return;
    const step = { ArrowDown: 1, ArrowUp: -1, PageDown: 8, PageUp: -8 }[e.key];
    if (step) {
      e.preventDefault();
      setPickerActive(p.active + step);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      choosePickerRow(p.active);
    } else if (e.key === 'Escape' || e.key === 'Tab') {
      e.preventDefault();
      $('#picker').close();
    }
  });
  $('#picker-list').addEventListener('mousedown', (e) => e.preventDefault());
  $('#picker-list').addEventListener('click', (e) => {
    const row = e.target.closest('[role="option"]');
    if (row) choosePickerRow(Number(row.dataset.index));
  });
  // Dragging the list on a phone drops the keyboard so more rows show, as native search lists do.
  $('#picker-list').addEventListener('touchmove', () => $('#picker-search').blur(), { passive: true });
  $('#picker').addEventListener('close', () => {
    if (S.picker) $(`#${S.picker.id}`)?.setAttribute('aria-expanded', 'false');
    S.picker = null;
    closeDialog($('#picker'));
  });
  window.addEventListener('resize', placePicker);
  window.visualViewport?.addEventListener('resize', placePicker);
  window.visualViewport?.addEventListener('scroll', placePicker);
  $('#peek-form').addEventListener('submit', sendPeek);
  $('#announcer')?.setAttribute('aria-live', 'polite');
  $('#announcer')?.setAttribute('aria-atomic', 'true');
  setInterval(updateAges, 30000);

  function boot() {
    setConnection('connecting');
    void refresh().then(connectStream);
    void checkSpeech();
    void loadDictation();
  }
  if (typeof navigator !== 'undefined' && 'serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => {});
  function resumeStream() {
    if (S.stream) S.stream.close();
    S.stream = null;
    S.streamOpen = false;
    clearTimeout(S.reconnectTimer);
    S.reconnectTimer = 0;
    S.reconnectDelay = 1000;
    connectStream();
  }
  window.addEventListener('online', resumeStream);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') resumeStream();
  });
  boot();
})();

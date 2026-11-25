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
  sheetCardId: null,
  sheetEngine: '',
  composeMode: null,
  composeTask: 'fix',
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
  contextId: null,
  composeNote: '',
  picker: null,
  dictating: null,
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
  // Every card states its outcome in `status`, which the card head shows as a colored line
  // above the title. A report title still carries the outcome as a prefix. The head drops that
  // prefix. A title then reads the same whichever way the outcome arrived.
  const STATUS_LABELS = {
    complete: ['Complete', 'positive'],
    'step-open': ['Step left open', 'warning'],
    incomplete: ['Incomplete', 'negative'],
    'no-final-report': ['No final report', 'warning'],
    // Cards written before a reply without a status line read as no final report keep their
    // stored word and show the same line.
    'no-status': ['No final report', 'warning'],
    // A plain reply from a session that owes no report, such as an answer to a typed question.
    reply: ['Reply'],
    'plan-ready': ['Plan ready'],
    waiting: ['Waiting on you'],
    notice: ['Notice'],
  };
  const REPORT_STATUS = [
    ['Done: ', 'complete'],
    ['Agent done: ', 'step-open'],
    ['Incomplete: ', 'incomplete'],
    ['Unreported: ', 'no-final-report'],
    ['Plan ready: ', 'plan-ready'],
  ];
  // A card stored before the daemon wrote `status` names its outcome in the title alone. Only
  // a report card is titled that way. A question starting with `Done:` keeps its title. An
  // info card without a transcript ref came from a producer's event instead of a turn. An ask
  // or finding card waits on a decision.
  const legacyStatus = (c) => {
    if (c.kind === 'info') return c.refs?.length ? 'no-final-report' : 'notice';
    if (c.kind === 'plan') return 'plan-ready';
    return c.kind === 'ask' || c.kind === 'finding' ? 'waiting' : 'notice';
  };
  // Every card gets a line. A card can never draw its head without one. An outcome this build
  // does not know falls back to what the card's kind and title say.
  const cardStatus = (c) => {
    const title = String(c.title || ''),
      report = c.kind === 'info' || c.kind === 'plan',
      hit = report && REPORT_STATUS.find(([prefix]) => title.startsWith(prefix)),
      stated = c.status || hit?.[1] || legacyStatus(c),
      key = STATUS_LABELS[stated] ? stated : hit?.[1] || legacyStatus(c),
      [label, tone] = STATUS_LABELS[key] || STATUS_LABELS.notice;
    return { label, tone, title: hit ? heading(c.body) || title.slice(hit[0].length) : titleOf(c) };
  };
  const rowStatus = (c) => {
    const status = cardStatus(c);
    return h('span', { class: 'row-status', 'data-tone': status.tone }, status.label);
  };
  const answerable = (c) => (c.kind !== 'ask' && c.kind !== 'plan') || c.answerable !== false,
    dshOf = (c) => (c.engine === 'dsh' && c.source?.uri?.startsWith('dsh://session/') ? c.source.uri.slice(14) : null),
    ompsdkUri = 'ompsdk://session/',
    ompsdkOf = (c) => (c.engine === 'ompsdk' && c.source?.uri?.startsWith(ompsdkUri) ? c.source.uri.slice(ompsdkUri.length) : null),
    paneOf = (c) => c.source?.pane || (dshOf(c) ? `dsh:${dshOf(c)}` : null) || (ompsdkOf(c) ? `ompsdk:${ompsdkOf(c)}` : null),
    // An ompsdk session has no tmux pane or dsh page, so Peek and Jump are not offered for it.
    shownPane = (c) => (ompsdkOf(c) ? null : paneOf(c)),
    paneApi = (pane) => (pane.startsWith('dsh:') ? `/v1/dsh/${enc(pane.slice(4))}` : `/v1/panes/${enc(pane)}`),
    restorable = (c) => c.kind === 'info' && /^(exited|reload:exited):/.test(c.id),
    // The step a report left open, named by its `Recommended action:` line.
    hasStep = (c) => c.kind === 'info' && Boolean(c.star?.action),
    // A plan card shows its execution fields beside its options, never on a question step.
    planFields = (c) => c.kind === 'plan' && answerable(c) && !c.questions?.length;
  // The three ways a session starts, each with the help line the sheet shows under Mode.
  const MODES = {
    goal: 'The session works on it until it reports done.',
    plan: 'Read-only planning. The plan arrives here for review before anything runs.',
    'plan-into':
      'The planner drafts a plan read-only, the harness approves it without review, and the execution model carries it out at the execution effort chosen below.',
  };
  // dsh has its own help line for each plan mode, because a dsh plan runs in an omp planner.
  const DSH_MODES = {
    plan: 'omp drafts the plan read-only on Opus. The plan arrives here for review. A dsh session carries out the plan you approve.',
    'plan-into': 'omp drafts the plan read-only on Opus. A dsh session then carries it out without asking you.',
  };
  // What a session is asked to do. The daemon keeps each task's direction text. The sheet
  // shows its help line, and the result message names the launch by its verb.
  const TASKS = {
    investigate: { help: 'Read-only. The session checks the facts and reports back.', doing: 'Investigating' },
    fix: { help: 'Finds the root cause, fixes it for good, and checks nothing else broke.', doing: 'Fixing' },
    act: { help: 'Carries out the work with the smallest change that fully solves it.', doing: 'Acting' },
    verify: { help: 'Checks the work is done and right, runs the full test gate, and fixes any gap.', doing: 'Verifying' },
    simplify: { help: 'Makes it smaller and plainer without changing what it does.', doing: 'Simplifying' },
  };
  // Every card action in dock order. Each entry holds its key, its button label, the cards that
  // offer it and what it does. An action without a label has no button. The dock, Enter and the
  // key handler all read this table. A card that offers an action shows one button for it under
  // one key, while a card that does not offer it answers neither. The keys match
  // triage-card-actions in emacs/triage-card.el.
  const ACTIONS = {
    // Plan keeps one shape wherever a card offers it. A report with an open step plans that step
    // in a window of its own when the report came from a tmux pane.
    plan: {
      key: 'p',
      label: (c) => hasStep(c) && 'Plan next step',
      run: (c) => openSheet(c, 'plan', 'act', { newWindow: hasStep(c) }),
    },
    // Dispatch is the sheet's only button and also names the keys that open the sheet with another
    // mode or task.
    dispatch: {
      key: 'd',
      label: 'Dispatch',
      also: ['fix', 'act', 'plan', 'planInto'],
      run: (c) => openSheet(c, null, 'investigate'),
    },
    fix: { key: 'f', run: (c) => openSheet(c, null, 'fix') },
    act: { key: 'A', run: (c) => openSheet(c, null, 'act') },
    planInto: { key: 'P', run: (c) => openSheet(c, 'plan-into', 'act') },
    doNext: {
      key: 'D',
      label: 'Do next step',
      offered: hasStep,
      run: (c) => openSheet(c, 'goal', 'act', { newWindow: true }),
    },
    close: {
      key: 'C',
      label: 'Acknowledge & close',
      offered: paneOf,
      run: (c) => act(c, 'close', { note: noteValue(c) }, 'close'),
    },
    request: {
      key: 'r',
      label: 'Request final report',
      offered: (c) => c.kind === 'info' && c.report_due && paneOf(c),
      run: (c) => act(c, 'report', {}, 'request'),
    },
    restore: {
      key: 'R',
      label: 'Restore run',
      offered: restorable,
      run: (c) => act(c, 'restore', {}, 'restore'),
    },
    cli: { key: 'O', label: 'Open in CLI', offered: ompsdkOf, run: (c) => act(c, 'cli', {}, 'cli') },
    ack: {
      key: 'a',
      label: 'Acknowledge',
      offered: (c) => c.kind === 'info',
      run: (c) => act(c, 'answer', { option: null, text: null, note: noteValue(c) }, 'ack'),
    },
    skip: { key: 's', label: 'Skip', run: (c) => act(c, 'skip', {}, 'skip') },
    context: { key: 't', label: 'Context', run: (c) => openContext(c.id) },
    later: { key: 'l', label: 'Later', run: (c) => act(c, 'later', { minutes: 60 }, 'later') },
    peek: { key: 'v', label: 'Peek', offered: shownPane, run: (c) => openPeek(c) },
    // A dsh session has no tmux pane to jump to. Its card opens the dsh web UI instead.
    jump: {
      key: 'J',
      label: (c) => (dshOf(c) ? 'Open' : 'Jump'),
      offered: shownPane,
      run: (c) => (dshOf(c) ? openDsh() : jump(c)),
    },
    note: { key: 'n', run: () => $('#note')?.focus() },
    other: { key: 'o', offered: (c) => otherAllowed(c), run: (c) => toggleOther(c) },
    copy: { key: 'w', offered: (c) => c.kind === 'info' && Boolean(String(c.body ?? '').trim()), run: (c) => copyReport(c) },
    executor: { key: 'e', offered: planFields, run: () => keyPicker('plan-executor') },
    effort: { key: 'E', offered: (c) => planFields(c) && c.engine === 'dsh', run: () => keyPicker('plan-executor-effort') },
  };
  const offers = (c, id) => !ACTIONS[id].offered || Boolean(ACTIONS[id].offered(c)),
    // The action a key runs on a card. A card that does not offer the action has none.
    actionOf = (c, key) => Object.keys(ACTIONS).find((id) => ACTIONS[id].key === key && offers(c, id)),
    // The label of the button a card shows for an action, or null when it shows none.
    labelOf = (c, id) => {
      const { label } = ACTIONS[id];
      return (offers(c, id) && (typeof label === 'function' ? label(c) : label)) || null;
    },
    // The action a card's dock leads with, which Enter runs as well. A report leads with its
    // open step, else with closing its session, else with a plain acknowledge. A prompt that
    // takes its answer in the pane leads with Peek. Every other card's primary is an option.
    leadOf = (c) =>
      c.kind === 'info'
        ? ['plan', 'close', 'ack'].find((id) => labelOf(c, id))
        : !answerable(c) && offers(c, 'peek')
          ? 'peek'
          : null;
  const current = () => S.cards[S.idx] || null;
  const recIndex = (c) => {
    const i = (c.options || []).findIndex((o) => o.recommended);
    return i < 0 ? 0 : i;
  };
  function optionTask(option) {
    const label = option.label.trimStart().toLowerCase();
    if (!label.startsWith('investigate')) return 'act';
    return label.includes('fix') ? 'fix' : 'investigate';
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
        dispatchMode: null,
        dispatchTask: 'investigate',
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
  function snapshot() {
    const d = S.shownId && S.drafts.get(S.shownId),
      other = $('#other-text'),
      active = document.activeElement,
      inside = Boolean($('.card')?.contains(active));
    if (d && other) d.other = other.value;
    const selection =
      inside && typeof active?.selectionStart === 'number'
        ? [active.selectionStart, active.selectionEnd, active.selectionDirection]
        : null;
    return {
      id: inside ? active.id || null : null,
      selection,
      scroll: $('.card-scroll')?.scrollTop || 0,
      report: $('.report-wrap .body')?.scrollTop || 0,
      inside,
    };
  }
  function restore(v, changed) {
    const scroll = $('.card-scroll'),
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
    const typed = [...S.drafts.values()].some((d) => d.note.trim() || d.other.trim());
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
      fact(
        'Priority',
        p === 5
          ? h('span', { title: 'Priority 5, the lowest' }, 'Reading list')
          : h('span', { title: 'Priority, 1 is most urgent' }, `P${p}`),
      ),
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
      status = cardStatus(c);
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
        button(c, 'copy', 'btn ghost', { 'aria-keyshortcuts': ACTIONS.copy.key }, () => copyReport(c), 'Copy', h('kbd', { class: 'hint', 'aria-hidden': 'true' }, ACTIONS.copy.key)),
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
      executor = draft(c).executor,
      catalog = PICKERS['plan-executor'].data,
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
    // A dsh effort rides on its execution model. The field describes the shown card, whichever
    // card's catalog was read last. It lists no level until this card names a model.
    if (engine === 'dsh')
      Object.assign(PICKERS['plan-executor-effort'], {
        fallback: executor ? 'Planner effort' : 'needs an execution model',
        options: (executor && catalog?.engine === 'dsh' ? effortLevels(catalog, executor) : []).map((level) => pickerOption(level)),
      });
    // An omp Plan Review slider lists roles only, so its role keeps its own thinking level.
    return el(
      'div',
      'plan-executor',
      pair('plan-executor', 'Execution model', executor),
      engine === 'dsh' && pair('plan-executor-effort', 'Execution effort', draft(c).executorThinking),
    );
  }
  // Roles loaded for another harness's plan card are dropped until loadModels refills the list.
  function planExecutorLabels(engine) {
    const cfg = PICKERS['plan-executor'];
    if (cfg.engine !== engine) cfg.options = [];
    cfg.engine = engine;
    cfg.placeholder = engine === 'dsh' ? 'Search models' : 'Search roles';
    cfg.fallback = engine === 'dsh' ? 'current model' : 'omp smol role';
  }

  function otherAllowed(c) {
    const questions = c.questions?.length ? c.questions : null,
      question = questions ? questions[draft(c).step] : null;
    // Other requires a prompt the inbox can answer. A plain finding takes only its listed
    // options. Past the last question the review step takes over.
    if (!answerable(c) || (c.kind === 'finding' && !questions)) return false;
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
      const kids = [
          h('kbd', { class: 'num', 'aria-hidden': 'true' }, i < 9 ? String(i + 1) : '·'),
          el(
            'span',
            'opt-copy',
            el('span', 'opt-label', mdInline(o.label)),
            o.description && el('span', 'opt-desc', mdInline(o.description)),
            o.direction && el('span', 'opt-dir', mdInline(o.direction)),
          ),
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
    if (otherAllowed(c)) rows.push(otherRow(c));
    const out = [
      planFields(c) && planExecutorView(c),
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
            el('strong', null, c.engine === 'dsh' ? 'Answer in the dsh web UI. ' : c.engine === 'ompsdk' ? 'Answer in the CLI. ' : 'Answer in the pane. '),
            'This prompt is read only here.',
          ),
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
    if (c.kind === 'finding') openSheet(c, null, 'act', { answers: d.answers });
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
          h('kbd', { class: 'num', 'aria-hidden': 'true' }, ACTIONS.other.key),
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
  // A single-line field shows the line breaks of a note as spaces.
  function oneLine(text) {
    return text.replace(/\s*\n\s*/g, ' ');
  }
  function noteRow(c) {
    const d = draft(c),
      busy = S.pending?.id === c.id;
    return el(
      'div',
      'note',
      h('label', { for: 'note' }, 'Note', h('kbd', { class: 'hint', 'aria-hidden': 'true' }, ACTIONS.note.key)),
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
          value: oneLine(d.note),
          disabled: busy,
          oninput: (e) => {
            draft(c).note = e.target.value;
          },
        }),
        micButton('note', 'Dictate the note'),
      ),
    );
  }
  // Each label takes the first free letter its words offer, lower case before upper, then a
  // digit. Port of triage-card-choice-keys in emacs/triage-card.el.
  function choiceKeys(labels) {
    const used = new Set();
    return labels.map((label) => {
      const letters = label.split(/[^\p{L}\p{N}]+/u).filter(Boolean).flatMap((word) => [...word].flatMap((ch) => [ch.toLowerCase(), ch.toUpperCase()])),
        key = [...letters, ...'123456789'].find((k) => !used.has(k));
      if (!key) throw new Error(`No key left for choice ${label}`);
      used.add(key);
      return key;
    });
  }
  // The dock draws the card's actions from the table. The lead becomes the primary button. Every
  // other labeled action the card offers follows it.
  function dock(c) {
    const lead = leadOf(c),
      hint = (text) => h('kbd', { class: 'hint', 'aria-hidden': 'true' }, text),
      control = (id) => {
        const { key, run, also = [] } = ACTIONS[id],
          primary = id === lead,
          keys = [key, ...also.filter((other) => !labelOf(c, other)).map((other) => ACTIONS[other].key)];
        return button(
          c,
          id,
          primary ? 'btn primary' : 'btn',
          {
            id: `${id}-btn`,
            'data-primary': primary ? 'true' : null,
            'aria-keyshortcuts': (primary ? [...keys, 'Enter'] : keys).join(' '),
          },
          () => run(c),
          labelOf(c, id),
          hint(key),
        );
      },
      group = Object.keys(ACTIONS).filter((id) => id !== lead && labelOf(c, id)),
      pane = group.filter((id) => id === 'peek' || id === 'jump');
    return el(
      'footer',
      'dock',
      lead && control(lead),
      el(
        'div',
        'dock-group',
        group.filter((id) => !pane.includes(id)).map((id) => control(id)),
        pane.length > 0 && el('div', 'pane-actions', pane.map((id) => control(id))),
      ),
    );
  }
  function cardView(c, changed) {
    const busy = S.pending?.id === c.id,
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
        noteRow(c),
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
        split ? el('div', 'evidence', evidence) : evidence,
        action,
      ),
      dock(c),
    );
  }
  function emptyView() {
    if (S.connection === 'reconnecting' && (S.offline || !S.initialized))
      return h(
        'section',
        { class: 'empty offline', 'aria-labelledby': 'empty-title' },
        h('h2', { id: 'empty-title' }, 'Daemon unreachable'),
        h('p', { class: 'empty-lead', role: 'status' }, 'Retrying automatically.'),
        h('button', { type: 'button', class: 'btn', 'data-retry': true, 'aria-keyshortcuts': 'g' }, 'Retry now', h('kbd', { class: 'hint', 'aria-hidden': 'true' }, 'g')),
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
  function resultMessage(c, verb, body) {
    if (verb === 'skip') return `Skipped · ${c.title}`;
    if (verb === 'later') return `Later 1 hour · ${c.title}`;
    if (verb === 'dispatch' || (verb === 'answer' && body.answers && c.kind === 'finding'))
      return `${
        body.mode === 'plan' || body.mode === 'plan-into' ? 'Planning' : TASKS[body.task]?.doing ?? 'Launching'
      } · ${c.title}`;
    if (verb === 'close') return `Closed · ${c.title}`;
    if (verb === 'report') return `Requested report · ${c.title}`;
    if (verb === 'restore') return `Restoring run · ${c.title}`;
    if (verb === 'cli') return `Opened in CLI · ${c.title}`;
    if (c.kind === 'info') return `Acknowledged · ${c.title}`;
    if (body.answers)
      return `Sent ${body.answers
        .map((answer, i) => answer.text || c.questions?.[i]?.options?.[answer.option]?.label || 'answer')
        .join(', ')} · ${c.title}`;
    return `${c.kind === 'finding' ? 'Launched' : 'Sent'} ${body.text || c.options?.[body.option]?.label || 'Custom answer'} · ${c.title}`;
  }
  async function act(c, verb, body, control) {
    if (Date.now() < S.remoteSwapUntil || S.pending || !S.cards.some((x) => x.id === c.id)) return;
    S.pending = { id: c.id, control };
    S.inflight.add(c.id);
    // The card leaves now; settle reports the daemon's result, which can take seconds of tmux typing.
    const request = api(`/v1/cards/${enc(c.id)}/${verb}`, body);
    request.catch(() => {});
    const dir = verb === 'skip' ? 'left' : verb === 'later' ? 'down' : 'right',
      text = resultMessage(c, verb, body);
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
    if (i === options.length && otherAllowed(c)) return toggleOther(c);
    const option = options[i];
    if (!option || !answerable(c) || c.kind === 'info') return;
    if (c.kind === 'finding') return openSheet(c, null, optionTask(option), { option: i });
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
    if (!otherAllowed(c)) return;
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
  // Enter runs the card's primary. That is the action its dock leads with, else the recommended
  // option.
  function primary(c) {
    if (Date.now() < S.remoteSwapUntil) return;
    const lead = leadOf(c);
    if (lead) ACTIONS[lead].run(c);
    else if (c.questions?.length && answerable(c)) primaryStep(c);
    else pick(c, recIndex(c));
  }
  async function jump(c) {
    const pane = paneOf(c);
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
  // Dictation runs in the browser and the daemon only stores the dictionary. A browser that can
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
    'no-start':
      'The microphone did not start. Please allow the microphone for this site, turn on Dictation and Siri in Settings, then press again.',
    'not-allowed': 'Please allow the microphone for this site to dictate.',
    'service-not-allowed': "Please turn on Dictation and Siri in this device's Settings to dictate.",
  };
  // Safari reports its silence timeout as one of these, which ends a run without failing it.
  const DICTATION_PAUSES = ['aborted', 'no-speech'];
  // A recogniser on iPhone can be left dead: start() returns without an event, so nothing ever ends the run.
  // The first wait is long because the microphone permission question can sit open; later waits are short.
  const DICTATION_FIRST_GRACE = 120000;
  const DICTATION_START_TIMEOUT = 4000;
  const DICTATION_END_TIMEOUT = 1500;
  const DICTATION_PAUSE_TIMEOUT = 2500;
  // The one recogniser this page runs. iPhone asks for the microphone again for a new recogniser and can leave a
  // replaced one dead, so every run reuses it and the page drops it only when it stops answering.
  const speech = {
    recognition: null,
    engine: null,
    granted: false,
    attempts: 0,
    timer: 0,
  };
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
      if (active.id !== id) return;
      // A press while the microphone is listening stops it. A press while it is still starting ends the wait,
      // so a start the browser never finishes cannot swallow every later press.
      if (active.phase === 'listening') stopDictation(active);
      else cancelDictation(active);
      return;
    }
    const field = $(`#${id}`),
      engine = speechEngine();
    if (!field || field.disabled || !engine) return;
    const run = { id, card: field.closest('.card') ? current()?.id : null, phase: 'starting', recognition: null, cancelled: false, stopped: false, announced: false, heard: false, error: '' };
    S.dictating = run;
    paintMics();
    // Safari starts recognition only inside the press that asked for it, so its engine starts before any await.
    if (!engine.local) return startDictation(run, engine);
    try {
      if ((await engine.Recognition.available(speechOptions())) !== 'available') return await installSpeech(engine.Recognition, run);
      if (run.cancelled || S.dictating !== run) return;
      startDictation(run, engine);
    } catch (e) {
      if (S.dictating === run) {
        endDictation();
        toast(`Dictation failed (${e.message}).`, true);
      }
    }
  }
  // One recogniser for the whole page, rebuilt only when its engine changes or it stops answering.
  function speechRecognition(engine) {
    if (speech.recognition && speech.engine === engine.Recognition) return speech.recognition;
    const recognition = new engine.Recognition();
    Object.assign(recognition, { lang: speechLang(), interimResults: false, continuous: true });
    if (engine.local) recognition.processLocally = true;
    recognition.onstart = () => speechStarted(recognition);
    recognition.onaudiostart = () => speechStarted(recognition);
    recognition.onresult = (e) => speechResult(recognition, e);
    recognition.onerror = (e) => speechError(recognition, e);
    recognition.onend = () => speechEnded(recognition);
    speech.recognition = recognition;
    speech.engine = engine.Recognition;
    return recognition;
  }
  function clearSpeechTimer() {
    if (!speech.timer) return;
    clearTimeout(speech.timer);
    speech.timer = 0;
  }
  function startDictation(run, engine) {
    try {
      const recognition = speechRecognition(engine);
      run.recognition = recognition;
      run.heard = false;
      speech.attempts += 1;
      // The wait is armed before start() so an engine that reports starting synchronously still clears it.
      watchSpeech(run, recognition);
      recognition.start();
    } catch (e) {
      speech.recognition = null;
      speech.engine = null;
      clearSpeechTimer();
      endDictation();
      toast(`Dictation failed (${e.message}).`, true);
      return;
    }
    paintMics();
    if (!run.announced) {
      run.announced = true;
      announce('The microphone is listening. Please press it again to stop.');
    }
  }
  // A recogniser that reports nothing after start() is dead on iPhone. It ends the run and is dropped so the next
  // press builds a fresh one, instead of leaving a microphone that looks live and answers no press.
  function watchSpeech(run, recognition) {
    clearSpeechTimer();
    speech.timer = setTimeout(
      () => {
        speech.timer = 0;
        if (S.dictating !== run || run.recognition !== recognition) return;
        speech.recognition = null;
        speech.engine = null;
        run.error = 'no-start';
        finishDictation(run);
      },
      // Only the first press can be waiting on a permission question; later presses say so quickly.
      speech.attempts > 1 ? DICTATION_START_TIMEOUT : DICTATION_FIRST_GRACE,
    );
  }
  function speechStarted(recognition) {
    speech.granted = true;
    const run = S.dictating;
    if (!run || run.recognition !== recognition) return;
    clearSpeechTimer();
    if (run.phase === 'listening') return;
    run.phase = 'listening';
    paintMics();
  }
  function speechResult(recognition, e) {
    const run = S.dictating;
    if (!run || run.recognition !== recognition) return;
    speechStarted(recognition);
    // The result list is array-like, not iterable, in Safari and in Chromium, so it is read by index.
    // A browser that hands back something else ends the run with its message instead of a live microphone.
    try {
      const words = readResults(e)
        .map((result) => result[0].transcript.trim())
        .filter(Boolean);
      if (!words.length || run.cancelled) return;
      if (placeDictation(run, applyDictionary(words.join(' '), S.dictionary))) run.heard = true;
      else cancelDictation(run);
    } catch (err) {
      run.error = err.message;
      finishDictation(run);
      try {
        recognition.abort();
      } catch {}
    }
  }
  function speechError(recognition, e) {
    const run = S.dictating;
    if (!run || run.recognition !== recognition) return;
    // An error is an answer from the recogniser, so this page is no longer waiting on a silent start.
    speech.granted = true;
    clearSpeechTimer();
    run.error = e.error;
    // A real failure ends the run now. Safari sends this without a following onend in some versions.
    if (!DICTATION_PAUSES.includes(e.error)) return finishDictation(run);
    // A pause ends the run itself; only a browser that never ends it needs the timer.
    speech.timer = setTimeout(() => finishDictation(run), DICTATION_PAUSE_TIMEOUT);
  }
  function speechEnded(recognition) {
    const run = S.dictating;
    if (run?.recognition === recognition) finishDictation(run);
  }
  // The final results from `resultIndex` on, read by index because the list has no iterator.
  function readResults(e) {
    const results = e.results,
      finals = [];
    for (let i = Number(e.resultIndex) || 0; i < Number(results.length); i += 1) {
      const result = results[i];
      if (result?.isFinal && result[0]) finals.push(result);
    }
    return finals;
  }
  function stopDictation(run) {
    run.stopped = true;
    clearSpeechTimer();
    try {
      run.recognition?.stop();
    } catch {
      speech.recognition = null;
      speech.engine = null;
      return finishDictation(run);
    }
    // A stop the browser never answers would leave the button dead, so the run ends anyway and a fresh
    // recogniser takes the next press.
    speech.timer = setTimeout(() => {
      speech.timer = 0;
      if (S.dictating !== run) return;
      speech.recognition = null;
      speech.engine = null;
      finishDictation(run);
    }, DICTATION_END_TIMEOUT);
  }
  function cancelDictation(run) {
    run.cancelled = true;
    clearSpeechTimer();
    try {
      run.recognition?.abort();
    } catch {}
    if (S.dictating === run) endDictation();
  }
  function finishDictation(run) {
    clearSpeechTimer();
    if (S.dictating === run) endDictation();
    const error = run.error;
    if (!run.cancelled && !run.stopped && error) toast(DICTATION_ERRORS[error] ?? `Dictation stopped (${error}).`, true);
  }
  // The first press on each site installs the browser's speech model. Press again to dictate.
  async function installSpeech(Recognition, run) {
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
      if (S.dictating === run) endDictation();
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
  // Matches whole words and ignores letter case. One pass means replaced text never matches again. The letter
  // before a match comes back as a capture group because Safari before 16.4 rejects a lookbehind at construction.
  function applyDictionary(text, entries) {
    if (!entries.length) return text;
    const heard = entries.map(([form]) => form.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|'),
      written = new Map(entries.map(([form, word]) => [form.toLowerCase(), word]));
    return text.replace(
      new RegExp(`(^|[^\\p{L}\\p{N}])(?:${heard})(?![\\p{L}\\p{N}])`, 'giu'),
      (match, before) => {
        const form = match.slice(before.length);
        return before + (written.get(form.toLowerCase()) ?? form);
      },
    );
  }
  async function checkSpeech() {
    const engine = speechEngine();
    let status = window.isSecureContext ? 'unavailable' : 'insecure';
    if (engine && !engine.local) status = 'online';
    else if (engine)
      try {
        status = (await engine.Recognition.available(speechOptions())) ?? status;
      } catch {}
    if (!Object.prototype.hasOwnProperty.call(DICTATION_STATUS, status)) status = 'unavailable';
    document.documentElement.dataset.dictation = DICTATION_READY.includes(status) ? 'on' : 'off';
    $('#dictation-status').textContent = DICTATION_STATUS[status];
  }
  async function loadDictation() {
    try {
      const data = await api('/v1/dictation');
      $('#dictation-dictionary').value = data.dictionary;
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
      await api('/v1/dictation', { dictionary });
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
    openSheet(null, S.composeMode, S.composeTask);
  }
  // The session sheet, Plan approval and the settings fields share the picker.
  const PICKERS = {
    'compose-cwd': { title: 'Working directory', placeholder: 'Search or type a path', free: true, fallback: 'Home directory', options: [] },
    'compose-session': { title: 'tmux session', placeholder: 'Search sessions', free: false, fallback: 'Daemon default', options: [] },
    'compose-engine': { title: 'Engine', placeholder: 'Search engines', free: false, fallback: 'Daemon default', options: [pickerOption('omp'), pickerOption('dsh'), pickerOption('ompsdk')] },
    'compose-model': { title: 'Model', placeholder: 'Search or type a selector', free: true, fallback: 'Daemon default', options: [] },
    'compose-thinking': { title: 'Thinking', placeholder: 'Search levels', free: false, fallback: 'Daemon default', options: [] },
    'compose-executor': { title: 'Execution model', placeholder: 'Search or type a selector', free: true, fallback: 'omp smol role', options: [] },
    'compose-executor-effort': { title: 'Execution effort', placeholder: 'Search levels', free: false, fallback: 'Planner effort', options: [] },
    'plan-executor': { title: 'Execution model', placeholder: 'Search roles', free: false, fallback: 'omp smol role', options: [] },
    'plan-executor-effort': { title: 'Execution effort', placeholder: 'Search levels', free: false, fallback: 'Planner effort', options: [] },
  };
  const MODEL_PICKERS = [
    'compose-model',
    'compose-executor',
    'compose-executor-effort',
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
  // A picker opened by its key hands focus to `back` when it closes, or to the card's primary
  // button. The next key then reaches the sheet or the card instead of typing into the field.
  function keyPicker(id, back = null) {
    openPicker(id);
    S.dialogFocus.set($('#picker'), back);
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
    if (p.id === 'compose-engine') {
      S.sheetEngine = row.value;
      const c = sheetCard();
      setSheetMode(c ? draft(c).dispatchMode : S.composeMode, c);
    }
    if (p.id === 'plan-executor' && current()?.kind === 'plan') draft(current()).executor = row.value;
    if (p.id === 'plan-executor-effort' && current()?.kind === 'plan') draft(current()).executorThinking = row.value;
    $('#picker').close();
    if (changed) clearModels(p.id.replace(/-engine$/, ''));
    else if (/-model$|-executor$/.test(p.id)) refreshEfforts(sheetOf(p.id));
  }
  // Model selectors belong to one engine. A new engine clears its sheet's model and executor,
  // and the catalog that stood behind them, so the next picker never offers another engine's
  // models while the new catalog is still being read.
  function clearModels(sheet) {
    setPickerValue(`${sheet}-model`, '');
    setPickerValue(`${sheet}-executor`, '');
    setPickerValue(`${sheet}-executor-effort`, '');
    const model = PICKERS[`${sheet}-model`];
    for (const id of ['model', 'executor', 'executor-effort']) {
      PICKERS[`${sheet}-${id}`].options = [];
      refreshPicker(`${sheet}-${id}`);
    }
    model.data = null;
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
    return ['compose', 'plan'].find((sheet) => id.startsWith(`${sheet}-`)) || null;
  }
  // Each sheet lists the models of the engine its Engine field picks, or the daemon's when Default.
  // Its model field keeps the catalog data, so an effort list can follow the model picked. A
  // catalog read can take seconds, most of all a dsh bridge that is timing out, so the newest read
  // owns the pickers: an older one answering later must not put its engine's models under a field
  // that now names another engine.
  let modelLoad = 0;
  async function loadModels() {
    const run = ++modelLoad;
    try {
      const base = await api('/v1/models');
      if (run !== modelLoad) return;
      const daemon = base.engine || 'omp',
        lists = { [daemon]: base },
        load = async (engine) => (lists[engine] ??= await api(`/v1/models?engine=${engine}`)),
        options = (list) =>
          (list || []).map(({ selector, name }) => pickerOption(selector, name, name === selector ? null : selector));
      const engine = $('#compose-engine')?.value || daemon,
        data = await load(engine);
      if (run !== modelLoad) return;
      const model = PICKERS['compose-model'],
        thinking = PICKERS['compose-thinking'],
        executor = PICKERS['compose-executor'];
      model.data = data;
      if ($('#compose')?.open) {
        const c = sheetCard();
        setSheetMode(c ? draft(c).dispatchMode : S.composeMode, c);
      }
      PICKERS['compose-engine'].fallback = daemon;
      model.options = executor.options = options(data.models);
      model.fallback = data.default
        ? model.options.find((option) => option.value === data.default)?.label || data.default
        : `${engine} default`;
      thinking.fallback = data.default_thinking ?? `${engine} default`;
      executor.fallback = engine === 'dsh' ? 'planner model' : 'omp smol role';
      $('#compose-session-pair').hidden = engine === 'dsh' || engine === 'ompsdk';
      refreshEfforts('compose');
      for (const id of ['engine', 'model', 'thinking', 'executor', 'executor-effort'])
        refreshPicker(`compose-${id}`);
      // A plan card from the other harness offers that harness's execution models.
      const plan = current(),
        planEngine = plan?.kind === 'plan' ? plan.engine || 'omp' : daemon;
      planExecutorLabels(planEngine);
      const planModels = await load(planEngine);
      if (run !== modelLoad) return;
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
  // An image attached in the session sheet. It is read as base64 and sent with the prompt or the
  // dispatch; the daemon stores one copy for both engines.
  const IMAGE_TYPES = new Set(['image/png', 'image/jpeg', 'image/webp', 'image/gif']),
    IMAGE_LIMIT = 10 * 1024 * 1024;
  let sheetImage = null;

  function showSheetImage() {
    const preview = $('#compose-image-preview'),
      thumb = $('#compose-image-thumb'),
      name = $('#compose-image-name');
    if (!preview || !thumb) return;
    if (sheetImage?.url) URL.revokeObjectURL(sheetImage.url);
    if (sheetImage) sheetImage.url = null;
    if (!sheetImage?.file) {
      preview.hidden = true;
      thumb.removeAttribute('src');
      name.textContent = '';
      return;
    }
    sheetImage.url = URL.createObjectURL(sheetImage.file);
    thumb.src = sheetImage.url;
    name.textContent = sheetImage.file.name;
    preview.hidden = false;
  }
  function attachSheetImage(file) {
    const hint = $('#compose-hint');
    if (!file) return;
    if (!IMAGE_TYPES.has(file.type)) {
      hint.textContent = 'Attach a PNG, JPEG, WebP or GIF image.';
      return;
    }
    if (file.size > IMAGE_LIMIT) {
      hint.textContent = 'That image is over 10 MB.';
      return;
    }
    sheetImage = { file, url: null };
    showSheetImage();
    hint.textContent = '';
  }
  function clearSheetImage() {
    if (sheetImage?.url) URL.revokeObjectURL(sheetImage.url);
    sheetImage = null;
    const input = $('#compose-image');
    if (input) input.value = '';
    showSheetImage();
  }
  function sheetImageBody() {
    const file = sheetImage?.file;
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
  function wireSheetImage() {
    const input = $('#compose-image'),
      add = $('#compose-image-add'),
      remove = $('#compose-image-remove');
    if (!input || !add) return;
    add.addEventListener('click', () => input.click());
    input.addEventListener('change', () => attachSheetImage(input.files?.[0]));
    remove?.addEventListener('click', () => clearSheetImage());
    // The whole sheet takes a pasted image, because a card's sheet opens on its options.
    $('#compose')?.addEventListener('paste', (e) => {
      const file = [...(e.clipboardData?.files || [])].find((f) => IMAGE_TYPES.has(f.type));
      if (file && !e.clipboardData.getData('text')) {
        e.preventDefault();
        attachSheetImage(file);
      }
    });
  }

  // One submit for the one sheet. Without a card it starts a session and closes at once. With a
  // card it dispatches the brief and keeps the sheet open until the daemon answers.
  async function submitSheet(e) {
    e.preventDefault();
    const start = $('#compose-start'),
      hint = $('#compose-hint'),
      id = S.sheetCardId,
      c = sheetCard();
    if (start.disabled) return;
    if (id) {
      if (!c) {
        hint.textContent = 'This card is no longer in the queue.';
        return;
      }
      return dispatchCard(c, start, hint);
    }
    return startSession(start, hint);
  }

  function sheetMode(mode) {
    return mode || PICKERS['compose-model'].data?.default_mode || 'goal';
  }

  async function startSession(start, hint) {
    const prompt = $('#compose-prompt').value.trim(),
      cwd = $('#compose-cwd').value.trim(),
      mode = sheetMode(S.composeMode);
    if (!prompt) return;
    // Disable before reading an attached image: a second submit during that read would
    // otherwise start the same session twice.
    start.disabled = true;
    let image;
    try {
      image = await sheetImageBody();
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
      mode,
      task: S.composeTask,
    };
    if (image) body.image = image;
    if (mode === 'plan-into') {
      body.executor = $('#compose-executor').value.trim() || null;
      body.executor_thinking = $('#compose-executor-effort').value.trim() || null;
    }
    // The daemon does the launch, which can take it a while. The sheet closes now and the
    // toast reports the result. A busy host never holds the inbox.
    hint.textContent = '';
    S.composeNote = '';
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
      S.composeNote ||= prompt;
      if (!$('#compose-prompt').value.trim()) $('#compose-prompt').value = prompt;
      hint.textContent = message;
      if (!$('dialog[open]')) showDialog($('#compose'));
      toast(message, true);
    } finally {
      pending?.remove();
      start.disabled = false;
    }
  }

  // A card dispatch keeps the sheet open until the daemon answers, so a refusal leaves the
  // instruction in place for a retry.
  async function dispatchCard(c, start, hint) {
    if (!S.cards.some((x) => x.id === c.id)) {
      hint.textContent = 'This card is no longer in the queue.';
      return;
    }
    const d = draft(c),
      mode = sheetMode(d.dispatchMode);
    d.note = $('#compose-prompt').value;
    const launch = {
      mode,
      task: d.dispatchTask,
      note: noteValue(c),
      engine: $('#compose-engine').value || null,
      model: $('#compose-model').value.trim() || null,
      thinking: $('#compose-thinking').value.trim() || null,
    };
    let image;
    try {
      image = await sheetImageBody();
    } catch (err) {
      hint.textContent = err.message;
      return;
    }
    start.disabled = true;
    hint.textContent = 'Launching…';
    if (image) launch.image = image;
    if (mode === 'plan-into') {
      launch.executor = $('#compose-executor').value.trim() || null;
      launch.executor_thinking = $('#compose-executor-effort').value.trim() || null;
    }
    // A finding launches through its answer, which names the option or the answers it chose.
    const ok = d.dispatchAnswers
      ? await act(c, 'answer', { answers: d.dispatchAnswers, ...launch }, 'send')
      : d.dispatchOption != null
        ? await act(c, 'answer', { option: d.dispatchOption, text: null, ...launch }, `option-${d.dispatchOption}`)
        : await act(c, 'dispatch', { ...launch, new_window: Boolean(d.dispatchWindow) }, 'dispatch');
    start.disabled = false;
    if (ok || !S.cards.some((x) => x.id === c.id)) {
      hint.textContent = '';
      $('#compose').close();
    } else if (hint.textContent === 'Launching…')
      hint.textContent = 'Could not dispatch.';
  }

  function selectSegment(group, value) {
    for (const b of group.children) {
      const on = b.dataset.value === value;
      b.setAttribute('aria-checked', String(on));
      b.tabIndex = on ? 0 : -1;
    }
  }
  // The keys of a radio group's shown buttons, mnemonic as in the Emacs launch sheet. Each
  // button prints its key, which shows while the group holds focus.
  function segmentKeys(group) {
    const bs = [...group.children].filter((b) => !b.hidden),
      keys = choiceKeys(bs.map((b) => b.firstChild.nodeValue.trim()));
    bs.forEach((b, i) => {
      b.querySelector('kbd').textContent = keys[i];
      b.setAttribute('aria-keyshortcuts', keys[i]);
    });
    return new Map(keys.map((key, i) => [key, bs[i]]));
  }
  // The card the sheet holds, or null for a plain session.
  function sheetCard() {
    return S.sheetCardId ? S.cards.find((x) => x.id === S.sheetCardId) || null : null;
  }
  // The tasks the sheet offers. A finding's answers already decide, question by question, what to
  // investigate, so its sheet drops Investigate.
  function sheetTasks(c = sheetCard()) {
    const tasks = Object.keys(TASKS);
    return c && draft(c).dispatchAnswers ? tasks.filter((task) => task !== 'investigate') : tasks;
  }
  // Mode says how the session starts. Start sends the mode the sheet shows.
  // An unpicked mode follows the engine's default. A picked mode sticks.
  function setSheetMode(mode, c = sheetCard()) {
    if (!Object.hasOwn(MODES, mode)) mode = null;
    // dsh displays omp help and fields, which the daemon ignores.
    const dsh = $('#compose-engine').value === 'dsh';
    if (c) draft(c).dispatchMode = mode;
    else S.composeMode = mode;
    const shown = sheetMode(mode);
    selectSegment($('#compose-mode'), shown);
    $('#compose-mode-help').textContent = (dsh && DSH_MODES[shown]) || MODES[shown];
    // dsh has no execution fields, because the sheet's Model and Thinking carry the plan out.
    $('#compose-models').hidden = dsh || shown !== 'plan-into';
  }
  // Task says what the session is asked to do. A task the context does not offer falls back to Act.
  function setSheetTask(task, c = sheetCard()) {
    const tasks = sheetTasks(c);
    if (!tasks.includes(task)) task = 'act';
    if (c) draft(c).dispatchTask = task;
    else S.composeTask = task;
    for (const b of $('#compose-task').children) b.hidden = !tasks.includes(b.dataset.value);
    selectSegment($('#compose-task'), task);
    $('#compose-task-help').textContent = TASKS[task].help;
  }
  // Opens the session sheet. With a card it is that card's dispatch, and the context strip names
  // the card until Clear drops it. Without one it starts a plain session. `newWindow` asks for a
  // tmux window of its own instead of the pane that just reported. The next step then never flips
  // a finished omp session into another mode. The daemon ignores it for a dsh card, whose brief
  // continues the card's session.
  function openSheet(c, mode, task, { newWindow = false, option = null, answers = null } = {}) {
    if (c && (S.pending || !c.id || Date.now() < S.remoteSwapUntil)) return;
    const field = $('#compose-engine');
    S.sheetCardId = c ? c.id : null;
    if (c) {
      const d = draft(c),
        // A card with a session of its own preselects that session's engine, which lets the brief
        // continue there. Every other card keeps the engine last picked in the sheet.
        engine = dshOf(c) ? 'dsh' : ompsdkOf(c) ? 'ompsdk' : c.source?.pane ? 'omp' : S.sheetEngine,
        // Default names the daemon's engine. A move between those two keeps the model list.
        daemon = PICKERS['compose-engine'].fallback,
        same = (field.value || daemon) === (engine || daemon);
      d.dispatchOption = option;
      d.dispatchAnswers = answers;
      d.dispatchWindow = newWindow;
      if (field.value !== engine) setPickerValue('compose-engine', engine);
      if (!same) clearModels('compose');
      else if (!PICKERS['compose-model'].options.length) void loadModels();
      $('#compose-prompt').value = d.note;
    } else {
      // A plain session keeps the engine last picked in the sheet, not the engine of the card
      // whose dispatch came before it.
      const daemon = PICKERS['compose-engine'].fallback,
        same = (field.value || daemon) === (S.sheetEngine || daemon);
      if (field.value !== S.sheetEngine) setPickerValue('compose-engine', S.sheetEngine);
      if (!same) clearModels('compose');
      $('#compose-prompt').value = S.composeNote;
      void loadComposeSessions();
    }
    $('#compose-context').hidden = !c;
    $('#compose-context-title').textContent = c ? titleOf(c) : '';
    $('#compose-context-source').textContent = c ? sourceName(c.source || {}) : '';
    $('#compose-prompt-label').textContent = c ? 'Instruction' : 'What should the session do?';
    $('#compose-prompt').placeholder = c
      ? 'Optional direction for the session'
      : 'Ask a question, give a task, or start a chat';
    $('#compose-target').hidden = Boolean(c);
    $('#compose-session-pair').hidden = ['dsh', 'ompsdk'].includes(field.value || PICKERS['compose-engine'].fallback);
    setSheetMode(mode, c);
    setSheetTask(task, c);
    $('#compose-hint').textContent = '';
    $('#compose-start').disabled = false;
    showDialog($('#compose'));
    // A card's sheet opens on its options and a new session in its text, as in Emacs.
    $(c ? '#compose-form' : '#compose-prompt').focus({ preventScroll: true });
    void loadModels();
  }
  // Clear drops the card and leaves a plain new session. The card's draft stays where it was.
  function clearSheetCard() {
    const field = $('#compose-engine'),
      daemon = PICKERS['compose-engine'].fallback,
      same = (field.value || daemon) === (S.sheetEngine || daemon);
    S.sheetCardId = null;
    $('#compose-context').hidden = true;
    $('#compose-context-title').textContent = '';
    $('#compose-context-source').textContent = '';
    $('#compose-prompt-label').textContent = 'What should the session do?';
    $('#compose-prompt').placeholder = 'Ask a question, give a task, or start a chat';
    $('#compose-prompt').value = S.composeNote;
    $('#compose-target').hidden = false;
    if (field.value !== S.sheetEngine) setPickerValue('compose-engine', S.sheetEngine);
    if (!same) clearModels('compose');
    $('#compose-session-pair').hidden = ['dsh', 'ompsdk'].includes(field.value || daemon);
    setSheetMode(S.composeMode, null);
    setSheetTask(S.composeTask, null);
    $('#compose-hint').textContent = '';
    $('#compose-start').disabled = false;
    void loadComposeSessions();
    $('#compose-prompt').focus({ preventScroll: true });
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
        rowStatus(c),
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
              'aria-keyshortcuts': 'r',
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
    S.contextId = id;
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
        rowStatus(c),
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
  document.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey || e.isComposing) return;
    const t = e.target,
      // Peek, History and Context take the keys of their Emacs buffers. Context opens above History.
      view = ['context', 'history', 'peek'].map((id) => $(`#${id}`)).find((d) => d?.open);
    if (view) {
      if (textField(t)) return;
      const shown = current(),
        row = t.closest?.('.history-item'),
        fn =
          {
            peek: {
              f: togglePeekMode,
              g: () => void refreshPeek(),
              s: () => $('#peek-input').focus(),
              J: () => shown && paneOf(shown) === S.peekPane && ACTIONS.jump.run(shown),
            },
            history: {
              g: openHistory,
              t: () => row && $('.h-context', row)?.click(),
              r: () => row && $('.h-reload', row)?.click(),
            },
            context: { g: () => openContext(S.contextId) },
          }[view.id][e.key] || (e.key === 'q' && (() => view.close()));
      if (fn) {
        e.preventDefault();
        fn();
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
    if (key === 'Escape' && S.countsExpanded)
      return run(() => {
        closeCounts();
        $('#count-toggle')?.focus({ preventScroll: true });
      });
    if (key === '?') return run(() => showDialog($('#keys')));
    if (key === 'h') return run(openHistory);
    if (key === 'c') return run(openCompose);
    if (key === 'S') return run(openSettings);
    if (key === 'g') return run(retryNow);
    const c = current();
    if (!c) return;
    if (/^[1-9]$/.test(key)) return run(() => pick(c, Number(key) - 1));
    if (t.closest?.('.report-wrap .body') && ['ArrowUp', 'ArrowDown'].includes(key)) return;
    if (key === 'Enter') {
      if (!t.closest?.('button,a,select')) run(() => primary(c));
      return;
    }
    const id = actionOf(c, key);
    if (id) return run(() => ACTIONS[id].run(c));
    switch (key) {
      case 'Escape':
        if (otherAllowed(c) && draft(c).open) toggleOther(c);
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
    const mic = e.target.closest?.('[data-dictate]');
    if (mic) void toggleDictation(mic.dataset.dictate);
  });
  document.addEventListener('focusin', (e) => {
    if (S.countsExpanded && !e.target.closest?.('#count-toggle, #counts')) closeCounts();
  });
  // Closing the sheet that holds the field being dictated stops listening and drops the words.
  document.addEventListener(
    'close',
    (e) => {
      const run = S.dictating;
      if (!run || !e.target.contains($(`#${run.id}`))) return;
      cancelDictation(run);
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
  window
    .matchMedia?.('(prefers-color-scheme: dark)')
    .addEventListener('change', (e) => applyTheme(e.matches ? 'dark' : 'light'));
  $('#btn-theme')?.addEventListener('click', () =>
    applyTheme(document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'),
  );
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
    S.sheetCardId = null;
    clearSheetImage();
    closeDialog($('#compose'));
  });
  $('#compose-context-clear').addEventListener('click', clearSheetCard);
  $('#compose-form').addEventListener('submit', submitSheet);
  $('#compose-prompt').addEventListener('input', (e) => {
    const c = sheetCard();
    if (!c) {
      S.composeNote = e.target.value;
      return;
    }
    const d = draft(c);
    d.note = e.target.value;
    // The card's Note field shows the same note.
    if (S.shownId === c.id && $('#note')) $('#note').value = oneLine(d.note);
  });
  wireSheetImage();
  // Mode and Task are radio groups. A click, an arrow key or the key each button shows picks it. A
  // picked key hands focus back to the sheet's options, the way an Emacs choice prompt does.
  for (const [group, pick] of [
    [$('#compose-mode'), (value) => setSheetMode(value)],
    [$('#compose-task'), (value) => setSheetTask(value)],
  ]) {
    group.addEventListener('click', (e) => {
      const b = e.target.closest('[data-value]');
      if (b) pick(b.dataset.value);
    });
    group.addEventListener('focusin', () => segmentKeys(group));
    group.addEventListener('keydown', (e) => {
      const bs = [...group.children].filter((b) => !b.hidden),
        i = bs.findIndex((b) => b.getAttribute('aria-checked') === 'true'),
        step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key],
        keyed = !e.ctrlKey && !e.metaKey && !e.altKey && segmentKeys(group).get(e.key);
      if (keyed) {
        e.preventDefault();
        pick(keyed.dataset.value);
        $('#compose-form').focus();
        return;
      }
      if (!step) return;
      e.preventDefault();
      const next = bs[(i + step + bs.length) % bs.length];
      pick(next.dataset.value);
      next.focus();
    });
  }
  // Outside a text field the sheet takes the option keys of the Emacs launch sheet. m and T move
  // into Mode and Task, the picker keys open their picker, a attaches an image and D drops it.
  // q closes the sheet and Tab moves from the options into the text. Ctrl+Enter starts the
  // session. Enter alone never does, as RET never launches in Emacs.
  const SHEET_KEYS = {
    m: 'compose-mode',
    T: 'compose-task',
    d: 'compose-cwd',
    s: 'compose-session',
    e: 'compose-engine',
    M: 'compose-model',
    t: 'compose-thinking',
    x: 'compose-executor',
    X: 'compose-executor-effort',
    a: 'compose-image-add',
    D: 'compose-image-remove',
  };
  $('#compose').addEventListener('keydown', (e) => {
    if (e.isComposing || e.defaultPrevented) return;
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      $('#compose-form').requestSubmit();
      return;
    }
    if (e.ctrlKey || e.metaKey || e.altKey || textField(e.target)) return;
    if (e.target.closest?.('.picker-field')) return;
    const form = $('#compose-form'),
      id = SHEET_KEYS[e.key],
      field = id && $(`#${id}`);
    if (e.key === 'Tab' && !e.shiftKey && e.target === form) {
      e.preventDefault();
      $('#compose-prompt').focus();
    } else if (e.key === 'q') {
      e.preventDefault();
      $('#compose').close();
    } else if (field && !field.closest('[hidden]')) {
      e.preventDefault();
      if (PICKERS[id]) keyPicker(id, form);
      else if (field.matches('[role="radiogroup"]')) field.querySelector('[aria-checked="true"]').focus();
      else field.click();
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
  // The on-screen keyboard shrinks the visual viewport without resizing the page. A sheet sized to
  // the page keeps its bottom rows behind the keyboard. Mirror the shorter viewport into CSS so an
  // open sheet and the card dock end above the keyboard.
  function syncViewport() {
    const vv = window.visualViewport,
      style = document.documentElement.style,
      compact = Boolean(vv && vv.scale < 1.05 && vv.height > 0 && vv.height < innerHeight * 0.75),
      height = compact ? vv.height : innerHeight,
      top = compact ? vv.offsetTop : 0;
    if (!height) return;
    style.setProperty('--vvh', `${Math.round(height)}px`);
    style.setProperty('--vvt', `${Math.round(top)}px`);
    document.documentElement.classList.toggle('compact-viewport', compact);
  }
  window.addEventListener('resize', syncViewport);
  window.visualViewport?.addEventListener('resize', syncViewport);
  window.visualViewport?.addEventListener('scroll', syncViewport);
  syncViewport();
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
    // WebKit refuses speech recognition while the page is not visible, so a hidden inbox drops the run instead
    // of leaving a microphone that can never come back.
    else if (S.dictating) cancelDictation(S.dictating);
  });
  boot();
})();

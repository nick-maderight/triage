const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const historyStart = app.indexOf('  function historyDayStart() {');
const historyEnd = app.indexOf('  // render', historyStart);
const drawerStart = app.indexOf('  function historyItem(row) {');
const drawerEnd = app.indexOf('  // keys', drawerStart);
assert.ok(historyStart >= 0 && historyEnd > historyStart && drawerEnd > drawerStart);
const historyFunctions = `${app.slice(historyStart, historyEnd)}\n${app.slice(drawerStart, drawerEnd)}`;

const localDayStart = (ms) => {
  const date = new Date(ms);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
};

function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function makeNode(tag, props = {}, ...children) {
  return {
    tag,
    props: typeof props === 'string' ? { class: props } : props,
    children: children.flat(Infinity).filter((child) => child != null && child !== false && child !== ''),
  };
}

const textOf = (node) =>
  typeof node === 'string' || typeof node === 'number'
    ? String(node)
    : node?.children?.map(textOf).join('') || '';

function makeHistoryHarness(now) {
  const state = {
      cards: [],
      initialized: true,
      historyPromise: null,
      historyPromiseDate: null,
      history: { loading: false, loaded: false, dayStart: null, error: null, today: 0, lastAt: null },
    },
    requests = [],
    requestWaiters = [],
    renderWaiters = [],
    listWaiters = [];

  class LocalDate extends Date {
    constructor(...args) {
      super(...(args.length ? args : [now]));
    }

    static now() {
      return now;
    }
  }

  function notify(waiters) {
    for (let i = waiters.length - 1; i >= 0; i--) {
      if (waiters[i].predicate()) waiters.splice(i, 1)[0].resolve();
    }
  }

  const list = {
    children: [],
    textContent: '',
    updates: 0,
    replaceChildren(...children) {
      this.children = children;
      this.textContent = children.map(textOf).join('');
      this.updates++;
      notify(listWaiters);
    },
  };
  const dialog = { open: false };
  const api = (url) => {
    const pending = deferred(),
      request = { url: new URL(url, 'http://triage.test'), resolve: pending.resolve };
    requests.push(request);
    notify(requestWaiters);
    return pending.promise;
  };
  const context = vm.createContext({
    S: state,
    Date: LocalDate,
    URLSearchParams,
    api,
    stamp: (value) => (value == null || value === '' || !Number.isFinite(Number(value)) ? null : Number(value)),
    render: () => notify(renderWaiters),
    document: { querySelectorAll: () => [] },
    age: () => 'now',
    el: (tag, props, ...children) => makeNode(tag, props || {}, ...children),
    h: makeNode,
    chip: (kind) => makeNode('span', `chip chip-${kind}`, kind),
    sourceName: (source) => source.label || source.uri || 'unknown',
    clock: (ms) => String(ms),
    showDialog() {},
    $: (selector) => (selector === '#history-list' ? list : dialog),
  });

  vm.runInContext(
    `${historyFunctions}\nObject.assign(globalThis, { historyRows, emptyHistory, openHistory, updateAges });`,
    context,
  );

  const watch = (waiters, predicate) => {
    if (predicate()) return Promise.resolve();
    return new Promise((resolve) => waiters.push({ predicate, resolve }));
  };

  return {
    context,
    state,
    list,
    requests,
    setNow(value) {
      now = value;
    },
    waitForRequest(count) {
      return watch(requestWaiters, () => requests.length >= count).then(() => requests[count - 1]);
    },
    waitForRender(predicate) {
      return watch(renderWaiters, predicate);
    },
    waitForList(predicate) {
      return watch(listWaiters, predicate);
    },
    waitForListUpdate(after) {
      return watch(listWaiters, () => list.updates > after);
    },
  };
}

function localDayEnd(ms) {
  const date = new Date(localDayStart(ms));
  date.setDate(date.getDate() + 1);
  return date.getTime();
}

function assertDayBounds(request, date) {
  const params = request.url.searchParams;
  assert.equal(request.url.pathname, '/v1/history');
  assert.deepEqual([...params.keys()].sort(), ['limit', 'today_end_ms', 'today_start_ms']);
  assert.equal(params.get('limit'), '50');
  assert.equal(Number(params.get('today_start_ms')), localDayStart(date));
  assert.equal(Number(params.get('today_end_ms')), localDayEnd(date));
}

const handled = (title, at) => ({
  card: { kind: 'ask', title, source: { label: 'Project' } },
  swipe: { action: 'done', at_ms: at },
});

test('empty inbox refreshes Handled today after local midnight', async () => {
  const before = new Date(2026, 0, 15, 23, 59).getTime();
  const after = new Date(2026, 0, 16, 0, 1).getTime();
  const harness = makeHistoryHarness(before);
  harness.state.history.dayStart = localDayStart(before);
  harness.state.history.loaded = true;
  harness.state.history.today = 8;
  harness.setNow(after);
  harness.context.updateAges();

  assert.equal(harness.requests.length, 1, 'midnight starts a current-day history request');
  assertDayBounds(harness.requests[0], after);
  const loaded = harness.waitForRender(
    () => !harness.state.history.loading && harness.state.history.loaded,
  );
  harness.requests[0].resolve({ cards: [], today_count: 0 });
  await loaded;

  assert.equal(harness.state.history.today, 0, 'Handled today uses the new local-day count');
});

test('older local-day history response cannot overwrite rollover count', async () => {
  const before = new Date(2026, 0, 15, 23, 59).getTime();
  const after = new Date(2026, 0, 16, 0, 1).getTime();
  const harness = makeHistoryHarness(before);
  const oldLoad = harness.context.historyRows();
  harness.setNow(after);
  const currentLoad = harness.context.historyRows();

  assert.equal(harness.requests.length, 2, 'rollover starts a separate current-day request');
  assertDayBounds(harness.requests[0], before);
  assertDayBounds(harness.requests[1], after);
  harness.requests[1].resolve({ cards: [], today_count: 0 });
  await currentLoad;
  harness.requests[0].resolve({ cards: [], today_count: 8 });
  await oldLoad;

  assert.equal(harness.state.history.today, 0, 'late old count is ignored');
});

test('superseded history response cannot replace current drawer rows', async () => {
  const before = new Date(2026, 0, 15, 23, 59).getTime();
  const after = new Date(2026, 0, 16, 0, 1).getTime();
  const harness = makeHistoryHarness(before);
  harness.state.history.dayStart = localDayStart(before);
  harness.state.history.loaded = true;
  const drawer = harness.context.openHistory();
  harness.setNow(after);
  harness.context.updateAges();
  const current = handled('current-day', after);

  assert.equal(harness.requests.length, 2);
  assertDayBounds(harness.requests[0], before);
  assertDayBounds(harness.requests[1], after);
  const summaryLoaded = harness.waitForRender(
    () => harness.state.history.loaded && !harness.state.history.loading,
  );
  harness.requests[1].resolve({ cards: [current], today_count: 0 });
  await summaryLoaded;
  assert.equal(harness.state.history.today, 0, 'current-day count is installed before the old response');

  const request = harness.waitForRequest(3);
  const listUpdate = harness.waitForListUpdate(harness.list.updates);
  harness.requests[0].resolve({ cards: [handled('old-day', before)], today_count: 8 });
  const outcome = await Promise.race([
    request.then(() => 'retry'),
    listUpdate.then(() => 'rendered'),
    new Promise((resolve) => setImmediate(() => resolve('stalled'))),
  ]);

  assert.equal(outcome, 'retry', 'late previous-day rows do not replace the open drawer');
  assert.equal(harness.state.history.today, 0, 'late old count cannot replace the current count');
  const currentRows = harness.waitForList(() => harness.list.textContent.includes('current-day'));
  harness.requests[2].resolve({ cards: [current], today_count: 0 });
  await currentRows;
  await drawer;

  assert.equal(harness.list.textContent.includes('old-day'), false, 'late previous-day rows are discarded');
  assert.equal(harness.list.textContent.includes('Loading history…'), false, 'current history rows replace loading');
});

test('history dialog refreshes after a superseded day response', async () => {
  const before = new Date(2026, 0, 15, 23, 59).getTime();
  const after = new Date(2026, 0, 16, 0, 1).getTime();
  const harness = makeHistoryHarness(before);
  harness.state.history.dayStart = localDayStart(before);
  harness.state.history.loaded = true;
  const drawer = harness.context.openHistory();
  harness.setNow(after);
  harness.context.updateAges();
  const current = handled('current-day', after);

  const summaryLoaded = harness.waitForRender(
    () => harness.state.history.loaded && !harness.state.history.loading,
  );
  harness.requests[1].resolve({ cards: [current], today_count: 0 });
  await summaryLoaded;
  assert.equal(harness.list.textContent.includes('Loading history…'), true, 'drawer waits for its replacement request');

  const request = harness.waitForRequest(3);
  const done = drawer.then(() => 'finished');
  harness.requests[0].resolve({ cards: [handled('old-day', before)], today_count: 8 });
  const outcome = await Promise.race([
    request.then(() => 'retry'),
    done,
    new Promise((resolve) => setImmediate(() => resolve('stalled'))),
  ]);
  assert.equal(outcome, 'retry', 'superseded history request starts a current-day load');

  const currentRows = harness.waitForList(() => harness.list.textContent.includes('current-day'));
  harness.requests[2].resolve({ cards: [current], today_count: 0 });
  await currentRows;
  await drawer;

  assert.equal(harness.list.textContent.includes('Loading history…'), false, 'current rows replace loading history');
  assert.equal(harness.list.textContent.includes('current-day'), true, 'drawer shows current-day history');
  assert.equal(harness.list.textContent.includes('old-day'), false, 'drawer discards previous-day history');
});

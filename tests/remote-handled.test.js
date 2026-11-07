const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');
const test = require('node:test');

const source = readFileSync(join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const functionSource = (name, async = false) => {
  const declaration = async ? 'async function' : 'function',
    found = source.match(new RegExp(`^  ${declaration} ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'))?.[0];
  assert.ok(found, `app.js defines ${name}`);
  return found;
};

function queueHarness({ cards, shownId, idx = 0, pending = null }) {
  const state = {
      cards,
      counts: {},
      version: 0,
      idx,
      shownId,
      drafts: new Map(cards.map((card) => [card.id, { note: card.id, other: '', open: false }])),
      pending,
      initialized: true,
      history: { loaded: true, error: null },
      remoteSwapUntil: 0,
      inflight: new Set(),
    },
    toasts = [],
    renders = [],
    leaves = [],
    timers = [];
  const context = vm.createContext({
    S: state,
    snapshot: () => ({}),
    byPriority: () => 0,
    countsFrom: () => ({}),
    titleOf: (card) => card.title,
    toast: (message, error = false) => toasts.push({ message, error }),
    $: () => ({ classList: { add: (name) => leaves.push(name) } }),
    render: (_view, result = null) => {
      renders.push(result);
      state.shownId = state.cards[state.idx]?.id || null;
    },
    emptyHistory() {},
    Date: { now: () => 1000 },
    window: { matchMedia: () => ({ matches: false }) },
    setTimeout: (fn, delay) => {
      timers.push(delay);
      fn();
      return timers.length;
    },
  });
  vm.runInContext(`${functionSource('applyQueue')}\nglobalThis.applyQueue = applyQueue;`, context);
  return { state, context, toasts, renders, leaves, timers };
}

test('remote removal announces handling, prunes its draft, advances and guards input', () => {
  const current = { id: 'current', title: 'Current card' },
    next = { id: 'next', title: 'Next card' },
    harness = queueHarness({ cards: [current, next], shownId: current.id });

  harness.context.applyQueue({ cards: [next], version: 1 });

  assert.deepEqual(harness.toasts, [{ message: 'Handled elsewhere · Current card', error: false }]);
  assert.deepEqual([...harness.state.drafts.keys()], ['next']);
  assert.equal(harness.state.shownId, 'next');
  assert.equal(harness.state.remoteSwapUntil, 1500);
  assert.deepEqual(harness.renders, ['Handled elsewhere · Current card']);
  assert.deepEqual(harness.leaves, ['leaving-right']);
  assert.deepEqual(harness.timers, [150]);
  assert.equal(harness.state.direction, 1);
});

test('queue refresh after the local action splice does not announce remote handling', () => {
  const current = { id: 'current', title: 'Current card' },
    next = { id: 'next', title: 'Next card' },
    harness = queueHarness({ cards: [current, next], shownId: current.id });
  harness.state.cards.splice(0, 1);

  harness.context.applyQueue({ cards: [next], version: 1 });

  assert.deepEqual(harness.toasts, []);
  assert.equal(harness.state.remoteSwapUntil, 0);
  assert.deepEqual(harness.leaves, []);
});


test('pick ignores input during the remote swap guard', () => {
  const start = source.indexOf('  const byPriority ='),
    end = source.indexOf('  let serial =', start),
    comparator = vm.runInNewContext(`(() => { ${source.slice(start, end)} return byPriority; })()`),
    sorted = [
      { id: 'b', priority: 2, created_ms: 1 },
      { id: 'c', priority: 1, created_ms: 2 },
      { id: 'a', priority: 1, created_ms: 2 },
      { id: 'd', priority: 1, created_ms: 1 },
    ].sort(comparator);
  assert.deepEqual(sorted.map((card) => card.id), ['d', 'a', 'c', 'b']);

  const state = { remoteSwapUntil: 1500 },
    card = { options: [{}], kind: 'ask' };
  let actions = 0;
  const context = vm.createContext({
    S: state,
    Date: { now: () => 1000 },
    answerable: () => true,
    act: () => actions++,
    noteValue: () => null,
  });
  vm.runInContext(`${functionSource('pick')}\nglobalThis.pick = pick;`, context);
  context.pick(card, 0);
  assert.equal(actions, 0);
});

const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');
const test = require('node:test');

const source = readFileSync(join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const functionSource = (name) => {
  const found = source.match(new RegExp(`^  function ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'))?.[0];
  assert.ok(found, `app.js defines ${name}`);
  return found;
};
const comparatorStart = source.indexOf('  const byPriority ='),
  comparatorEnd = source.indexOf('  let serial =', comparatorStart),
  byPriority = vm.runInNewContext(
    `(() => { ${source.slice(comparatorStart, comparatorEnd)} return byPriority; })()`,
  );

function queueHarness({ cards, shownId, idx = 0, drafts = new Map(), laterOpen = false, pending = null }) {
  const state = {
      cards,
      counts: {},
      version: 0,
      idx,
      shownId,
      drafts,
      pending,
      laterOpen,
      initialized: true,
      history: { loaded: true, error: null },
      remoteSwapUntil: 0,
      inflight: new Set(),
      direction: 1,
    },
    context = vm.createContext({
      S: state,
      snapshot: () => ({}),
      byPriority,
      countsFrom: () => ({}),
      titleOf: (card) => card.title,
      toast() {},
      $: () => ({ classList: { add() {} } }),
      render: () => {
        state.shownId = state.cards[state.idx]?.id || null;
      },
      updateHeader() {},
      updateQueueLine() {},
      emptyHistory() {},
      Date: { now: () => 1000 },
      window: { matchMedia: () => ({ matches: false }) },
      setTimeout: (fn) => fn(),
    });
  vm.runInContext(`${functionSource('applyQueue')}\nglobalThis.applyQueue = applyQueue;`, context);
  return { state, context };
}

const info = (id, priority, created_ms) => ({ id, priority, created_ms, kind: 'info', title: id });
const low1 = info('low1', 3, 1),
  low2 = info('low2', 3, 2),
  urgent = info('urgent', 1, 3);

function update(harness, cards) {
  harness.context.applyQueue({ cards, version: 2 });
}

test('a new higher-priority leader takes the view to the top', () => {
  const harness = queueHarness({ cards: [low1, low2], shownId: 'low2', idx: 1 });

  update(harness, [low1, low2, urgent]);

  assert.equal(harness.state.idx, 0);
  assert.equal(harness.state.direction, -1);
  assert.equal(harness.state.shownId, 'urgent');
});

test('an equal-priority arrival leaves the shown card in place', () => {
  const harness = queueHarness({ cards: [low1, low2], shownId: 'low2', idx: 1 });

  update(harness, [low1, low2, info('low3', 3, 3)]);

  assert.equal(harness.state.idx, 1);
  assert.equal(harness.state.direction, 1);
  assert.equal(harness.state.shownId, 'low2');
});

test('typed text on the shown card blocks a leader jump', () => {
  const harness = queueHarness({
    cards: [low1, low2],
    shownId: 'low2',
    idx: 1,
    drafts: new Map([['low2', { note: 'half-typed', other: '', open: false }]]),
  });

  update(harness, [low1, low2, urgent]);

  assert.equal(harness.state.idx, 2);
  assert.equal(harness.state.shownId, 'low2');
});

test('an open Other row blocks a leader jump', () => {
  const harness = queueHarness({
    cards: [low1, low2],
    shownId: 'low2',
    idx: 1,
    drafts: new Map([['low2', { note: '', other: '', open: true }]]),
  });

  update(harness, [low1, low2, urgent]);

  assert.equal(harness.state.idx, 2);
});

test('the later menu blocks a leader jump', () => {
  const harness = queueHarness({ cards: [low1, low2], shownId: 'low2', idx: 1, laterOpen: true });

  update(harness, [low1, low2, urgent]);

  assert.equal(harness.state.idx, 2);
});

test('an in-flight action blocks a leader jump', () => {
  const harness = queueHarness({ cards: [low1, low2], shownId: 'low2', idx: 1, pending: { id: 'low2' } });

  update(harness, [low1, low2, urgent]);

  assert.equal(harness.state.idx, 2);
});

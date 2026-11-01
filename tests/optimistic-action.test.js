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
const card = (id, kind = 'ask') => ({ id, kind, title: id.toUpperCase(), options: [{ label: 'Yes' }] });
const settled = () => new Promise((resolve) => setImmediate(resolve));

function harness({ cards, inflight = [], api = async () => ({}) }) {
  const state = {
      cards: [...cards],
      counts: {},
      version: 0,
      idx: 0,
      shownId: cards[0]?.id ?? null,
      drafts: new Map(cards.map((c) => [c.id, { note: `note ${c.id}`, other: '', open: false }])),
      pending: null,
      inflight: new Set(inflight),
      initialized: true,
      history: { loaded: true, error: null },
      remoteSwapUntil: 0,
      actionRevision: 0,
      dirty: false,
      direction: 1,
      laterOpen: false,
    },
    calls = { toasts: [], announcements: [], refreshes: 0, renders: 0, headers: 0, queueLines: 0 },
    context = vm.createContext({
      S: state,
      api,
      enc: encodeURIComponent,
      snapshot: () => ({}),
      byPriority: () => 0,
      titleOf: (c) => c.title,
      toast: (message, error = false) => calls.toasts.push({ message, error }),
      announce: (message) => calls.announcements.push(message),
      refresh: () => {
        calls.refreshes++;
      },
      render: () => {
        calls.renders++;
        state.shownId = state.cards[state.idx]?.id ?? null;
      },
      updateHeader: () => {
        calls.headers++;
      },
      updateQueueLine: () => {
        calls.queueLines++;
      },
      emptyHistory() {},
      $: () => null,
      Date: { now: () => 1000 },
      window: { matchMedia: () => ({ matches: true }) },
    });
  vm.runInContext(
    [
      functionSource('countsFrom'),
      functionSource('resultMessage'),
      functionSource('applyQueue'),
      functionSource('act', true),
      functionSource('settle', true),
      'Object.assign(globalThis, { act, applyQueue });',
    ].join('\n'),
    context,
  );
  return { state, context, calls };
}

test('an action moves to the next card before the daemon answers', async () => {
  let reply;
  const pending = new Promise((resolve) => {
      reply = resolve;
    }),
    { state, context, calls } = harness({ cards: [card('a'), card('b')], api: () => pending });

  const outcome = await Promise.race([
    context.act(state.cards[0], 'answer', { option: 0 }, 'opt-0'),
    new Promise((resolve) => setTimeout(() => resolve('still waiting'), 50)),
  ]);

  assert.equal(outcome, true);
  assert.deepEqual(state.cards.map((c) => c.id), ['b']);
  assert.equal(state.shownId, 'b');
  assert.ok(state.inflight.has('a'));
  assert.deepEqual(calls.toasts, [{ message: 'Sent Yes · A', error: false }]);
  reply({ ok: true });
  await settled();
  assert.equal(state.inflight.size, 0);
  assert.equal(calls.refreshes, 1);
  assert.equal(state.drafts.has('a'), false);
});

test('a failed action reports its error and keeps the draft for the returning card', async () => {
  const { state, context, calls } = harness({
    cards: [card('a'), card('b')],
    api: async () => {
      throw Object.assign(new Error('ask dialog not found'), { status: 422 });
    },
  });

  await context.act(state.cards[0], 'answer', { option: 0 }, 'opt-0');
  await settled();

  assert.deepEqual(calls.toasts, [
    { message: 'Sent Yes · A', error: false },
    { message: 'ask dialog not found · A', error: true },
  ]);
  assert.deepEqual(calls.announcements, ['Error: ask dialog not found · A']);
  assert.equal(state.drafts.get('a').note, 'note a');
  assert.equal(state.inflight.size, 0);
  assert.equal(calls.refreshes, 1);
});

test('409 and 404 report a card handled elsewhere without an error', async () => {
  for (const status of [409, 404]) {
    const { state, context, calls } = harness({
      cards: [card('a'), card('b')],
      api: async () => {
        throw Object.assign(new Error('card is not queued'), { status });
      },
    });

    await context.act(state.cards[0], 'answer', { option: 0 }, 'opt-0');
    await settled();

    assert.deepEqual(calls.toasts.at(-1), { message: 'Already handled elsewhere · A', error: false });
    assert.deepEqual(calls.announcements, ['Already handled elsewhere · A']);
    assert.equal(state.drafts.has('a'), false);
    assert.equal(state.remoteSwapUntil, 0);
  }
});

test('queue refreshes keep an in-flight card hidden but keep its draft', () => {
  const a = card('a'),
    b = card('b', 'info'),
    { state, context } = harness({ cards: [b], inflight: ['a'] });
  state.drafts.set('a', { note: 'note a', other: '', open: false });

  context.applyQueue({ cards: [a, b], version: 2 });

  assert.deepEqual(state.cards.map((c) => c.id), ['b']);
  assert.equal(state.counts.ask, 0);
  assert.equal(state.counts.info, 1);
  assert.equal(state.drafts.get('a').note, 'note a');
});

test('a refresh that keeps the shown card updates the header and queue line without a rebuild', () => {
  const a = card('a'),
    b = card('b'),
    { state, context, calls } = harness({ cards: [a, b] });

  context.applyQueue({ cards: [a, b, card('c')], version: 2 });

  assert.equal(calls.renders, 0);
  assert.equal(calls.headers, 1);
  assert.equal(calls.queueLines, 1);
  assert.equal(state.cards.length, 3);
});

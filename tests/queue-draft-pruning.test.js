const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');
const test = require('node:test');

const source = readFileSync(join(__dirname, '..', 'ui', 'app.js'), 'utf8');

test('queue refresh prunes drafts for removed cards', () => {
  const applyQueueSource = source.match(/^  function applyQueue\(q\) \{[\s\S]*?^  \}/m)?.[0];
  assert.ok(applyQueueSource, 'app.js defines applyQueue');

  const current = { id: 'current' };
  const S = {
    cards: [{ id: 'removed' }, current],
    counts: {},
    version: 0,
    idx: 1,
    shownId: 'current',
    drafts: new Map([
      ['removed', { note: 'stale' }],
      ['current', { note: 'kept' }],
    ]),
    initialized: true,
    history: {},
    inflight: new Set(),
  };
  const applyQueue = vm.runInNewContext(`(${applyQueueSource})`, {
    S,
    snapshot: () => ({}),
    byPriority: () => 0,
    countsFrom: () => ({}),
    updateHeader: () => {},
    updateQueueLine: () => {},
    render: () => {},
  });

  applyQueue({ cards: [current], version: 1 });

  assert.deepEqual([...S.drafts.keys()], ['current'], 'removed card drafts are pruned');
});

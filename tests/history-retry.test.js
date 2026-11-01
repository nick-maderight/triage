const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');
const test = require('node:test');

const source = readFileSync(join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const start = source.indexOf('  function historyDayStart() {');
const end = source.indexOf('  // render', start);
assert.ok(start >= 0 && end > start);
const historyFunctions = source.slice(start, end);

function historyHarness() {
  const state = {
      cards: [],
      historyPromise: null,
      historyPromiseDate: null,
      history: { loading: false, loaded: false, dayStart: null, error: null, today: 0, lastAt: null },
    },
    responses = [
      () => Promise.reject(new Error('temporary failure')),
      () => Promise.resolve({ cards: [], today_count: 0 }),
    ];
  let requests = 0;
  const context = vm.createContext({
    S: state,
    api: () => responses[requests++](),
    render() {},
    stamp: () => null,
    Date,
    URLSearchParams,
  });
  vm.runInContext(
    `${historyFunctions}\nObject.assign(globalThis, { historyRows, emptyHistory });`,
    context,
  );
  return { state, context, requests: () => requests };
}

test('empty-state history retries failures and clears errors after recovery', async () => {
  const retry = historyHarness();
  await retry.context.emptyHistory();
  assert.equal(retry.state.history.error, 'temporary failure');
  assert.equal(retry.state.history.loaded, false, 'failed history fetch remains retryable');
  await retry.context.emptyHistory();
  assert.equal(retry.requests(), 2, 'retry performs a second history request');
  assert.equal(retry.state.history.loaded, true);

  const recovery = historyHarness();
  await recovery.context.emptyHistory();
  assert.equal(recovery.state.history.error, 'temporary failure');
  await recovery.context.historyRows();
  assert.equal(recovery.state.history.error, null, 'successful history fetch clears stale error');
});

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const appSource = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');

test('queue refresh retries with capped exponential delay while stream stays open', async () => {
  const refreshStart = appSource.indexOf('  function refresh() {');
  const retryStart = appSource.indexOf('  function scheduleRefreshRetry() {');
  const reconnectStart = appSource.indexOf('  function reconnect() {', refreshStart);
  assert.notEqual(refreshStart, -1);
  assert.notEqual(reconnectStart, -1);

  const timers = [];
  let nextTimer = 1;
  let fetches = 0;
  const context = vm.createContext({
    S: {
      actionRevision: 0,
      dirty: false,
      initialized: false,
      pending: null,
      refreshPromise: null,
      refreshRetryDelay: 1000,
      refreshRetryTimer: 0,
      streamOpen: true,
    },
    api: async () => {
      fetches += 1;
      throw new Error('offline');
    },
    announce() {},
    applyQueue() {},
    clearTimeout(id) {
      const index = timers.findIndex((timer) => timer.id === id);
      if (index >= 0) timers.splice(index, 1);
    },
    render() {},
    setConnection() {},
    setTimeout(callback, delay) {
      const timer = { id: nextTimer++, callback, delay };
      timers.push(timer);
      return timer.id;
    },
    toast() {},
  });
  const start = retryStart === -1 ? refreshStart : retryStart;
  vm.runInContext(
    `${appSource.slice(start, reconnectStart)}\nglobalThis.refresh = refresh;`,
    context,
  );

  await context.refresh();
  const delays = [1000, 2000, 4000, 8000, 16000, 30000, 30000];
  for (const delay of delays) {
    const timer = timers.shift();
    assert.ok(timer, 'failed queue fetch schedules a retry');
    assert.equal(timer.delay, delay);
    timer.callback();
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.equal(fetches, delays.length + 1);
  assert.equal(timers.length, 1);
  assert.equal(timers[0].delay, 30000);
});

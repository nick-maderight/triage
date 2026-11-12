const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const worker = fs.readFileSync(path.join(__dirname, '..', 'ui', 'sw.js'), 'utf8');

// Runs the worker's activate handler against open windows that may or may not answer its ping.
async function activate(windows) {
  const handlers = { activate: [], message: [] },
    deleted = [],
    timers = [],
    self = {
      addEventListener: (type, handler) => (handlers[type] ||= []).push(handler),
      removeEventListener: (type, handler) => {
        handlers[type] = (handlers[type] || []).filter((item) => item !== handler);
      },
      location: { origin: 'https://triage.test' },
      clients: { claim: async () => {}, matchAll: async () => windows },
    },
    caches = {
      keys: async () => ['triage-shell-v11', 'triage-shell-v12', 'triage-shell-v13', 'triage-shell-v14'],
      delete: async (key) => deleted.push(key),
    },
    context = vm.createContext({
      self,
      caches,
      fetch() {},
      URL,
      setTimeout: (handler) => (timers.push(handler), 0),
    });
  vm.runInContext(worker, context);
  let pending;
  handlers.activate[0]({ waitUntil: (promise) => (pending = promise) });
  // Let activate reach its ping wait before the windows answer.
  for (let i = 0; i < 20 && !timers.length; i++) await Promise.resolve();
  for (const client of windows) {
    if (!client.answers) continue;
    for (const handler of handlers.message)
      handler({ data: { type: 'triage-shell-pong' }, source: { id: client.id } });
  }
  timers.forEach((handler) => handler());
  await pending;
  return { deleted };
}

function window(id, url, answers) {
  const client = {
    id,
    url,
    answers,
    posted: [],
    navigated: [],
    postMessage(message) {
      client.posted.push(message);
    },
    async navigate(target) {
      client.navigated.push(target);
    },
  };
  return client;
}

test('a worker update reloads only tabs that do not answer its ping', async () => {
  const old = window('old', 'https://triage.test/', false),
    current = window('current', 'https://triage.test/?card=1', true),
    result = await activate([old, current]);
  assert.deepEqual(result.deleted, ['triage-shell-v11', 'triage-shell-v12', 'triage-shell-v13'], 'every cache but the current one goes');
  assert.equal(old.posted[0].type, 'triage-shell-ping', 'every tab is asked once');
  assert.equal(current.posted[0].type, 'triage-shell-ping');
  assert.deepEqual(old.navigated, ['https://triage.test/'], 'a silent tab reloads itself');
  assert.deepEqual(current.navigated, [], 'a tab that answers keeps its place');
});

test('a worker update reloads every tab when none answer', async () => {
  const one = window('one', 'https://triage.test/?one', false),
    two = window('two', 'https://triage.test/?two', false),
    result = await activate([one, two]);
  assert.deepEqual(result.deleted, ['triage-shell-v11', 'triage-shell-v12', 'triage-shell-v13'], 'every cache but the current one goes');
  assert.deepEqual(
    [one.navigated, two.navigated],
    [['https://triage.test/?one'], ['https://triage.test/?two']],
  );
});

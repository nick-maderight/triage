const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const worker = fs.readFileSync(path.join(__dirname, '..', 'ui', 'sw.js'), 'utf8');

// Runs ui/sw.js in a fake service worker scope whose open windows are `windows`.
function scope(windows = []) {
  const handlers = {},
    shown = [],
    focused = [],
    opened = [];
  const self = {
    addEventListener: (type, handler) => {
      handlers[type] = handler;
    },
    location: { origin: 'https://triage.test' },
    registration: {
      showNotification: async (title, options) => {
        shown.push({ title, options });
      },
    },
    clients: {
      matchAll: async () => windows.map((url) => ({ url, focus: async () => focused.push(url) })),
      openWindow: async (url) => {
        opened.push(url);
      },
    },
  };
  vm.runInContext(worker, vm.createContext({ self, caches: {}, fetch() {}, URL }));
  const dispatch = async (type, event) => {
    let pending;
    handlers[type]({ ...event, waitUntil: (p) => (pending = p) });
    await pending;
  };
  return { dispatch, shown, focused, opened };
}

test('a push shows its card as a notification tagged with the card id', async () => {
  const { dispatch, shown } = scope();
  const card = { title: 'Approve format 5?', body: 'random:12 OWNR', tag: 'card-1' };
  await dispatch('push', { data: { json: () => card } });
  assert.equal(shown.length, 1);
  assert.equal(shown[0].title, 'Approve format 5?');
  assert.equal(shown[0].options.body, 'random:12 OWNR');
  assert.equal(shown[0].options.tag, 'card-1');
});

test('a push without a readable payload still shows a notification', async () => {
  const garbled = {
    json: () => {
      throw new SyntaxError('Unexpected token');
    },
  };
  for (const data of [null, garbled]) {
    const { dispatch, shown } = scope();
    await dispatch('push', { data });
    assert.equal(shown.length, 1);
    assert.equal(shown[0].title, 'Triage');
    assert.equal(shown[0].options.tag, 'triage');
  }
});

test('tapping a notification focuses an open inbox, or opens one', async () => {
  const tap = { notification: { close() {} } };
  const open = scope(['https://triage.test/']);
  await open.dispatch('notificationclick', tap);
  assert.deepEqual(open.focused, ['https://triage.test/']);
  assert.deepEqual(open.opened, []);

  const closed = scope();
  await closed.dispatch('notificationclick', tap);
  assert.deepEqual(closed.opened, ['/']);
});

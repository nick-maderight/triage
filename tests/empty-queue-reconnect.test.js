const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const functionSource = (name) => {
  const source = app.match(new RegExp(`^  function ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'))?.[0];
  assert.ok(source, `app.js defines ${name}`);
  return source;
};

const methods = ['setConnection', 'emptyView', 'retryNow'].map(functionSource).join('\n');

function node(tag, props = {}, ...children) {
  return {
    tag,
    props: typeof props === 'string' ? { class: props } : props,
    children: children.flat(Infinity).filter((child) => child != null && child !== false && child !== ''),
  };
}

const text = (value) =>
  typeof value === 'string' ? value : value?.children?.map(text).join('') || '';

function heading(view) {
  if (!view) return null;
  if (view.tag === 'h2') return text(view);
  for (const child of view.children || []) {
    const result = heading(child);
    if (result) return result;
  }
  return null;
}

// Runs the connection functions with fake timers: flush() fires every pending timer at once.
function harness() {
  const connection = { dataset: {}, setAttribute() {} },
    label = { textContent: '' },
    rendered = [],
    calls = [],
    timers = new Map();
  let nextTimer = 0;
  const context = vm.createContext({
    S: {
      connection: 'connected',
      initialized: true,
      cards: [],
      history: { loading: false, loaded: true, error: null, today: 0, lastAt: null },
      offline: false,
      offlineTimer: 0,
      reconnectTimer: 0,
      reconnectDelay: 1000,
    },
    $: (selector) => (selector === '#connection' ? connection : label),
    h: node,
    el: (tag, props, ...children) => node(tag, props || {}, ...children),
    chip: (kind) => node('span', `chip-${kind}`, kind),
    render: () => rendered.push(heading(context.emptyView())),
    setTimeout: (fn) => {
      timers.set(++nextTimer, fn);
      return nextTimer;
    },
    clearTimeout: (id) => timers.delete(id),
    connectStream: () => calls.push('connectStream'),
    refresh: () => {
      calls.push('refresh');
      return Promise.resolve();
    },
  });
  vm.runInContext(
    `${methods}\nglobalThis.setConnection = setConnection; globalThis.emptyView = emptyView; globalThis.retryNow = retryNow;`,
    context,
  );
  const flush = () => {
    for (const [id, fn] of [...timers]) {
      timers.delete(id);
      fn();
    }
  };
  return { context, connection, label, rendered, calls, flush };
}

test('a drop that outlasts the grace period shows the offline view, and recovery restores inbox zero', () => {
  const { context, connection, label, rendered, flush } = harness();

  context.setConnection('reconnecting');
  assert.deepEqual(rendered, [], 'the queue stays on screen during the grace period');
  flush();
  context.setConnection('connected');

  assert.deepEqual(rendered, ['Daemon unreachable', 'Inbox zero'], 'empty view follows disconnect and recovery');
  assert.equal(connection.dataset.state, 'connected');
  assert.equal(label.textContent, 'Connected');
});

test('a drop that recovers within the grace period never shows the offline view', () => {
  const { context, rendered, flush } = harness();

  context.setConnection('reconnecting');
  context.setConnection('connected');
  flush();

  assert.deepEqual(rendered, []);
});

test('Retry now skips the reconnect backoff', () => {
  const { context, calls, flush } = harness();
  context.S.reconnectTimer = context.setTimeout(() => calls.push('backoff'));
  context.S.reconnectDelay = 8000;

  context.retryNow();
  flush();

  assert.deepEqual(calls, ['connectStream']);
  assert.equal(context.S.reconnectTimer, 0);
  assert.equal(context.S.reconnectDelay, 1000);

  context.retryNow();
  assert.deepEqual(calls, ['connectStream', 'refresh'], 'with no backoff pending, Retry now refreshes the queue');
});

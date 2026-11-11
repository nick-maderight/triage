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

function inbox() {
  const state = {
      shell: null,
      drafts: new Map(),
      inflight: new Set(),
      pending: null,
      actionRevision: 0,
      dirty: false,
      refreshPromise: null,
      refreshRetryTimer: 0,
      refreshRetryDelay: 1000,
      streamOpen: true,
      initialized: true,
    },
    page = { shell: 'W/"loaded"', dialog: false, prompt: '', applied: 0, reloads: 0 },
    context = vm.createContext({
      S: state,
      api: async () => ({ cards: [], version: 1, shell: page.shell }),
      applyQueue: () => page.applied++,
      setConnection() {},
      scheduleRefreshRetry() {},
      clearTimeout() {},
      toast() {},
      announce() {},
      render() {},
      $: (selector) => ({ 'dialog[open]': page.dialog ? {} : null, '#compose-prompt': { value: page.prompt } })[selector] ?? null,
      location: { reload: () => page.reloads++ },
    });
  vm.runInContext(`${functionSource('reloadIfStale')}\n${functionSource('refresh')}\nglobalThis.refresh = refresh;`, context);
  return { state, page, refresh: () => context.refresh() };
}

test('a queue from a daemon serving another shell reloads the page instead of applying', async () => {
  const { page, refresh } = inbox();
  await refresh();
  await refresh();
  assert.deepEqual([page.applied, page.reloads], [2, 0], 'the shell the page loaded with keeps it');
  page.shell = 'W/"upgraded"';
  await refresh();
  assert.deepEqual([page.applied, page.reloads], [2, 1]);
});

test('typed text, an open dialog or an action in flight holds the reload', async () => {
  const { state, page, refresh } = inbox(),
    draft = { note: '', other: 'half typed', dispatch: '' };
  await refresh();
  page.shell = 'W/"upgraded"';
  state.drafts.set('card', draft);
  await refresh();
  draft.other = '';
  page.prompt = 'unsent prompt';
  await refresh();
  page.prompt = '';
  page.dialog = true;
  await refresh();
  page.dialog = false;
  state.inflight.add('card');
  await refresh();
  assert.deepEqual([page.applied, page.reloads], [5, 0], 'each held refresh still applies the queue');
  state.inflight.clear();
  await refresh();
  assert.deepEqual([page.applied, page.reloads], [5, 1]);
});

test('a daemon rolled back to a build that names no shell also reloads the page', async () => {
  const { page, refresh } = inbox();
  await refresh();
  page.shell = undefined;
  await refresh();
  assert.equal(page.reloads, 1);
});

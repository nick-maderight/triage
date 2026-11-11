const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const functionSource = (name, async = false) => {
  const source = app.match(
    new RegExp(`^  ${async ? 'async ' : ''}function ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'),
  )?.[0];
  assert.ok(source, `app.js defines ${name}`);
  return source;
};
const settled = async () => {
  for (let i = 0; i < 3; i++) await new Promise((resolve) => setImmediate(resolve));
};

// One New session sheet whose daemon request the test settles by hand. That lets a test
// check the state the user sees while the launch runs.

function harness() {
  const toasts = [],
    announcements = [],
    requests = [],
    prompt = { value: 'Make the inbox snappy' },
    start = { disabled: false },
    hint = { textContent: '' },
    compose = {
      open: true,
      close() {
        this.open = false;
      },
      showModal() {
        this.open = true;
      },
    },
    other = { open: false, close() {}, showModal() {} },
    elements = {
      '#compose-prompt': prompt,
      '#compose-cwd': { value: '' },
      '#compose-session': { value: '' },
      '#compose-engine': { value: 'dsh' },
      '#compose-model': { value: '' },
      '#compose-thinking': { value: '' },
      '#compose': compose,
      '#compose-start': start,
      '#compose-hint': hint,
    };
  let answer;
  const response = new Promise((resolve, reject) => {
    answer = { resolve, reject };
  });
  const context = vm.createContext({
    S: { composeMode: 'goal' },
    $: (selector) => {
      if (selector === 'dialog[open]') return compose.open ? compose : other.open ? other : null;
      return elements[selector] || null;
    },
    fetch: (url, init) => {
      requests.push({ url, init });
      return response;
    },
    sheetImageBody: async () => null,
    announce: (text) => announcements.push(text),
    toast: (message, error = false, keep = false) => {
      const toast = {
        message,
        error,
        keep,
        removed: false,
        remove() {
          this.removed = true;
        },
      };
      toasts.push(toast);
      return toast;
    },
    showDialog: (dialog) => dialog.showModal(),
  });
  vm.runInContext(
    `${functionSource('api', true)}\n${functionSource('submitCompose', true)}\nglobalThis.submitCompose = submitCompose;`,
    context,
  );
  return { context, prompt, start, hint, compose, other, toasts, announcements, requests, answer };
}

test('Start closes the sheet while the daemon launches', async () => {
  const h = harness(),
    submitted = h.context.submitCompose({ preventDefault() {} });
  await settled();
  assert.equal(h.requests.length, 1);
  assert.equal(h.compose.open, false);
  assert.equal(h.prompt.value, '');
  assert.equal(h.start.disabled, true);
  assert.deepEqual(
    h.toasts.map((toast) => [toast.message, toast.keep]),
    [['Starting…', true]],
  );
  h.answer.resolve({ ok: true, json: async () => ({ pane: 'dsh:abc' }) });
  await submitted;
  assert.deepEqual(h.announcements, ['Starting a session.', 'Started · dsh:abc']);
  assert.deepEqual(
    h.toasts.map((toast) => toast.message),
    ['Starting…', 'Started a dsh session.'],
  );
  assert.equal(h.toasts[0].removed, true);
  assert.equal(h.start.disabled, false);
});

test('a failed Start puts its prompt back and reopens the sheet', async () => {
  const h = harness(),
    submitted = h.context.submitCompose({ preventDefault() {} });
  await settled();
  assert.equal(h.compose.open, false);
  h.answer.resolve({
    ok: false,
    status: 400,
    json: async () => ({ error: 'dsh bridge unreachable: timeout: global' }),
  });
  await submitted;
  assert.equal(h.prompt.value, 'Make the inbox snappy');
  assert.equal(h.hint.textContent, 'dsh bridge unreachable: timeout: global');
  assert.equal(h.compose.open, true);
  assert.deepEqual(
    h.toasts.map((toast) => [toast.message, toast.error]),
    [
      ['Starting…', false],
      ['dsh bridge unreachable: timeout: global', true],
    ],
  );
  assert.equal(h.toasts[0].removed, true);
});

test('a failed Start leaves a newer draft and an open sheet alone', async () => {
  const h = harness(),
    submitted = h.context.submitCompose({ preventDefault() {} });
  await settled();
  h.other.open = true;
  h.prompt.value = 'A newer question';
  h.answer.resolve({ ok: false, status: 500, statusText: 'Server Error', json: async () => null });
  await submitted;
  assert.equal(h.prompt.value, 'A newer question');
  assert.equal(h.other.open, true);
  assert.equal(h.compose.open, false);
  assert.deepEqual(
    h.toasts.map((toast) => [toast.message, toast.error]),
    [
      ['Starting…', false],
      ['500 Server Error', true],
    ],
  );
});

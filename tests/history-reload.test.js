const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');
const test = require('node:test');

const source = readFileSync(join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const start = source.indexOf('  async function reloadHistoryCard(');
const end = source.indexOf('  function openHistory() {', start);
assert.ok(start >= 0 && end > start);
const reloadFunctions = source.slice(start, end);

// Minimal element stand-in: h() records tag, props and flattened children.
const node = (tag, props, ...kids) => ({
  tag,
  props: typeof props === 'string' ? { class: props } : props || {},
  kids: kids.flat(Infinity).filter((x) => x != null && x !== false && x !== ''),
});

function harness(apiImpl) {
  const log = { api: [], toasts: [], announced: [], refreshes: 0 };
  const context = vm.createContext({
    enc: encodeURIComponent,
    api: (path, body) => {
      log.api.push([path, body]);
      return apiImpl(log);
    },
    toast: (text, error = false) => log.toasts.push([text, error]),
    announce: (text) => log.announced.push(text),
    refresh: () => {
      log.refreshes++;
      return Promise.resolve();
    },
    h: node,
    el: (tag, cls, ...kids) => node(tag, cls || {}, ...kids),
    chip: (k) => node('span', `chip chip-${k}`, k),
    sourceName: () => 'proj',
    stamp: () => null,
    clock: () => '12:00',
    Date,
  });
  vm.runInContext(
    `${reloadFunctions}\nObject.assign(globalThis, { reloadHistoryCard, historyItem });`,
    context,
  );
  return { context, log };
}

const card = { id: 'x', title: 'T' };

test('reload success posts, announces, refreshes and toggles the button', async () => {
  const button = { disabled: false };
  let during;
  const { context, log } = harness(async () => {
    during = button.disabled;
    return { ok: true, card: 'y' };
  });
  await context.reloadHistoryCard(card, button);
  assert.equal(log.api.length, 1);
  assert.equal(log.api[0][0], '/v1/cards/x/reload');
  assert.deepEqual(log.toasts, []);
  assert.deepEqual(log.announced, ['Reloaded · T']);
  assert.equal(log.refreshes, 1);
  assert.equal(during, true, 'button disabled during the request');
  assert.equal(button.disabled, false, 'button re-enabled afterwards');
});

test('reload id is URL-encoded and untitled cards get a fallback name', async () => {
  const { context, log } = harness(async () => ({ ok: true }));
  await context.reloadHistoryCard({ id: 'a/b' }, { disabled: false });
  assert.equal(log.api[0][0], '/v1/cards/a%2Fb/reload');
  assert.deepEqual(log.announced, ['Reloaded · Untitled card']);
});

test('reload 409 reports already queued as an error toast', async () => {
  const button = { disabled: false };
  const { context, log } = harness(async () => {
    throw Object.assign(new Error('card is already queued'), { status: 409 });
  });
  await context.reloadHistoryCard(card, button);
  assert.deepEqual(log.toasts, [['Already queued · T', true]]);
  assert.deepEqual(log.announced, ['Already queued · T']);
  assert.equal(log.refreshes, 0);
  assert.equal(button.disabled, false);
});

test('reload other failures report the error message', async () => {
  const button = { disabled: false };
  const { context, log } = harness(async () => {
    throw Object.assign(new Error('card not found'), { status: 404 });
  });
  await context.reloadHistoryCard(card, button);
  assert.deepEqual(log.toasts, [['Reload failed: card not found', true]]);
  assert.deepEqual(log.announced, ['Reload failed: card not found']);
  assert.equal(log.refreshes, 0);
  assert.equal(button.disabled, false);
});

test('reload does nothing while the button is disabled', async () => {
  const button = { disabled: true };
  const { context, log } = harness(async () => ({ ok: true }));
  await context.reloadHistoryCard(card, button);
  assert.equal(log.api.length, 0);
  assert.equal(log.toasts.length, 0);
  assert.equal(button.disabled, true);
});

test('history rows carry a Reload button unless the card was deferred', async () => {
  const { context, log } = harness(async () => ({ ok: true }));
  const meta = (row) => context.historyItem(row).kids[0];
  const buttons = (row) => meta(row).kids.filter((k) => k.tag === 'button');
  const reload = (row) => buttons(row).find((k) => /\bh-reload\b/.test(k.props.class));
  for (const action of ['done', 'skipped', 'unknown-action']) {
    const kids = meta({ card, swipe: { action } }).kids,
      b = kids.find((k) => /\bh-reload\b/.test(k.props.class));
    assert.ok(b, `${action} row has a Reload button`);
    assert.equal(kids.at(-1), b, 'Reload is the last meta child');
    const contextButton = kids.find((k) => /\bh-context\b/.test(k.props.class));
    assert.ok(contextButton, `${action} row has a Context button`);
    assert.ok(kids.indexOf(contextButton) < kids.indexOf(b), 'Context sits beside Reload');
  }
  assert.equal(reload({ card, swipe: { action: 'later' } }), undefined);
  assert.equal(reload({ card: { title: 'no id' }, swipe: { action: 'done' } }), undefined);

  const button = { disabled: false };
  await reload({ card, swipe: { action: 'done' } }).props.onclick({ currentTarget: button });
  assert.equal(log.api[0][0], '/v1/cards/x/reload');
});

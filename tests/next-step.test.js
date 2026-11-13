const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const functionSource = (name, async = false) => {
  const declaration = async ? 'async function' : 'function',
    source = app.match(new RegExp(`^  ${declaration} ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'))?.[0];
  assert.ok(source, `app.js defines ${name}`);
  return source;
};

// A next-step button opens the sheet, and the sheet asks the daemon for a session of its own.
test('the next step opens the sheet and launches in a new window', async () => {
  const card = { id: 'report-card', kind: 'info', title: 'Report', star: { action: 'Add both as read collaborators.' } },
    requests = [],
    elements = {
      '#dispatch-note': { value: '', focus() {} },
      '#dispatch-hint': { textContent: '' },
      '#dispatch-start': { disabled: false },
      '#dispatch-engine': { value: 'omp' },
      '#dispatch-model': { value: '' },
      '#dispatch-thinking': { value: '' },
      '#dispatch-executor': { value: '' },
      '#dispatch-executor-effort': { value: '' },
      '#dispatch-mode': { children: [] },
      '#dispatch-mode-help': { textContent: '' },
      '#dispatch-models': { hidden: true },
      '#dispatch-image': { value: '' },
      '#dispatch': { open: false, showModal() { this.open = true; }, close() { this.open = false; } },
    },
    state = {
      cards: [card],
      counts: { info: 1 },
      drafts: new Map([[card.id, { dispatchMode: 'plan', note: '' }]]),
      dispatchId: null,
      pending: null,
      remoteSwapUntil: 0,
      inflight: new Set(),
      actionRevision: 0,
      dirty: false,
      idx: 0,
      history: { loaded: true, error: null },
    },
    opened = [],
    context = vm.createContext({
      S: state,
      Date: { now: () => 1000 },
      window: { matchMedia: () => ({ matches: true }) },
      $: (selector) => elements[selector] || null,
      draft: (current) => state.drafts.get(current.id),
      dshOf: () => false,
      PICKERS: { 'dispatch-model': { options: [{}] } },
      setPickerValue() {},
      clearModels() {},
      loadModels() {},
      showDialog: (dialog) => opened.push(dialog),
      snapshot: () => ({}),
      render() {},
      enc: encodeURIComponent,
      sheetImageBody: async () => null,
      resultMessage: () => 'Launched · Report',
      toast() {},
      refresh() {},
      fetch: async (url, init) => {
        requests.push({ url, init });
        return { ok: true, json: async () => ({ pane: '%42' }) };
      },
    });
  vm.runInContext(
    `${functionSource('selectSegment')}\n${functionSource('setDispatchMode')}\n${functionSource('openDispatch')}\n${functionSource('api', true)}\n${functionSource('act', true)}\n${functionSource('settle', true)}\n${functionSource('submitDispatch', true)}\nglobalThis.openDispatch = openDispatch; globalThis.submitDispatch = submitDispatch;`,
    context,
  );

  context.openDispatch(card, 'act', true, null, null, true);
  assert.equal(state.dispatchId, card.id);
  assert.equal(state.drafts.get(card.id).dispatchWindow, true);
  assert.equal(opened.length, 1, 'the sheet opened');
  await context.submitDispatch({ preventDefault() {} });
  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, '/v1/cards/report-card/dispatch');
  assert.deepEqual(JSON.parse(requests[0].init.body), {
    mode: 'act',
    note: null,
    engine: 'omp',
    model: null,
    thinking: null,
    new_window: true,
  });

  // Dispatch from the same card keeps its own behavior: no new window unless asked.
  requests.length = 0;
  state.cards.push(card);
  context.openDispatch(card, 'investigate');
  assert.equal(state.drafts.get(card.id).dispatchWindow, false);
  await context.submitDispatch({ preventDefault() {} });
  assert.equal(JSON.parse(requests[0].init.body).new_window, false);
});

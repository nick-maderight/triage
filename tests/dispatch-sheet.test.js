// The Dispatch sheet is one sheet for every card. These tests hold what it opens with and what
// Launch sends to the card's own session, engine, note and mode.
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
const slice = (from, to) => {
  const start = app.indexOf(from),
    end = app.indexOf(to, start);
  assert.ok(start >= 0 && end > start, `app.js holds ${from.trim()}`);
  return app.slice(start, end);
};
// The Act direction the sheet starts on, read from the sheet's own function.
const actDefault = app.match(/const actDefault =\s*'([^']+)';/)?.[1];
assert.ok(actDefault, 'app.js defines the Act default direction');

// Loads the real sheet code, which is its opener, mode buttons, note field and Launch.
// `daemon` is the engine the daemon launches by default.
function sheet(cards, daemon = 'omp') {
  const requests = [],
    cleared = [],
    listeners = {},
    modes = ['investigate', 'investigate-fix', 'act', 'plan', 'plan-into'].map((mode) => ({
      dataset: { mode },
      attrs: {},
      setAttribute(name, value) {
        this.attrs[name] = value;
      },
    })),
    elements = {
      '#note': { value: '' },
      '#dispatch-note': { value: '', focus() {}, addEventListener: (type, listener) => (listeners[type] = listener) },
      '#dispatch-hint': { textContent: '' },
      '#dispatch-start': { disabled: false },
      '#dispatch-engine': { value: '' },
      '#dispatch-model': { value: '' },
      '#dispatch-thinking': { value: '' },
      '#dispatch-executor': { value: '' },
      '#dispatch-executor-effort': { value: '' },
      '#dispatch-mode': { children: modes },
      '#dispatch-mode-help': { textContent: '' },
      '#dispatch-models': { hidden: true },
      '#dispatch': { close() {} },
    },
    state = {
      cards: [...cards],
      counts: {},
      drafts: new Map(),
      shownId: cards[0].id,
      dispatchId: null,
      dispatchEngine: '',
      pending: null,
      remoteSwapUntil: 0,
      inflight: new Set(),
      actionRevision: 0,
      dirty: false,
      idx: 0,
      history: { loaded: true, error: null },
    },
    context = vm.createContext({
      S: state,
      Date: { now: () => 1000 },
      window: { matchMedia: () => ({ matches: true }) },
      $: (selector) => elements[selector] || null,
      PICKERS: { 'dispatch-engine': { fallback: daemon }, 'dispatch-model': { options: [{}] } },
      setPickerValue: (id, value) => {
        elements[`#${id}`].value = value;
      },
      clearModels: () => cleared.push(elements['#dispatch-engine'].value),
      loadModels() {},
      showDialog() {},
      snapshot: () => ({}),
      render() {},
      enc: encodeURIComponent,
      sheetImageBody: async () => null,
      resultMessage: () => '',
      toast() {},
      refresh() {},
      fetch: async (url, init) => {
        requests.push({ url, body: JSON.parse(init.body) });
        return { ok: true, json: async () => ({ pane: '%42' }) };
      },
    });
  vm.runInContext(
    `${slice('  const answerable =', '  const current =')}\n${functionSource('draft')}\n${functionSource('oneLine')}\n` +
      `${slice('  const noteValue =', '  function pick')}\n` +
      `${['selectSegment', 'setDispatchMode', 'openDispatch'].map((name) => functionSource(name)).join('\n')}\n` +
      `${['api', 'act', 'settle', 'submitDispatch'].map((name) => functionSource(name, true)).join('\n')}\n` +
      `${slice("  $('#dispatch-note').addEventListener('input'", "  $('#dispatch').addEventListener('keydown'")}\n` +
      'Object.assign(globalThis, { draft, openDispatch, setDispatchMode, submitDispatch });',
    context,
  );
  return {
    context,
    state,
    elements,
    requests,
    cleared,
    engine: () => elements['#dispatch-engine'].value,
    // Types into the sheet's note field the way the browser does.
    type(text) {
      elements['#dispatch-note'].value = text;
      listeners.input({ target: elements['#dispatch-note'] });
    },
    // Launches and returns the one request the sheet sent.
    async launch() {
      requests.length = 0;
      await context.submitDispatch({ preventDefault() {} });
      assert.equal(requests.length, 1, 'Launch sends one request');
      return requests[0];
    },
  };
}

const report = { id: 'report', kind: 'info', title: 'Report', source: { pane: '%5' }, star: { action: 'Add both as read collaborators.' } },
  dshReport = { id: 'dsh-report', kind: 'info', title: 'dsh report', engine: 'dsh', source: { uri: 'dsh://session/abc' } },
  finding = { id: 'finding', kind: 'finding', title: 'Finding', source: { uri: 'urn:rule' }, options: [{ label: 'Fix it' }] };

// A next-step button opens the sheet. The sheet then asks the daemon for a session of its own.
test('the next step opens the sheet and launches in a new window', async () => {
  const ui = sheet([report]);
  ui.context.openDispatch(report, 'act', { newWindow: true });
  assert.equal(ui.state.dispatchId, report.id);
  assert.equal(ui.elements['#dispatch-note'].value, actDefault, 'Act opens on the default instruction');
  const sent = await ui.launch();
  assert.equal(sent.url, '/v1/cards/report/dispatch');
  assert.deepEqual(sent.body, { mode: 'act', note: null, engine: 'omp', model: null, thinking: null, new_window: true });
  assert.equal(ui.context.draft(report).note, '', 'the default is not recorded as the card note');

  // Dispatch from the same card keeps its own behavior and asks for no new window.
  ui.state.cards.push(report);
  ui.context.openDispatch(report, 'investigate');
  assert.equal((await ui.launch()).body.new_window, false);
});

// An untouched mode default must not become the card's note, or a later Acknowledge or
// Investigate would send boilerplate the user never wrote.
test('the Act default clears for another mode and a typed instruction replaces it', () => {
  const ui = sheet([report, finding]);
  ui.context.openDispatch(report, 'act');
  const note = ui.elements['#dispatch-note'];
  assert.equal(note.value, actDefault, 'Act opens on the default direction');
  ui.context.setDispatchMode('plan');
  assert.equal(note.value, '', 'leaving Act clears an untouched default');
  assert.equal(ui.context.draft(report).note, '', 'the default never becomes the card note');
  ui.context.setDispatchMode('act');
  assert.equal(note.value, actDefault, 'Act restores the default');

  ui.context.openDispatch(finding, 'act');
  ui.type('Only the staging rule');
  assert.equal(ui.context.draft(finding).defaulted, false, 'typing takes over the instruction');
  ui.context.setDispatchMode('investigate');
  assert.equal(note.value, 'Only the staging rule', 'a typed instruction stays across modes');
});

// Investigate + fix starts on the longer direction, and each default belongs to its mode.
test('the Investigate + fix default follows its mode and clears like the Act one', () => {
  const ui = sheet([report]);
  ui.context.openDispatch(report, 'investigate-fix');
  const note = ui.elements['#dispatch-note'];
  assert.equal(
    note.value,
    `${actDefault}\n\nInvestigate and fix the root cause permanently and generically, not just this one problem. Make sure you don't break anything and that this type of problem does not occur again.`,
    'Investigate + fix opens on its own direction',
  );
  assert.equal(ui.context.draft(report).note, '', 'the default never becomes the card note');
  ui.context.setDispatchMode('act');
  assert.equal(note.value, actDefault, 'Act puts back its shorter default');
  ui.context.setDispatchMode('investigate-fix');
  assert.ok(note.value.includes('root cause'), 'Investigate + fix puts back its own default');
  ui.context.setDispatchMode('plan');
  assert.equal(note.value, '', 'another mode clears the untouched default');
  ui.type('Only the config');
  ui.context.setDispatchMode('investigate-fix');
  assert.equal(note.value, 'Only the config', 'a typed instruction survives the switch');
});

test('the sheet opens on the engine of the card session, and on the last pick for a card without one', () => {
  const ui = sheet([dshReport, report, finding]);
  ui.context.openDispatch(dshReport, 'investigate');
  assert.equal(ui.engine(), 'dsh', 'a dsh card continues in dsh');
  ui.context.openDispatch(report, 'investigate');
  assert.equal(ui.engine(), 'omp', 'a tmux card after a dsh card is back on omp');
  ui.context.openDispatch(finding, 'investigate');
  assert.equal(ui.engine(), '', 'a card without a session keeps Default, the last pick');
  assert.deepEqual(ui.cleared, ['dsh', 'omp'], 'the model list resets only when the engine in effect changes');

  // The picker stores the engine the user picks. Only cards without a session follow it.
  ui.state.dispatchEngine = 'dsh';
  ui.context.openDispatch(finding, 'investigate');
  assert.equal(ui.engine(), 'dsh');
  ui.context.openDispatch(report, 'investigate');
  assert.equal(ui.engine(), 'omp');
});

test('the card and the sheet share one note', async () => {
  const ui = sheet([report, finding]);
  ui.context.draft(report).note = 'Check the logs first';
  ui.context.openDispatch(report, 'investigate');
  assert.equal(ui.elements['#dispatch-note'].value, 'Check the logs first', 'the sheet opens on the card note');
  ui.type('Check the logs.\nThen the cache.');
  assert.equal(ui.context.draft(report).note, 'Check the logs.\nThen the cache.');
  assert.equal(ui.elements['#note'].value, 'Check the logs. Then the cache.', 'the card field follows the sheet on one line');
  assert.equal((await ui.launch()).body.note, 'Check the logs.\nThen the cache.', 'Launch keeps the line breaks');

  // A finding option launches through its answer with the same note.
  ui.state.shownId = finding.id;
  ui.context.draft(finding).note = 'Only the staging rule';
  ui.context.openDispatch(finding, 'act', { option: 0 });
  const sent = await ui.launch();
  assert.equal(sent.url, '/v1/cards/finding/answer');
  assert.deepEqual(sent.body, { option: 0, text: null, mode: 'act', note: 'Only the staging rule', engine: null, model: null, thinking: null });
});

test('Launch sends the mode picked in the sheet', async () => {
  const plan = sheet([finding]);
  plan.context.openDispatch(finding, 'investigate');
  plan.context.setDispatchMode('plan');
  assert.equal((await plan.launch()).body.mode, 'plan');

  const fix = sheet([finding]);
  fix.context.openDispatch(finding, 'investigate-fix');
  assert.equal((await fix.launch()).body.mode, 'investigate-fix');
});

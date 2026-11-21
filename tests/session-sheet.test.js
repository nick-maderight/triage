// The session sheet is the one sheet for a new session and for a card's dispatch. These tests
// hold what it opens with, which modes it offers and what Start sends for either context.
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
const plain = (value) => JSON.parse(JSON.stringify(value));
assert.ok(actDefault, 'app.js defines the Act default direction');

// Loads the real sheet code: its opener, context strip, mode buttons, note field and Start.
// `daemon` is the engine the daemon launches by default.
function sheet(cards, daemon = 'omp') {
  const requests = [],
    cleared = [],
    listeners = {},
    modes = ['goal', 'investigate', 'investigate-fix', 'act', 'plan', 'plan-into'].map((mode) => ({
      dataset: { mode },
      hidden: false,
      attrs: {},
      setAttribute(name, value) {
        this.attrs[name] = value;
      },
    })),
    elements = {
      '#note': { value: '' },
      '#compose-prompt': { value: '', focus() {}, addEventListener: (type, listener) => (listeners[type] = listener) },
      '#compose-prompt-label': { textContent: '' },
      '#compose-hint': { textContent: '' },
      '#compose-start': { disabled: false },
      '#compose-context': { hidden: true },
      '#compose-context-title': { textContent: '' },
      '#compose-context-source': { textContent: '' },
      '#compose-target': { hidden: false },
      '#compose-session-pair': { hidden: false },
      '#compose-engine': { value: '' },
      '#compose-model': { value: '' },
      '#compose-thinking': { value: '' },
      '#compose-executor': { value: '' },
      '#compose-executor-effort': { value: '' },
      '#compose-cwd': { value: '' },
      '#compose-session': { value: '' },
      '#compose-mode': { children: modes },
      '#compose-mode-help': { textContent: '' },
      '#compose-models': { hidden: true },
      '#compose': {
        open: false,
        close() {
          this.open = false;
        },
      },
    },
    state = {
      cards: [...cards],
      counts: {},
      drafts: new Map(),
      shownId: cards[0]?.id ?? null,
      sheetCardId: null,
      sheetEngine: '',
      composeMode: 'goal',
      pending: null,
      remoteSwapUntil: 0,
      inflight: new Set(),
      actionRevision: 0,
      dirty: false,
      idx: 0,
      history: { loaded: true, error: null },
    },
    image = { value: null },
    context = vm.createContext({
      S: state,
      Date: { now: () => 1000 },
      window: { matchMedia: () => ({ matches: true }) },
      $: (selector) => {
        if (selector === 'dialog[open]') return elements['#compose'].open ? elements['#compose'] : null;
        return elements[selector] || null;
      },
      PICKERS: { 'compose-engine': { fallback: daemon }, 'compose-model': { options: [{}] } },
      setPickerValue: (id, value) => {
        elements[`#${id}`].value = value;
      },
      clearModels: () => cleared.push(elements['#compose-engine'].value),
      loadModels() {},
      loadComposeSessions() {},
      showDialog() {
        elements['#compose'].open = true;
      },
      titleOf: (c) => c.title,
      sourceName: (s) => s.label || 'unknown',
      dshOf: (c) =>
        c.engine === 'dsh' && c.source?.uri?.startsWith('dsh://session/')
          ? c.source.uri.slice(14)
          : null,
      snapshot: () => ({}),
      render() {},
      enc: encodeURIComponent,
      sheetImageBody: async () => image.value,
      resultMessage: () => '',
      announce() {},
      toast() {},
      refresh() {},
      fetch: async (url, init) => {
        requests.push({ url, body: JSON.parse(init.body) });
        return { ok: true, json: async () => ({ pane: '%42' }) };
      },
      act: async (_c, verb, body, control) => {
        requests.push({ url: `/v1/cards/${_c.id}/${verb}`, body, control });
        return true;
      },
    });
  vm.runInContext(
    `${slice('  const answerable =', '  const current =')}\n${functionSource('draft')}\n${functionSource('oneLine')}\n` +
      `${slice('  const noteValue =', '  function pick')}\n` +
      `${['selectSegment', 'sheetCard', 'sheetModes', 'setSheetMode', 'openSheet', 'clearSheetCard'].map((name) => functionSource(name)).join('\n')}\n` +
      `${['api', 'startSession', 'dispatchCard', 'submitSheet'].map((name) => functionSource(name, true)).join('\n')}\n` +
      `${slice("  $('#compose-prompt').addEventListener('input'", '  wireSheetImage();')}\n` +
      'Object.assign(globalThis, { draft, openSheet, clearSheetCard, setSheetMode, sheetModes, submitSheet });',
    context,
  );
  return {
    context,
    state,
    elements,
    requests,
    cleared,
    image,
    engine: () => elements['#compose-engine'].value,
    mode: () => modes.find((button) => button.attrs['aria-checked'] === 'true')?.dataset.mode,
    shownModes: () => modes.filter((button) => !button.hidden).map((button) => button.dataset.mode),
    // Types into the sheet's field the way the browser does.
    type(text) {
      elements['#compose-prompt'].value = text;
      listeners.input({ target: elements['#compose-prompt'] });
    },
    // Starts or dispatches and returns the one request the sheet sent.
    async launch() {
      requests.length = 0;
      await context.submitSheet({ preventDefault() {} });
      assert.equal(requests.length, 1, 'Start sends one request');
      return requests[0];
    },
  };
}

const report = { id: 'report', kind: 'info', title: 'Report', source: { pane: '%5' }, star: { action: 'Add both as read collaborators.' } },
  dshReport = { id: 'dsh-report', kind: 'info', title: 'dsh report', engine: 'dsh', source: { uri: 'dsh://session/abc' } },
  finding = { id: 'finding', kind: 'finding', title: 'Finding', source: { uri: 'urn:rule' }, options: [{ label: 'Fix it' }] };

// A next-step button opens the sheet with the card attached. Start then asks the daemon for a
// session of its own and sends no working directory or tmux session, because the card has both.
test('a card opens the sheet with its context and dispatches in a new window', async () => {
  const ui = sheet([report]);
  ui.context.openSheet(report, 'act', { newWindow: true });
  assert.equal(ui.state.sheetCardId, report.id);
  assert.equal(ui.elements['#compose-context'].hidden, false, 'the context strip shows the card');
  assert.equal(ui.elements['#compose-context-title'].textContent, 'Report');
  assert.equal(ui.elements['#compose-context-source'].textContent, 'unknown');
  assert.equal(ui.elements['#compose-prompt-label'].textContent, 'Instruction');
  assert.equal(ui.elements['#compose-target'].hidden, true, 'a card supplies its own directory and session');
  assert.equal(ui.elements['#compose-prompt'].value, actDefault, 'Act opens on the default instruction');
  assert.equal(ui.mode(), 'act');
  const sent = await ui.launch();
  assert.equal(sent.url, '/v1/cards/report/dispatch');
  assert.deepEqual(plain(sent.body), { mode: 'act', note: null, engine: 'omp', model: null, thinking: null, new_window: true });
  assert.equal(ui.context.draft(report).note, '', 'the default is not recorded as the card note');

  // Dispatch from the same card keeps its own behavior and asks for no new window.
  ui.state.cards.push(report);
  ui.context.openSheet(report, 'investigate');
  assert.equal((await ui.launch()).body.new_window, false);
});

// An untouched mode default must not become the card's note, or a later Acknowledge or
// Investigate would send boilerplate the user never wrote.
test('the Act default clears for another mode and a typed instruction replaces it', () => {
  const ui = sheet([report, finding]);
  ui.context.openSheet(report, 'act');
  const field = ui.elements['#compose-prompt'];
  assert.equal(field.value, actDefault, 'Act opens on the default direction');
  ui.context.setSheetMode('plan');
  assert.equal(field.value, '', 'leaving Act clears an untouched default');
  assert.equal(ui.context.draft(report).note, '', 'the default never becomes the card note');
  ui.context.setSheetMode('act');
  assert.equal(field.value, actDefault, 'Act restores the default');

  ui.context.openSheet(finding, 'act');
  ui.type('Only the staging rule');
  assert.equal(ui.context.draft(finding).defaulted, false, 'typing takes over the instruction');
  ui.context.setSheetMode('investigate');
  assert.equal(field.value, 'Only the staging rule', 'a typed instruction stays across modes');
});

// Investigate + fix starts on the longer direction, and each default belongs to its mode.
test('the Investigate + fix default follows its mode and clears like the Act one', () => {
  const ui = sheet([report]);
  ui.context.openSheet(report, 'investigate-fix');
  const field = ui.elements['#compose-prompt'];
  assert.equal(
    field.value,
    `${actDefault}\n\nInvestigate and fix the root cause permanently and generically, not just this one problem. Make sure you don't break anything and that this type of problem does not occur again.`,
    'Investigate + fix opens on its own direction',
  );
  assert.equal(ui.context.draft(report).note, '', 'the default never becomes the card note');
  ui.context.setSheetMode('act');
  assert.equal(field.value, actDefault, 'Act puts back its shorter default');
  ui.context.setSheetMode('investigate-fix');
  assert.ok(field.value.includes('root cause'), 'Investigate + fix puts back its own default');
  ui.context.setSheetMode('plan');
  assert.equal(field.value, '', 'another mode clears the untouched default');
  ui.type('Only the config');
  ui.context.setSheetMode('investigate-fix');
  assert.equal(field.value, 'Only the config', 'a typed instruction survives the switch');
});

// The one sheet hides the modes its context cannot use. A plain session offers every mode. A
// card drops Goal, and a finding's answers can only act on those answers.
test('the sheet offers only the modes its context can use', () => {
  const ui = sheet([report, finding]);
  ui.context.openSheet(null, 'goal');
  assert.deepEqual(ui.shownModes(), ['goal', 'investigate', 'investigate-fix', 'act', 'plan', 'plan-into']);
  assert.equal(ui.mode(), 'goal');
  ui.context.setSheetMode('investigate');
  assert.equal(ui.mode(), 'investigate', 'a plain session takes a dispatch mode');

  ui.context.openSheet(report, 'investigate');
  assert.deepEqual(ui.shownModes(), ['investigate', 'investigate-fix', 'act', 'plan', 'plan-into']);
  assert.equal(ui.mode(), 'investigate');

  ui.context.openSheet(finding, 'act', { answers: [{ option: 0 }] });
  assert.deepEqual(ui.shownModes(), ['act', 'plan', 'plan-into']);
  ui.context.setSheetMode('investigate');
  assert.equal(ui.mode(), 'act', 'an answered finding falls back to Act');
});

test('the sheet opens on the engine of the card session, and on the last pick for a card without one', () => {
  const ui = sheet([dshReport, report, finding]);
  ui.context.openSheet(dshReport, 'investigate');
  assert.equal(ui.engine(), 'dsh', 'a dsh card continues in dsh');
  ui.context.openSheet(report, 'investigate');
  assert.equal(ui.engine(), 'omp', 'a tmux card after a dsh card is back on omp');
  ui.context.openSheet(finding, 'investigate');
  assert.equal(ui.engine(), '', 'a card without a session keeps Default, the last pick');
  assert.deepEqual(ui.cleared, ['dsh', 'omp'], 'the model list resets only when the engine in effect changes');

  // The picker stores the engine the user picks. Only cards without a session follow it.
  ui.state.sheetEngine = 'dsh';
  ui.context.openSheet(finding, 'investigate');
  assert.equal(ui.engine(), 'dsh');
  ui.context.openSheet(report, 'investigate');
  assert.equal(ui.engine(), 'omp');
});

test('the card and the sheet share one note', async () => {
  const ui = sheet([report, finding]);
  ui.context.draft(report).note = 'Check the logs first';
  ui.context.openSheet(report, 'investigate');
  assert.equal(ui.elements['#compose-prompt'].value, 'Check the logs first', 'the sheet opens on the card note');
  ui.type('Check the logs.\nThen the cache.');
  assert.equal(ui.context.draft(report).note, 'Check the logs.\nThen the cache.');
  assert.equal(ui.elements['#note'].value, 'Check the logs. Then the cache.', 'the card field follows the sheet on one line');
  assert.equal((await ui.launch()).body.note, 'Check the logs.\nThen the cache.', 'Start keeps the line breaks');

  // A finding option launches through its answer with the same note.
  ui.state.shownId = finding.id;
  ui.context.draft(finding).note = 'Only the staging rule';
  ui.context.openSheet(finding, 'act', { option: 0 });
  const sent = await ui.launch();
  assert.equal(sent.url, '/v1/cards/finding/answer');
  assert.deepEqual(plain(sent.body), { option: 0, text: null, mode: 'act', note: 'Only the staging rule', engine: null, model: null, thinking: null });
});

test('Start sends the mode picked in the sheet', async () => {
  const plan = sheet([finding]);
  plan.context.openSheet(finding, 'investigate');
  plan.context.setSheetMode('plan');
  assert.equal((await plan.launch()).body.mode, 'plan');

  const fix = sheet([finding]);
  fix.context.openSheet(finding, 'investigate-fix');
  assert.equal((await fix.launch()).body.mode, 'investigate-fix');
});

// Clear drops the card and leaves the same sheet as a plain session, with the card's note gone.
test('Clear turns the card sheet back into a plain new session', async () => {
  const ui = sheet([report]);
  ui.context.openSheet(report, 'act', { newWindow: true });
  ui.type('Change the cache');
  ui.context.clearSheetCard();
  assert.equal(ui.state.sheetCardId, null);
  assert.equal(ui.elements['#compose-context'].hidden, true, 'the context strip goes');
  assert.equal(ui.elements['#compose-prompt-label'].textContent, 'What should the session do?');
  assert.equal(ui.elements['#compose-target'].hidden, false, 'a plain session picks its own directory and session');
  assert.equal(ui.elements['#compose-prompt'].value, '');
  assert.deepEqual(ui.shownModes(), ['goal', 'investigate', 'investigate-fix', 'act', 'plan', 'plan-into']);
  assert.equal(ui.mode(), 'goal');
  assert.equal(ui.elements['#compose-models'].hidden, true);

  ui.elements['#compose-prompt'].value = 'Start something new';
  const sent = await ui.launch();
  assert.equal(sent.url, '/v1/sessions');
  assert.deepEqual(plain(sent.body), {
    prompt: 'Start something new',
    cwd: null,
    session: null,
    engine: null,
    model: null,
    thinking: null,
    mode: 'goal',
  });
});

// A plain session on a dispatch mode treats its text as the context, the way a card dispatch does.
test('a plain session sends its mode as a context dispatch', async () => {
  const ui = sheet([report]);
  ui.context.openSheet(null, 'goal');
  ui.type('Explore the cache layer');
  ui.context.setSheetMode('investigate');
  const sent = await ui.launch();
  assert.equal(sent.url, '/v1/sessions');
  assert.equal(sent.body.mode, 'investigate', 'the sheet keeps the mode it shows');
  assert.equal(sent.body.note, '', 'a dispatch mode sends an empty note so the text is context');

  const act = sheet([report]);
  act.context.openSheet(null, 'act');
  act.type('Wire the retry');
  assert.deepEqual(plain((await act.launch()).body), {
    prompt: 'Wire the retry',
    cwd: null,
    session: null,
    engine: null,
    model: null,
    thinking: null,
    mode: 'act',
    note: '',
  });
});

test('an attached image rides a card dispatch', async () => {
  const ui = sheet([report]),
    image = { media_type: 'image/png', data: 'iVBORw0KGgo=', name: 'shot.png' };
  ui.image.value = image;
  ui.context.openSheet(report, 'investigate');
  const sent = await ui.launch();
  assert.deepEqual(plain(sent.body), {
    mode: 'investigate',
    note: null,
    engine: 'omp',
    model: null,
    thinking: null,
    image,
    new_window: false,
  });
});

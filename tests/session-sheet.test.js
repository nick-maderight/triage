// The session sheet is the one sheet for a new session and for a card's dispatch. These tests
// hold what it opens with, which choices it offers and what Start sends for either context.
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
const plain = (value) => JSON.parse(JSON.stringify(value));

// Loads the real sheet code: its opener, context strip, mode buttons, note field and Start.
// `daemon` is the engine the daemon launches by default.
function sheet(cards, daemon = 'omp') {
  const requests = [],
    cleared = [],
    listeners = {},
    button = (value) => ({
      dataset: { value },
      hidden: false,
      attrs: {},
      setAttribute(name, next) {
        this.attrs[name] = next;
      },
    }),
    modes = ['goal', 'plan', 'plan-into'].map(button),
    tasks = ['investigate', 'fix', 'act', 'verify', 'simplify'].map(button),
    elements = {
      '#note': { value: '' },
      '#compose-prompt': { value: '', focus() {}, addEventListener: (type, listener) => (listeners[type] = listener) },
      '#compose-prompt-label': { textContent: '' },
      '#compose-form': { focus() {} },
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
      '#compose-task': { children: tasks },
      '#compose-task-help': { textContent: '' },
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
      composeTask: 'fix',
      composeNote: '',
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
      `${['selectSegment', 'sheetCard', 'sheetTasks', 'setSheetMode', 'setSheetTask', 'openSheet', 'clearSheetCard'].map((name) => functionSource(name)).join('\n')}\n` +
      `${['api', 'startSession', 'dispatchCard', 'submitSheet'].map((name) => functionSource(name, true)).join('\n')}\n` +
      `${slice("  $('#compose-prompt').addEventListener('input'", '  wireSheetImage();')}\n` +
      'Object.assign(globalThis, { draft, openSheet, clearSheetCard, setSheetMode, setSheetTask, sheetTasks, submitSheet });',
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
    mode: () => modes.find((button) => button.attrs['aria-checked'] === 'true')?.dataset.value,
    task: () => tasks.find((button) => button.attrs['aria-checked'] === 'true')?.dataset.value,
    shownModes: () => modes.filter((button) => !button.hidden).map((button) => button.dataset.value),
    shownTasks: () => tasks.filter((button) => !button.hidden).map((button) => button.dataset.value),
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
  ui.context.openSheet(report, 'goal', 'act', { newWindow: true });
  assert.equal(ui.state.sheetCardId, report.id);
  assert.equal(ui.elements['#compose-context'].hidden, false, 'the context strip shows the card');
  assert.equal(ui.elements['#compose-context-title'].textContent, 'Report');
  assert.equal(ui.elements['#compose-context-source'].textContent, 'unknown');
  assert.equal(ui.elements['#compose-prompt-label'].textContent, 'Instruction');
  assert.equal(ui.elements['#compose-target'].hidden, true, 'a card supplies its own directory and session');
  assert.equal(ui.elements['#compose-prompt'].value, '', 'a task never fills the Instruction');
  assert.equal(ui.mode(), 'goal');
  assert.equal(ui.task(), 'act');
  const sent = await ui.launch();
  assert.equal(sent.url, '/v1/cards/report/dispatch');
  assert.deepEqual(plain(sent.body), { mode: 'goal', task: 'act', note: null, engine: 'omp', model: null, thinking: null, new_window: true });
  assert.equal(ui.context.draft(report).note, '', 'an empty Instruction is no card note');

  // Dispatch from the same card keeps its own behavior and asks for no new window.
  ui.state.cards.push(report);
  ui.context.openSheet(report, 'goal', 'investigate');
  assert.equal((await ui.launch()).body.new_window, false);
});

// A New session sheet keeps its text when it closes, as the Emacs sheet does. A card's sheet in
// between shows the card's note, and a started session leaves no text behind.
test('a closed New session sheet keeps its text until a session starts', async () => {
  const ui = sheet([report]);
  ui.context.openSheet(null, 'goal', 'fix');
  ui.type('Check the backups');
  ui.elements['#compose'].close();
  ui.context.openSheet(report, 'goal', 'act');
  assert.equal(ui.elements['#compose-prompt'].value, '', 'the card sheet shows the card note');
  ui.elements['#compose'].close();
  ui.context.openSheet(null, 'goal', 'fix');
  assert.equal(ui.elements['#compose-prompt'].value, 'Check the backups');
  await ui.launch();
  ui.context.openSheet(null, 'goal', 'fix');
  assert.equal(ui.elements['#compose-prompt'].value, '', 'a started session leaves no text');
});

// A task only says what the session is asked to do. It never fills the Instruction, so an empty
// Instruction stays empty for every task.
test('a task never writes into the Instruction', async () => {
  const ui = sheet([report]);
  ui.context.openSheet(report, 'goal', 'investigate');
  const field = ui.elements['#compose-prompt'];
  ui.context.setSheetTask('fix');
  assert.equal(ui.task(), 'fix');
  assert.equal(field.value, '', 'Fix leaves the Instruction empty');
  ui.context.setSheetTask('act');
  assert.equal(field.value, '', 'Act leaves the Instruction empty');
  assert.equal(ui.context.draft(report).note, '');
  const sent = await ui.launch();
  assert.equal(sent.body.mode, 'goal');
  assert.equal(sent.body.task, 'act');
  assert.equal(sent.body.note, null, 'an empty Instruction sends no note');
});

test('a typed instruction survives a mode or task switch', () => {
  const ui = sheet([report, finding]);
  ui.context.openSheet(finding, 'goal', 'act');
  ui.type('Only the staging rule');
  ui.context.setSheetMode('plan');
  ui.context.setSheetTask('verify');
  assert.equal(ui.elements['#compose-prompt'].value, 'Only the staging rule');
  assert.equal(ui.context.draft(finding).note, 'Only the staging rule');
});

// Mode and Task are separate rows. A plain session offers every choice. A card offers every task
// too. A finding's answers take every task but Investigate, which their own decisions settle.
test('the sheet offers every mode, and every task but Investigate for a finding answer', () => {
  const ui = sheet([report, finding]);
  ui.context.openSheet(null, 'goal', 'fix');
  assert.deepEqual(ui.shownModes(), ['goal', 'plan', 'plan-into']);
  assert.deepEqual(ui.shownTasks(), ['investigate', 'fix', 'act', 'verify', 'simplify']);
  ui.context.setSheetMode('plan-into');
  assert.equal(ui.elements['#compose-models'].hidden, false, 'plan into shows the execution fields');
  ui.context.setSheetMode('goal');
  assert.equal(ui.elements['#compose-models'].hidden, true);

  ui.context.openSheet(report, 'goal', 'investigate');
  assert.deepEqual(ui.shownTasks(), ['investigate', 'fix', 'act', 'verify', 'simplify']);

  ui.context.openSheet(finding, 'goal', 'act', { answers: [{ option: 0 }] });
  assert.deepEqual(ui.shownModes(), ['goal', 'plan', 'plan-into']);
  assert.deepEqual(ui.shownTasks(), ['fix', 'act', 'verify', 'simplify']);
  ui.context.setSheetTask('investigate');
  assert.equal(ui.task(), 'act', 'an answered finding falls back to Act');
});

test('the sheet opens on the engine of the card session, and on the last pick for a card without one', () => {
  const ui = sheet([dshReport, report, finding]);
  ui.context.openSheet(dshReport, 'goal', 'investigate');
  assert.equal(ui.engine(), 'dsh', 'a dsh card continues in dsh');
  ui.context.openSheet(report, 'goal', 'investigate');
  assert.equal(ui.engine(), 'omp', 'a tmux card after a dsh card is back on omp');
  ui.context.openSheet(finding, 'goal', 'investigate');
  assert.equal(ui.engine(), '', 'a card without a session keeps Default, the last pick');
  assert.deepEqual(ui.cleared, ['dsh', 'omp'], 'the model list resets only when the engine in effect changes');

  // The picker stores the engine the user picks. Only cards without a session follow it.
  ui.state.sheetEngine = 'dsh';
  ui.context.openSheet(finding, 'goal', 'investigate');
  assert.equal(ui.engine(), 'dsh');
  ui.context.openSheet(report, 'goal', 'investigate');
  assert.equal(ui.engine(), 'omp');
});

test('the card and the sheet share one note', async () => {
  const ui = sheet([report, finding]);
  ui.context.draft(report).note = 'Check the logs first';
  ui.context.openSheet(report, 'goal', 'investigate');
  assert.equal(ui.elements['#compose-prompt'].value, 'Check the logs first', 'the sheet opens on the card note');
  ui.type('Check the logs.\nThen the cache.');
  assert.equal(ui.context.draft(report).note, 'Check the logs.\nThen the cache.');
  assert.equal(ui.elements['#note'].value, 'Check the logs. Then the cache.', 'the card field follows the sheet on one line');
  assert.equal((await ui.launch()).body.note, 'Check the logs.\nThen the cache.', 'Start keeps the line breaks');

  // A finding option launches through its answer with the same note.
  ui.state.shownId = finding.id;
  ui.context.draft(finding).note = 'Only the staging rule';
  ui.context.openSheet(finding, 'goal', 'act', { option: 0 });
  const sent = await ui.launch();
  assert.equal(sent.url, '/v1/cards/finding/answer');
  assert.deepEqual(plain(sent.body), { option: 0, text: null, mode: 'goal', task: 'act', note: 'Only the staging rule', engine: null, model: null, thinking: null });
});

test('Start sends the mode and the task picked in the sheet', async () => {
  const plan = sheet([finding]);
  plan.context.openSheet(finding, 'goal', 'investigate');
  plan.context.setSheetMode('plan');
  assert.equal((await plan.launch()).body.mode, 'plan');

  const fix = sheet([finding]);
  fix.context.openSheet(finding, 'goal', 'investigate');
  fix.context.setSheetTask('fix');
  const sent = await fix.launch();
  assert.equal(sent.body.mode, 'goal');
  assert.equal(sent.body.task, 'fix');
});

// Clear drops the card and leaves the same sheet as a plain session, with the card's note gone.
test('Clear turns the card sheet back into a plain new session', async () => {
  const ui = sheet([report]);
  ui.context.openSheet(report, 'goal', 'act', { newWindow: true });
  ui.type('Change the cache');
  ui.context.clearSheetCard();
  assert.equal(ui.state.sheetCardId, null);
  assert.equal(ui.elements['#compose-context'].hidden, true, 'the context strip goes');
  assert.equal(ui.elements['#compose-prompt-label'].textContent, 'What should the session do?');
  assert.equal(ui.elements['#compose-target'].hidden, false, 'a plain session picks its own directory and session');
  assert.equal(ui.elements['#compose-prompt'].value, '');
  assert.deepEqual(ui.shownModes(), ['goal', 'plan', 'plan-into']);
  assert.deepEqual(ui.shownTasks(), ['investigate', 'fix', 'act', 'verify', 'simplify']);
  assert.equal(ui.mode(), 'goal');
  assert.equal(ui.task(), 'fix', 'Clear restores the last plain-session task');
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
    task: 'fix',
  });
});

// A plain session sends its mode and its task, and no note. The task is the only part of the
// brief that a task changes.
test('a plain session sends its mode and task with no note', async () => {
  const ui = sheet([report]);
  ui.context.openSheet(null, 'goal', 'investigate');
  ui.type('Explore the cache layer');
  ui.context.setSheetMode('plan');
  const sent = await ui.launch();
  assert.equal(sent.url, '/v1/sessions');
  assert.equal(sent.body.mode, 'plan', 'the sheet keeps the mode it shows');
  assert.equal(sent.body.task, 'investigate');
  assert.equal('note' in sent.body, false, 'a plain session never sends a note');

  const act = sheet([report]);
  act.context.openSheet(null, 'goal', 'act');
  act.type('Wire the retry');
  assert.deepEqual(plain((await act.launch()).body), {
    prompt: 'Wire the retry',
    cwd: null,
    session: null,
    engine: null,
    model: null,
    thinking: null,
    mode: 'goal',
    task: 'act',
  });
});

test('an attached image rides a card dispatch', async () => {
  const ui = sheet([report]),
    image = { media_type: 'image/png', data: 'iVBORw0KGgo=', name: 'shot.png' };
  ui.image.value = image;
  ui.context.openSheet(report, 'goal', 'investigate');
  const sent = await ui.launch();
  assert.deepEqual(plain(sent.body), {
    mode: 'goal',
    task: 'investigate',
    note: null,
    engine: 'omp',
    model: null,
    thinking: null,
    image,
    new_window: false,
  });
});

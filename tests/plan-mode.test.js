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

const keydownStart = app.indexOf("  document.addEventListener('keydown', (e) => {"),
  keydownEnd = app.indexOf('\n  });', keydownStart);
assert.ok(keydownStart >= 0 && keydownEnd > keydownStart, 'app.js defines the global key handler');
const keydownSource = app.slice(keydownStart, keydownEnd + '\n  });'.length);
// The handler routes every card key through the action table, which runs beside it here.
const tableStart = app.indexOf('  const answerable ='),
  tableEnd = app.indexOf('  const current =', tableStart);
assert.ok(tableStart >= 0 && tableEnd > tableStart, 'app.js defines the action table');
const tableSources = app.slice(tableStart, tableEnd);

const modes = (list) =>
  list.map((value) => ({
    dataset: { value },
    hidden: false,
    attrs: { 'aria-checked': String(value === list[0]) },
    tabIndex: value === list[0] ? 0 : -1,
    setAttribute(name, next) {
      this.attrs[name] = next;
    },
    getAttribute(name) {
      return this.attrs[name];
    },
  }));
const checked = (group) => group.find((button) => button.getAttribute('aria-checked') === 'true').dataset.value;

test('i, A, p, P and g open the session sheet with matching modes and tasks while v keeps Peek', () => {
  const card = { id: 'card-1', kind: 'plan', title: 'Plan ready: Update tests', source: { pane: '%7' } },
    modeButtons = modes(['goal', 'plan', 'plan-into']),
    taskButtons = modes(['investigate', 'fix', 'act', 'verify', 'simplify']),
    dialog = { open: false },
    elements = {
      '#peek': { open: false },
      '#compose-mode': { children: modeButtons },
      '#compose-mode-help': { textContent: '' },
      '#compose-task': { children: taskButtons },
      '#compose-task-help': { textContent: '' },
      '#compose-hint': { textContent: '' },
      '#compose-prompt': { value: '', focus() {} },
      '#compose-prompt-label': { textContent: '' },
      '#compose-context': { hidden: true },
      '#compose-context-title': { textContent: '' },
      '#compose-context-source': { textContent: '' },
      '#compose-target': { hidden: false },
      '#compose-session-pair': { hidden: false },
      '#compose-models': { hidden: true },
      '#compose-engine': { value: '' },
      '#compose-executor-effort': { value: 'max' },
      '#compose-start': { disabled: false },
      '#compose': dialog,
    },
    listeners = {},
    actions = [],
    state = {
      cards: [card],
      idx: 0,
      pending: null,
      remoteSwapUntil: 0,
      laterOpen: false,
      countsExpanded: false,
      sheetCardId: null,
      sheetEngine: '',
      composeMode: 'goal',
      composeTask: 'fix',
      drafts: new Map([[card.id, { dispatchMode: 'goal', dispatchTask: 'investigate', note: 'Review the plan', dispatchAnswers: null }]]),
    },
    context = vm.createContext({
      S: state,
      PICKERS: { 'compose-engine': { fallback: 'omp' }, 'compose-model': { options: [] } },
      loadModels() {},
      loadComposeSessions() {},
      clearModels() {},
      setPickerValue: (id, value) => {
        elements[`#${id}`].value = value;
      },
      titleOf: (current) => current.title,
      sourceName: () => 'unknown',
      dshOf: () => null,
      enc: encodeURIComponent,
      Date: { now: () => 1000 },
      document: {
        addEventListener(type, listener) {
          listeners[type] = listener;
        },
        querySelector(selector) {
          return selector === 'dialog[open]' && dialog.open ? dialog : null;
        },
      },
      $: (selector) => elements[selector] || null,
      textField: () => false,
      current: () => card,
      draft: (current) => state.drafts.get(current.id),
      showDialog: (current) => {
        current.open = true;
      },
      openPeek: (current) => actions.push(['peek', current.id]),
      pick: (current, index) => actions.push(['pick', current.id, index]),
      primary: (current) => actions.push(['primary', current.id]),
    });
  vm.runInContext(
    `${tableSources}\n${functionSource('selectSegment')}\n${functionSource('sheetCard')}\n${functionSource('sheetTasks')}\n${functionSource('setSheetMode')}\n${functionSource('setSheetTask')}\n${functionSource('openSheet')}\n${keydownSource}`,
    context,
  );

  const press = (key) => {
    const event = {
      key,
      metaKey: false,
      ctrlKey: false,
      altKey: false,
      isComposing: false,
      target: { closest: () => null },
      preventDefault() {
        this.defaultPrevented = true;
      },
    };
    listeners.keydown(event);
    return event;
  };
  const selected = () => checked(modeButtons);
  const task = () => checked(taskButtons);

  press('i');
  assert.equal(dialog.open, true);
  assert.equal(state.sheetCardId, card.id);
  assert.equal(selected(), 'goal');
  assert.equal(task(), 'investigate');
  dialog.open = false;
  press('A');
  assert.equal(selected(), 'goal');
  assert.equal(task(), 'act');
  assert.equal(elements['#compose-models'].hidden, true);
  dialog.open = false;
  press('p');
  assert.equal(selected(), 'plan');
  assert.equal(task(), 'act', 'Plan opens on Act');
  assert.equal(state.drafts.get(card.id).dispatchMode, 'plan');
  assert.equal(state.drafts.get(card.id).dispatchTask, 'act');
  assert.match(elements['#compose-mode-help'].textContent, /plan/i);
  dialog.open = false;
  press('P');
  assert.equal(selected(), 'plan-into');
  assert.equal(task(), 'act', 'Plan into opens on Act');
  assert.equal(state.drafts.get(card.id).dispatchMode, 'plan-into');
  assert.equal(elements['#compose-models'].hidden, false);
  assert.match(elements['#compose-mode-help'].textContent, /without review/);
  dialog.open = false;
  press('A');
  assert.equal(selected(), 'goal');
  assert.equal(task(), 'act');
  assert.equal(elements['#compose-models'].hidden, true);
  dialog.open = false;
  // g has no card to dispatch, so it opens a plain session on Goal with the remembered task.
  press('g');
  assert.equal(dialog.open, true);
  assert.equal(state.sheetCardId, null);
  assert.equal(elements['#compose-prompt-label'].textContent, 'What should the session do?');
  assert.equal(elements['#compose-target'].hidden, false);
  assert.equal(selected(), 'goal');
  assert.equal(task(), 'fix');
  dialog.open = false;
  press('v');
  press('2');
  press('Enter');
  assert.deepEqual(actions, [
    ['peek', card.id],
    ['pick', card.id, 1],
    ['primary', card.id],
  ]);
});

test('a acknowledges the shown card even when a button holds focus', () => {
  const actions = [],
    listeners = {},
    state = {
      cards: [],
      idx: 0,
      pending: null,
      remoteSwapUntil: 0,
      laterOpen: false,
      countsExpanded: false,
      sheetCardId: null,
      composeMode: 'goal',
      drafts: new Map(),
    };
  let card = { id: 'info-card', kind: 'info', title: 'Report' };
  const context = vm.createContext({
      S: state,
      $: (selector) => (selector === '#peek' ? { open: false } : null),
      document: {
        addEventListener(type, listener) {
          listeners[type] = listener;
        },
        querySelector: () => null,
      },
      textField: () => false,
      current: () => card,
      Date: { now: () => 1000 },
      primary: (current) => actions.push(['primary', current.id]),
      enc: encodeURIComponent,
      noteValue: () => null,
      act: (current, _verb, _body, control) => actions.push([control, current.id]),
      openSheet: (current, mode, task, { newWindow = false } = {}) =>
        actions.push(['dispatch', current.id, mode, task, newWindow]),
    });
  vm.runInContext(`${tableSources}\n${keydownSource}`, context);

  const press = (key, onButton) => {
    const event = {
      key,
      metaKey: false,
      ctrlKey: false,
      altKey: false,
      isComposing: false,
      defaultPrevented: false,
      target: { closest: (selector) => (onButton && selector === 'button,a,select' ? {} : null) },
      preventDefault() {
        this.defaultPrevented = true;
      },
    };
    listeners.keydown(event);
    return event;
  };

  const focusedA = press('a', true);
  assert.deepEqual(actions, [['ack', 'info-card']], 'a acknowledges rather than running the primary');
  assert.equal(focusedA.defaultPrevented, true, 'a works while a button holds focus');
  const focusedEnter = press('Enter', true);
  assert.deepEqual(actions, [['ack', 'info-card']], 'Enter on a button belongs to that button');
  assert.equal(focusedEnter.defaultPrevented, false);
  press('Enter', false);
  press('a', false);
  assert.deepEqual(actions, [
    ['ack', 'info-card'],
    ['primary', 'info-card'],
    ['ack', 'info-card'],
  ]);
  // A card with no Acknowledge action never answers because `a` was pressed.
  card = { id: 'ask-card', kind: 'ask', title: 'Pick one', options: [{ label: 'Red' }] };
  press('a', false);
  assert.deepEqual(actions.at(-1), ['ack', 'info-card'], 'a does nothing on an ask card');
  // The same `p` plans a report's open step in its own session and opens the sheet with Plan on
  // any other card.
  card = { id: 'report-card', kind: 'info', star: { action: 'Fix it' } };
  press('p');
  assert.deepEqual(actions.at(-1), ['dispatch', 'report-card', 'plan', 'act', true]);
  card = { id: 'plan-card', kind: 'plan', title: 'Plan' };
  press('p');
  assert.deepEqual(actions.at(-1), ['dispatch', 'plan-card', 'plan', 'act', false]);
});

test('the session sheet posts model and thinking for every card mode', async () => {
  const card = { id: 'launch-card', kind: 'finding', title: 'Launch work' },
    planInto = { id: 'plan-into-card', kind: 'finding', title: 'Launch work' },
    state = {
      cards: [card, planInto],
      counts: { finding: 2 },
      drafts: new Map([
        [card.id, { dispatchMode: 'plan', dispatchTask: 'act', note: '' }],
        [planInto.id, { dispatchMode: 'plan-into', dispatchTask: 'act', note: '' }],
      ]),
      sheetCardId: card.id,
      pending: null,
      remoteSwapUntil: 0,
      inflight: new Set(),
      actionRevision: 0,
      dirty: false,
      idx: 0,
      history: { loaded: true, error: null },
    },
    requests = [],
    start = { disabled: false },
    hint = { textContent: '' },
    dialog = { close() {} },
    elements = {
      '#compose-start': start,
      '#compose-hint': hint,
      '#compose-prompt': { value: 'Make a plan' },
      '#compose-model': { value: 'provider/planner' },
      '#compose-engine': { value: 'dsh' },
      '#compose-thinking': { value: 'high' },
      '#compose-executor': { value: '' },
      '#compose-executor-effort': { value: '' },
      '#compose': dialog,
    },
    context = vm.createContext({
      S: state,
      $: (selector) => elements[selector] || null,
      draft: (current) => state.drafts.get(current.id),
      noteValue: (current) => state.drafts.get(current.id).note.trim() || null,
      sheetImageBody: async () => null,
      act: async (current, verb, body) => {
        requests.push({ url: `/v1/cards/${current.id}/${verb}`, body });
        return true;
      },
    });
  vm.runInContext(
    `${functionSource('sheetCard')}\n${functionSource('dispatchCard', true)}\n${functionSource('submitSheet', true)}\nglobalThis.submitSheet = submitSheet;`,
    context,
  );

  await context.submitSheet({ preventDefault() {} });
  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, '/v1/cards/launch-card/dispatch');
  assert.deepEqual(JSON.parse(JSON.stringify(requests[0].body)), {
    mode: 'plan',
    task: 'act',
    note: 'Make a plan',
    engine: 'dsh',
    model: 'provider/planner',
    thinking: 'high',
    new_window: false,
  });

  requests.length = 0;
  state.sheetCardId = planInto.id;
  elements['#compose-executor-effort'].value = 'max';
  await context.submitSheet({ preventDefault() {} });
  assert.equal(requests.length, 1);
  assert.deepEqual(JSON.parse(JSON.stringify(requests[0].body)), {
    mode: 'plan-into',
    task: 'act',
    engine: 'dsh',
    note: 'Make a plan',
    model: 'provider/planner',
    thinking: 'high',
    executor: null,
    executor_thinking: 'max',
    new_window: false,
  });
});

test('finding options open the session sheet and answer with its choices', async () => {
  const card = {
      id: 'finding-card', kind: 'finding', options: [
        { label: 'Do as recommended', recommended: true },
        { label: 'Investigate first' },
      ],
    },
    modeButtons = modes(['goal', 'plan', 'plan-into']),
    taskButtons = modes(['investigate', 'fix', 'act', 'verify', 'simplify']),
    dialog = { open: false, close() { this.open = false; } },
    start = { disabled: false },
    hint = { textContent: '' },
    elements = {
      '#compose-mode': { children: modeButtons },
      '#compose-mode-help': { textContent: '' },
      '#compose-task': { children: taskButtons },
      '#compose-task-help': { textContent: '' },
      '#compose-models': { hidden: true },
      '#compose-prompt': { value: '', focus() {} },
      '#compose-prompt-label': { textContent: '' },
      '#compose-context': { hidden: true },
      '#compose-context-title': { textContent: '' },
      '#compose-context-source': { textContent: '' },
      '#compose-target': { hidden: false },
      '#compose-session-pair': { hidden: false },
      '#compose-model': { value: 'provider/chosen' },
      '#compose-engine': { value: '' },
      '#compose-thinking': { value: 'xhigh' },
      '#compose-executor': { value: 'provider/executor' },
      '#compose-executor-effort': { value: 'max' },
      '#compose-hint': hint,
      '#compose-start': start,
      '#compose': dialog,
    },
    state = {
      cards: [card], counts: { finding: 1 }, drafts: new Map(), sheetCardId: null, sheetEngine: '',
      pending: null, remoteSwapUntil: 0, inflight: new Set(), actionRevision: 0,
      dirty: false, idx: 0, history: { loaded: true, error: null },
    },
    requests = [],
    context = vm.createContext({
      MODES: { goal: 'goal help', plan: 'plan help', 'plan-into': 'plan into help' },
      TASKS: {
        investigate: { help: 'Investigate help', doing: 'Investigating' },
        fix: { help: 'Fix help', doing: 'Fixing' },
        act: { help: 'Act help', doing: 'Acting' },
        verify: { help: 'Verify help', doing: 'Verifying' },
        simplify: { help: 'Simplify help', doing: 'Simplifying' },
      },
      S: state,
      PICKERS: {
        'compose-engine': { fallback: 'omp' },
        'compose-model': { options: [{ value: 'provider/chosen' }] },
      },
      Date: { now: () => 1000 },
      window: { matchMedia: () => ({ matches: true }) },
      document: { activeElement: null },
      $: (selector) => elements[selector] || null,
      draft: (current) => {
        if (!state.drafts.has(current.id)) state.drafts.set(current.id, {
          note: 'Do the safe thing', dispatchMode: 'goal', dispatchTask: 'investigate', dispatchOption: null, dispatchAnswers: null,
        });
        return state.drafts.get(current.id);
      },
      answerable: () => true,
      showDialog: (current) => { current.open = true; },
      loadModels() {},
      loadComposeSessions() {},
      titleOf: (current) => current.title,
      sourceName: () => 'unknown',
      dshOf: () => null,
      setPickerValue() {},
      clearModels() {},
      noteValue: (current) => state.drafts.get(current.id).note.trim() || null,
      snapshot: () => ({}),
      render() {},
      enc: encodeURIComponent,
      sheetImageBody: async () => null,
      resultMessage: () => 'Launched Do as recommended · Launch work',
      toast() {},
      refresh() {},
      act: async (current, verb, body) => {
        requests.push({ url: `/v1/cards/${current.id}/${verb}`, body });
        return true;
      },
    });
  vm.runInContext(
    `${functionSource('optionTask')}\n${functionSource('selectSegment')}\n${functionSource('sheetCard')}\n${functionSource('sheetTasks')}\n${functionSource('setSheetMode')}\n${functionSource('setSheetTask')}\n${functionSource('openSheet')}\n${functionSource('pick')}\n${functionSource('dispatchCard', true)}\n${functionSource('submitSheet', true)}\nglobalThis.pick = pick; globalThis.submitSheet = submitSheet;`,
    context,
  );
  context.pick(card, 0);
  assert.equal(dialog.open, true);
  assert.equal(checked(modeButtons), 'goal');
  assert.equal(checked(taskButtons), 'act');
  assert.equal(elements['#compose-prompt'].value, 'Do the safe thing');
  await context.submitSheet({ preventDefault() {} });
  assert.equal(requests[0].url, '/v1/cards/finding-card/answer');
  assert.deepEqual(JSON.parse(JSON.stringify(requests[0].body)), {
    option: 0, text: null, note: 'Do the safe thing', mode: 'goal', task: 'act',
    engine: null, model: 'provider/chosen', thinking: 'xhigh',
  });

  requests.length = 0;
  state.cards = [card];
  state.counts.finding = 1;
  context.pick(card, 1);
  assert.equal(checked(taskButtons), 'investigate');
  // Picking another Mode in the sheet changes what Start sends.
  context.setSheetMode('plan');
  await context.submitSheet({ preventDefault() {} });
  assert.equal(requests[0].url, '/v1/cards/finding-card/answer');
  assert.equal(requests[0].body.mode, 'plan');
  assert.equal('executor' in requests[0].body, false);
});

test('a plain session posts its mode and the execution model only for Plan into', async () => {
  const requests = [],
    prompt = { value: 'Start a session' },
    start = { disabled: false },
    elements = {
      '#compose-prompt': prompt,
      '#compose-cwd': { value: '' },
      '#compose-start': start,
      '#compose-hint': { textContent: '' },
      '#compose-session': { value: '' },
      '#compose-engine': { value: 'dsh' },
      '#compose-model': { value: '' },
      '#compose-thinking': { value: '' },
      '#compose-executor': { value: 'provider/executor' },
      '#compose-executor-effort': { value: '' },
      '#compose': { close() {} },
    },
    context = vm.createContext({
      S: { composeMode: 'goal', composeTask: 'fix', sheetCardId: null, cards: [] },
      $: (selector) => elements[selector] || null,
      fetch: async (url, init) => {
        requests.push({ url, init });
        return { ok: true, json: async () => ({ pane: '%51' }) };
      },
      announce() {},
      toast() {},
      sheetImageBody: async () => null,
    });
  vm.runInContext(
    `${functionSource('api', true)}\n${functionSource('sheetCard')}\n${functionSource('startSession', true)}\n${functionSource('submitSheet', true)}\nglobalThis.submitSheet = submitSheet;`,
    context,
  );

  await context.submitSheet({ preventDefault() {} });
  context.S.composeMode = 'plan';
  prompt.value = 'Start in plan mode';
  await context.submitSheet({ preventDefault() {} });
  context.S.composeMode = 'plan-into';
  prompt.value = 'Start in plan into mode';
  elements['#compose-executor-effort'].value = 'max';
  await context.submitSheet({ preventDefault() {} });

  const bodies = requests.map(({ init }) => JSON.parse(init.body));
  assert.equal(bodies.length, 3);
  assert.ok(bodies.every((body) => body.engine === 'dsh'));
  assert.equal(bodies[0].prompt, 'Start a session');
  assert.equal(bodies[1].prompt, 'Start in plan mode');
  assert.equal(bodies[2].prompt, 'Start in plan into mode');
  assert.equal(bodies[0].mode, 'goal');
  assert.equal(bodies[1].mode, 'plan');
  assert.equal(bodies[2].mode, 'plan-into');
  assert.ok(bodies.every((body) => body.task === 'fix'));
  assert.equal(bodies[2].executor, 'provider/executor');
  assert.equal(bodies[2].executor_thinking, 'max');
  assert.ok(!('executor' in bodies[0]) && !('executor' in bodies[1]));
  assert.ok(!('executor_thinking' in bodies[0]) && !('executor_thinking' in bodies[1]));
});

test('plan picker loads cycle roles without replacing the general model catalog', async () => {
  const pickers = {
      'compose-engine': { options: [], fallback: 'Daemon default' },
      'compose-model': { options: [], fallback: 'Daemon default' },
      'compose-thinking': { options: [], fallback: 'Daemon default' },
      'compose-executor': { options: [], fallback: 'omp smol role' },
      'compose-executor-effort': { options: [], fallback: 'Planner effort' },
      'plan-executor': { options: [], fallback: 'current Plan Review role' },
      'plan-executor-effort': { options: [], fallback: 'Planner effort' },
    },
    data = {
      engine: 'omp',
      models: [{ selector: 'provider/model', name: 'Concrete model' }, { selector: '@smol', name: 'smol role' }],
      execution_models: [{ selector: '@default', name: 'default role' }, { selector: '@smol', name: 'smol role' }],
      default: '@kimi',
      thinking: ['low', 'high'],
      default_thinking: 'high',
    },
    context = vm.createContext({
      PICKERS: pickers,
      modelLoad: 0,
      $: () => ({ value: '' }),
      pickerOption: (value, label, hint) => ({ value, label, hint }),
      api: async () => data,
      refreshPicker() {},
      setPickerValue() {},
      current: () => null,
    });
  vm.runInContext(
    `${functionSource('planExecutorLabels')}\n${functionSource('loadModels', true)}\n${functionSource('refreshEfforts')}\n${functionSource('effortLevels')}\nglobalThis.loadModels = loadModels;`,
    context,
  );
  await context.loadModels();
  assert.deepEqual(pickers['plan-executor'].options.map((option) => option.value), ['@default', '@smol']);
  assert.deepEqual(pickers['compose-executor'].options.map((option) => option.value), ['provider/model', '@smol']);
  assert.deepEqual(pickers['compose-executor-effort'].options.map((option) => option.value), ['low', 'high']);
  assert.deepEqual(pickers['plan-executor-effort'].options.map((option) => option.value), ['low', 'high']);
});

test('the session sheet lists the models of the engine its Engine field picks', async () => {
  const pickers = Object.fromEntries(
      ['compose'].flatMap((sheet) =>
        ['engine', 'model', 'thinking', 'executor', 'executor-effort'].map((id) => [
          `${sheet}-${id}`,
          { options: [], fallback: '' },
        ]),
      ).concat([
        ['plan-executor', { options: [], fallback: '' }],
        ['plan-executor-effort', { options: [], fallback: '' }],
      ]),
    ),
    catalogs = {
      omp: { engine: 'omp', models: [{ selector: '@kimi', name: '@kimi' }], execution_models: [], default: '@kimi', thinking: ['high'] },
      dsh: { engine: 'dsh', models: [{ selector: 'p/m', name: 'p/m' }], execution_models: [], default: null, thinking: ['high'] },
    },
    requested = [],
    context = vm.createContext({
      PICKERS: pickers,
      modelLoad: 0,
      $: (selector) => ({ value: selector === '#compose-engine' ? 'dsh' : '' }),
      pickerOption: (value, label, hint) => ({ value, label, hint }),
      api: async (path) => {
        requested.push(path);
        return catalogs[path.endsWith('dsh') ? 'dsh' : 'omp'];
      },
      refreshPicker() {},
      setPickerValue() {},
      current: () => null,
    });
  vm.runInContext(
    `${functionSource('planExecutorLabels')}\n${functionSource('loadModels', true)}\n${functionSource('refreshEfforts')}\n${functionSource('effortLevels')}\nglobalThis.loadModels = loadModels;`,
    context,
  );
  await context.loadModels();
  assert.deepEqual(requested, ['/v1/models', '/v1/models?engine=dsh']);
  assert.deepEqual(pickers['compose-model'].options.map((option) => option.value), ['p/m']);
  assert.equal(pickers['compose-model'].fallback, 'dsh default');
  assert.equal(pickers['compose-executor'].fallback, 'planner model');
  assert.equal(pickers['compose-engine'].fallback, 'omp');
  // The dsh sheet offers no execution effort until it names an execution model.
  const values = (id) => Array.from(pickers[id].options, (option) => option.value);
  assert.deepEqual(values('compose-executor-effort'), []);
  assert.equal(pickers['compose-executor-effort'].fallback, 'needs an execution model');
  // The plan card lists the levels of the model it runs on, and names the leftover omp level.
  assert.deepEqual(values('plan-executor-effort'), ['high']);
  assert.equal(pickers['plan-executor-effort'].fallback, 'Role level');
});

test('a dsh execution effort waits for its execution model', async () => {
  const fields = {},
    picked = [],
    pickers = {
      'compose-engine': { options: [], fallback: 'Daemon default' },
      'compose-model': { options: [], fallback: 'Daemon default' },
      'compose-thinking': { options: [], fallback: 'Daemon default' },
      'compose-executor': { options: [], fallback: 'omp smol role' },
      'compose-executor-effort': { options: [], fallback: 'Planner effort' },
      'plan-executor': { options: [], fallback: 'current Plan Review role' },
      'plan-executor-effort': { options: [], fallback: 'Planner effort' },
    },
    dsh = {
      engine: 'dsh',
      models: [
        { selector: 'p/fast', name: 'Fast', efforts: ['low', 'high'] },
        { selector: 'p/plain', name: 'Plain', efforts: [] },
      ],
      execution_models: [
        { selector: 'p/fast', name: 'Fast', efforts: ['low', 'high'] },
        { selector: 'p/plain', name: 'Plain', efforts: [] },
      ],
      default: 'p/fast',
      thinking: ['low', 'high'],
    },
    omp = {
      engine: 'omp',
      models: [{ selector: '@kimi', name: '@kimi' }],
      execution_models: [],
      default: '@kimi',
      thinking: ['low', 'high', 'max'],
    },
    options = (id) => Array.from(pickers[id].options, (option) => option.value),
    context = vm.createContext({
      PICKERS: pickers,
      modelLoad: 0,
      $: (selector) => (fields[selector] ??= { value: '' }),
      pickerOption: (value, label, hint) => ({ value, label, hint }),
      api: async (path) => (path.endsWith('dsh') ? dsh : omp),
      refreshPicker() {},
      setPickerValue: (id, value) => {
        picked.push([id, value]);
        fields[`#${id}`].value = value;
      },
      current: () => null,
    });
  vm.runInContext(
    `${functionSource('planExecutorLabels')}\n${functionSource('loadModels', true)}\n${functionSource('refreshEfforts')}\n${functionSource('effortLevels')}\nglobalThis.loadModels = loadModels;\nglobalThis.refreshEfforts = refreshEfforts;`,
    context,
  );
  fields['#compose-engine'] = { value: 'dsh' };
  await context.loadModels();
  // dsh waits for an execution model and says so.
  assert.deepEqual(options('compose-executor-effort'), []);
  assert.equal(pickers['compose-executor-effort'].fallback, 'needs an execution model');

  // A named model brings its own levels, and Default in the effort names the planner's level.
  fields['#compose-executor'].value = 'p/fast';
  context.refreshEfforts('compose');
  assert.deepEqual(options('compose-executor-effort'), ['low', 'high']);
  assert.equal(pickers['compose-executor-effort'].fallback, 'Planner effort');

  // Back to Default drops the level that belonged to the model.
  fields['#compose-executor-effort'].value = 'high';
  fields['#compose-executor'].value = '';
  context.refreshEfforts('compose');
  assert.deepEqual(options('compose-executor-effort'), []);
  assert.deepEqual(picked.at(-1), ['compose-executor-effort', '']);

  // A dsh plan card follows the same rule.
  pickers['plan-executor'].data = dsh;
  fields['#plan-executor'] = { value: '' };
  context.refreshEfforts('plan');
  assert.deepEqual(options('plan-executor-effort'), []);
  assert.equal(pickers['plan-executor-effort'].fallback, 'needs an execution model');
  fields['#plan-executor'].value = 'p/fast';
  context.refreshEfforts('plan');
  assert.deepEqual(options('plan-executor-effort'), ['low', 'high']);
  assert.equal(pickers['plan-executor-effort'].fallback, 'Planner effort');
});

// The last catalog read can belong to another card. The field must describe the card it sits on.
test('the effort field of a dsh plan card describes that card, whatever catalog was read last', () => {
  const pickers = {
      'plan-executor': { options: [], fallback: 'current Plan Review role' },
      'plan-executor-effort': { options: [{ value: 'max' }], fallback: 'Role level' },
    },
    dsh = { engine: 'dsh', models: [{ selector: 'p/fast', efforts: ['low', 'high'] }], default: 'p/fast', thinking: ['low', 'high'] },
    drafts = { waiting: { executor: '', executorThinking: '' }, chosen: { executor: 'p/fast', executorThinking: '' } },
    shown = {},
    context = vm.createContext({
      PICKERS: pickers,
      draft: (c) => drafts[c.id],
      pickerOption: (value) => ({ value }),
      ident: (name) => name,
      h: () => ({}),
      el: () => ({}),
      bindPicker() {},
      // An unset field shows its picker's Default row.
      setPickerValue: (id) => {
        shown[id] = pickers[id].fallback;
      },
    }),
    effort = (id) => {
      context.planExecutorView({ id, kind: 'plan', engine: 'dsh' });
      return [shown['plan-executor-effort'], Array.from(pickers['plan-executor-effort'].options, (option) => option.value)];
    };
  vm.runInContext(
    `${functionSource('planExecutorView')}\n${functionSource('planExecutorLabels')}\n${functionSource('effortLevels')}\nglobalThis.planExecutorView = planExecutorView;`,
    context,
  );

  // The catalog of an omp plan card was read last and left the omp label and levels behind.
  pickers['plan-executor'].data = { engine: 'omp', models: [], thinking: ['max'] };
  assert.deepEqual(effort('waiting'), ['needs an execution model', []]);
  // A card that names a model lists that model's levels once the dsh catalog is read.
  assert.deepEqual(effort('chosen'), ['Planner effort', []]);
  pickers['plan-executor'].data = dsh;
  assert.deepEqual(effort('chosen'), ['Planner effort', ['low', 'high']]);
  // Back on the card without a model, the levels of the other card are gone.
  assert.deepEqual(effort('waiting'), ['needs an execution model', []]);
});

test('a catalog that answers late never fills the model field of another engine', async () => {
  const pickers = Object.fromEntries(
      ['compose'].flatMap((sheet) =>
        ['engine', 'model', 'thinking', 'executor', 'executor-effort'].map((id) => [
          `${sheet}-${id}`,
          { options: [], fallback: '' },
        ]),
      ).concat([
        ['plan-executor', { options: [], fallback: '' }],
        ['plan-executor-effort', { options: [], fallback: '' }],
      ]),
    ),
    fields = { '#compose-engine': { value: '' } },
    omp = {
      engine: 'omp',
      models: [{ selector: '@kimi', name: '@kimi' }],
      execution_models: [],
      default: '@kimi',
      thinking: ['high'],
    },
    dsh = {
      engine: 'dsh',
      models: [{ selector: 'p/slow', name: 'Slow' }],
      execution_models: [],
      default: 'p/slow',
      thinking: ['low'],
    },
    held = {},
    context = vm.createContext({
      PICKERS: pickers,
      modelLoad: 0,
      $: (selector) => (fields[selector] ??= { value: '' }),
      pickerOption: (value, label, hint) => ({ value, label, hint }),
      // The dsh catalog answers only when the test says so, the way a timing-out bridge holds
      // one while the sheet moves on.
      api: async (path) => {
        if (path.endsWith('engine=dsh'))
          return new Promise((resolve) => {
            held.answer = () => resolve(dsh);
          });
        return omp;
      },
      refreshPicker() {},
      setPickerValue() {},
      current: () => null,
    }),
    values = (id) => Array.from(pickers[id].options, (option) => option.value);
  vm.runInContext(
    `${functionSource('planExecutorLabels')}\n${functionSource('loadModels', true)}\n${functionSource('refreshEfforts')}\n${functionSource('effortLevels')}\nglobalThis.loadModels = loadModels;`,
    context,
  );

  // The sheet opens on dsh and its catalog read hangs.
  fields['#compose-engine'].value = 'dsh';
  const slow = context.loadModels();
  for (let i = 0; i < 100 && !held.answer; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(typeof held.answer, 'function', 'the dsh catalog read starts');

  // The engine goes back to omp, whose catalog is already in hand.
  fields['#compose-engine'].value = 'omp';
  await context.loadModels();
  assert.deepEqual(values('compose-model'), ['@kimi'], 'the omp field lists the omp catalog');

  // The dsh read answers now, long after the field moved to omp.
  held.answer();
  await slow;
  assert.deepEqual(values('compose-model'), ['@kimi'], 'a late dsh catalog leaves the omp field alone');
  assert.equal(pickers['compose-model'].fallback, '@kimi');

  // A read made for dsh itself still fills the field.
  held.answer = null;
  fields['#compose-engine'].value = 'dsh';
  const fresh = context.loadModels();
  for (let i = 0; i < 100 && !held.answer; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
  held.answer();
  await fresh;
  assert.deepEqual(values('compose-model'), ['p/slow'], 'the dsh field lists the dsh catalog');
  assert.equal(pickers['compose-model'].fallback, 'Slow');
});

test('plan approval sends chosen role while Default and Refine keep their request contracts', () => {
  const approve = { id: 'plan-picked', kind: 'plan', options: [{ label: 'Approve' }, { label: 'Refine' }] },
    defaultApprove = { id: 'plan-default', kind: 'plan', options: [{ label: 'Approve' }] },
    refine = { id: 'plan-refine', kind: 'plan', options: [{ label: 'Approve' }, { label: 'Refine' }] },
    ask = { id: 'ask', kind: 'ask', options: [{ label: 'Yes' }] },
    drafts = new Map([
      [approve.id, { executor: '@slow', executorThinking: 'max', other: '' }],
      [defaultApprove.id, { executor: '', other: '' }],
      [refine.id, { executor: '@smol', other: 'Add a rollback step' }],
      [ask.id, { executor: '@smol', other: '' }],
    ]),
    actions = [],
    context = vm.createContext({
      S: { remoteSwapUntil: 0 },
      Date: { now: () => 1000 },
      answerable: () => true,
      draft: (card) => drafts.get(card.id),
      noteValue: () => null,
      act: (...args) => actions.push(args),
      toast() {},
      announce() {},
      toggleOther() {},
      $: () => null,
    });
  vm.runInContext(`${functionSource('pick')}\nglobalThis.pick = pick;`, context);
  context.pick(approve, 0);
  context.pick(defaultApprove, 0);
  context.pick(refine, 1);
  context.pick(ask, 0);
  assert.deepEqual(JSON.parse(JSON.stringify(actions.map(([, , body]) => body))), [
    { option: 0, text: null, note: null, executor: '@slow', executor_thinking: 'max' },
    { option: 0, text: null, note: null, executor: null, executor_thinking: null },
    { option: 1, text: 'Add a rollback step', note: null },
    { option: 0, text: null, note: null },
  ]);
});

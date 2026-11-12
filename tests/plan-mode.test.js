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

test('i, A, p and P open dispatch with matching modes while v keeps Peek', () => {
  const card = { id: 'card-1', kind: 'plan', title: 'Plan ready: Update tests' },
    buttons = ['investigate', 'act', 'plan', 'plan-into'].map((mode) => ({
      dataset: { mode },
      attrs: { 'aria-checked': String(mode === 'investigate') },
      tabIndex: mode === 'investigate' ? 0 : -1,
      setAttribute(name, value) {
        this.attrs[name] = value;
      },
      getAttribute(name) {
        return this.attrs[name];
      },
    })),
    dialog = { open: false },
    elements = {
      '#peek': { open: false },
      '#dispatch-mode': { children: buttons },
      '#dispatch-mode-help': { textContent: '' },
      '#dispatch-hint': { textContent: '' },
      '#dispatch-mode-investigate': { hidden: false },
      '#dispatch-models': { hidden: true },
      '#dispatch-note': { value: '', focus() {} },
      '#dispatch-executor-effort': { value: 'max' },
      '#dispatch-start': { disabled: false },
      '#dispatch': dialog,
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
      dispatchId: card.id,
      composeMode: 'goal',
      drafts: new Map([[card.id, { dispatchMode: 'investigate', dispatch: 'Review the plan', dispatchAnswers: null }]]),
    },
    context = vm.createContext({
      S: state,
      PICKERS: { 'dispatch-model': { options: [] } },
      loadModels() {},
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
      dshOf: () => null,
      draft: (current) => state.drafts.get(current.id),
      showDialog: (current) => {
        current.open = true;
      },
      openPeek: (current) => actions.push(['peek', current.id]),
      pick: (current, index) => actions.push(['pick', current.id, index]),
      primary: (current) => actions.push(['primary', current.id]),
    });
  vm.runInContext(
    `${functionSource('selectSegment')}\n${functionSource('setDispatchMode')}\n${functionSource('openDispatch')}\n${keydownSource}`,
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
  const selected = () => buttons.find((button) => button.getAttribute('aria-checked') === 'true').dataset.mode;

  press('i');
  assert.equal(dialog.open, true);
  assert.equal(selected(), 'investigate');
  dialog.open = false;
  press('A');
  assert.equal(selected(), 'act');
  assert.equal(elements['#dispatch-models'].hidden, true);
  dialog.open = false;
  press('p');
  assert.equal(selected(), 'plan');
  assert.equal(state.drafts.get(card.id).dispatchMode, 'plan');
  assert.match(elements['#dispatch-mode-help'].textContent, /plan/i);
  dialog.open = false;
  press('P');
  assert.equal(selected(), 'plan-into');
  assert.equal(state.drafts.get(card.id).dispatchMode, 'plan-into');
  assert.equal(elements['#dispatch-models'].hidden, false);
  assert.match(elements['#dispatch-mode-help'].textContent, /without review/);
  dialog.open = false;
  press('A');
  assert.equal(selected(), 'act');
  assert.equal(elements['#dispatch-models'].hidden, true);
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

test('a confirms the primary action even when a button holds focus', () => {
  const card = { id: 'card-1', kind: 'info', title: 'Report' },
    actions = [],
    listeners = {},
    state = {
      cards: [card],
      idx: 0,
      pending: null,
      remoteSwapUntil: 0,
      laterOpen: false,
      countsExpanded: false,
      dispatchId: null,
      composeMode: 'goal',
      drafts: new Map(),
    },
    context = vm.createContext({
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
    });
  vm.runInContext(keydownSource, context);

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
  assert.deepEqual(actions, [['primary', 'card-1']]);
  assert.equal(focusedA.defaultPrevented, true);
  const focusedEnter = press('Enter', true);
  assert.deepEqual(actions, [['primary', 'card-1']]);
  assert.equal(focusedEnter.defaultPrevented, false);
  press('Enter', false);
  press('a', false);
  assert.deepEqual(actions, [
    ['primary', 'card-1'],
    ['primary', 'card-1'],
    ['primary', 'card-1'],
  ]);
});

test('dispatch sheet posts model and thinking for every mode', async () => {
  const card = { id: 'launch-card', kind: 'finding', title: 'Launch work' },
    state = {
      cards: [card],
      counts: { finding: 1 },
      drafts: new Map([
        [card.id, { dispatchMode: 'plan', dispatch: 'Make a plan' }],
        ['plan-into-card', { dispatchMode: 'plan-into', dispatch: 'Make a plan' }],
      ]),
      dispatchId: card.id,
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
      '#dispatch-start': start,
      '#dispatch-hint': hint,
      '#dispatch-note': { value: 'Make a plan' },
      '#dispatch-model': { value: 'provider/planner' },
      '#dispatch-engine': { value: 'dsh' },
      '#dispatch-thinking': { value: 'high' },
      '#dispatch-executor': { value: '' },
      '#dispatch-executor-effort': { value: '' },
      '#dispatch': dialog,
    },
    context = vm.createContext({
      S: state,
      Date: { now: () => 1000 },
      window: { matchMedia: () => ({ matches: true }) },
      $: (selector) => elements[selector] || null,
      draft: (current) => state.drafts.get(current.id),
      snapshot: () => ({}),
      render() {},
      enc: encodeURIComponent,
      fetch: async (url, init) => {
        requests.push({ url, init });
        return { ok: true, json: async () => ({ pane: '%42' }) };
      },
      sheetImageBody: async () => null,
      resultMessage: () => 'Planning · Launch work',
      toast() {},
      refresh() {},
    });
  vm.runInContext(
    `${functionSource('api', true)}\n${functionSource('act', true)}\n${functionSource('settle', true)}\n${functionSource('submitDispatch', true)}\nglobalThis.submitDispatch = submitDispatch;`,
    context,
  );

  await context.submitDispatch({ preventDefault() {} });
  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, '/v1/cards/launch-card/dispatch');
  assert.deepEqual(JSON.parse(requests[0].init.body), {
    mode: 'plan', note: 'Make a plan', engine: 'dsh', model: 'provider/planner', thinking: 'high',
  });

  requests.length = 0;
  card.id = 'plan-into-card';
  state.dispatchId = 'plan-into-card';
  context.S.cards = [card];
  elements['#dispatch-executor-effort'].value = 'max';
  await context.submitDispatch({ preventDefault() {} });
  assert.equal(requests.length, 1);
  assert.deepEqual(JSON.parse(requests[0].init.body), {
    mode: 'plan-into',
    engine: 'dsh',
    note: 'Make a plan',
    model: 'provider/planner',
    thinking: 'high',
    executor: null,
    executor_thinking: 'max',
  });
});

test('finding options open the launch sheet and answer with its choices', async () => {
  const card = {
      id: 'finding-card', kind: 'finding', options: [
        { label: 'Do as recommended', recommended: true },
        { label: 'Investigate first' },
      ],
    },
    buttons = ['investigate', 'act', 'plan', 'plan-into'].map((mode) => ({
      dataset: { mode },
      attrs: { 'aria-checked': String(mode === 'investigate') },
      tabIndex: mode === 'investigate' ? 0 : -1,
      setAttribute(name, value) { this.attrs[name] = value; },
      getAttribute(name) { return this.attrs[name]; },
    })),
    dialog = { open: false, close() { this.open = false; } },
    start = { disabled: false },
    hint = { textContent: '' },
    elements = {
      '#dispatch-mode': { children: buttons },
      '#dispatch-mode-investigate': { hidden: false },
      '#dispatch-mode-help': { textContent: '' },
      '#dispatch-models': { hidden: true },
      '#dispatch-note': { value: '', focus() {} },
      '#dispatch-model': { value: 'provider/chosen' },
      '#dispatch-engine': { value: '' },
      '#dispatch-thinking': { value: 'xhigh' },
      '#dispatch-executor': { value: 'provider/executor' },
      '#dispatch-executor-effort': { value: 'max' },
      '#dispatch-hint': hint,
      '#dispatch-start': start,
      '#dispatch': dialog,
    },
    state = {
      cards: [card], counts: { finding: 1 }, drafts: new Map(), dispatchId: null,
      pending: null, remoteSwapUntil: 0, inflight: new Set(), actionRevision: 0,
      dirty: false, idx: 0, history: { loaded: true, error: null },
    },
    requests = [],
    context = vm.createContext({
      S: state,
      PICKERS: { 'dispatch-model': { options: [{ value: 'provider/chosen' }] } },
      Date: { now: () => 1000 },
      window: { matchMedia: () => ({ matches: true }) },
      document: { activeElement: null },
      $: (selector) => elements[selector] || null,
      draft: (current) => {
        if (!state.drafts.has(current.id)) state.drafts.set(current.id, {
          note: 'Do the safe thing', dispatch: '', dispatchMode: 'investigate', dispatchOption: null, dispatchAnswers: null,
        });
        return state.drafts.get(current.id);
      },
      answerable: () => true,
      showDialog: (current) => { current.open = true; },
      loadModels() {},
      dshOf: () => null,
      snapshot: () => ({}),
      render() {},
      enc: encodeURIComponent,
      fetch: async (url, init) => {
        requests.push({ url, init });
        return { ok: true, json: async () => ({ ok: true }) };
      },
      sheetImageBody: async () => null,
      resultMessage: () => 'Launched Do as recommended · Launch work',
      toast() {},
      refresh() {},
    });
  vm.runInContext(
    `${functionSource('optionMode')}\n${functionSource('selectSegment')}\n${functionSource('setDispatchMode')}\n${functionSource('openDispatch')}\n${functionSource('pick')}\n${functionSource('api', true)}\n${functionSource('act', true)}\n${functionSource('settle', true)}\n${functionSource('submitDispatch', true)}\nglobalThis.pick = pick; globalThis.submitDispatch = submitDispatch;`,
    context,
  );
  context.setDispatchMode = (mode) => {
    const selected = buttons.find((button) => button.dataset.mode === mode);
    buttons.forEach((button) => button.setAttribute('aria-checked', String(button === selected)));
    state.drafts.get(card.id).dispatchMode = mode;
  };

  context.pick(card, 0);
  assert.equal(dialog.open, true);
  assert.equal(buttons.find((button) => button.getAttribute('aria-checked') === 'true').dataset.mode, 'act');
  assert.equal(elements['#dispatch-note'].value, 'Do the safe thing');
  await context.submitDispatch({ preventDefault() {} });
  assert.equal(requests[0].url, '/v1/cards/finding-card/answer');
  assert.deepEqual(JSON.parse(requests[0].init.body), {
    option: 0, text: null, note: 'Do the safe thing', mode: 'act',
    engine: null, model: 'provider/chosen', thinking: 'xhigh',
  });

  requests.length = 0;
  state.cards = [card];
  state.counts.finding = 1;
  context.pick(card, 1);
  assert.equal(buttons.find((button) => button.getAttribute('aria-checked') === 'true').dataset.mode, 'investigate');
  context.setDispatchMode('plan');
  await context.submitDispatch({ preventDefault() {} });
  assert.equal(requests[0].url, '/v1/cards/finding-card/answer');
  assert.equal(JSON.parse(requests[0].init.body).mode, 'plan');
  assert.equal('executor' in JSON.parse(requests[0].init.body), false);
});

test('compose posts its mode and the execution model only for Plan into', async () => {
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
      S: { composeMode: 'goal' },
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
    `${functionSource('api', true)}\n${functionSource('submitCompose', true)}\nglobalThis.submitCompose = submitCompose;`,
    context,
  );

  await context.submitCompose({ preventDefault() {} });
  context.S.composeMode = 'plan';
  prompt.value = 'Start in plan mode';
  await context.submitCompose({ preventDefault() {} });
  context.S.composeMode = 'plan-into';
  prompt.value = 'Start in plan into mode';
  elements['#compose-executor-effort'].value = 'max';
  await context.submitCompose({ preventDefault() {} });

  const bodies = requests.map(({ init }) => JSON.parse(init.body));
  assert.equal(bodies.length, 3);
  assert.ok(bodies.every((body) => body.engine === 'dsh'));
  assert.equal(bodies[0].prompt, 'Start a session');
  assert.equal(bodies[1].prompt, 'Start in plan mode');
  assert.equal(bodies[2].prompt, 'Start in plan into mode');
  assert.equal(bodies[0].mode, 'goal');
  assert.equal(bodies[1].mode, 'plan');
  assert.equal(bodies[2].mode, 'plan-into');
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
      'dispatch-engine': { options: [], fallback: 'Daemon default' },
      'dispatch-model': { options: [], fallback: 'Daemon default' },
      'dispatch-thinking': { options: [], fallback: 'Daemon default' },
      'dispatch-executor': { options: [], fallback: 'omp smol role' },
      'dispatch-executor-effort': { options: [], fallback: 'Planner effort' },
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

test('each sheet lists the models of the engine its Engine field picks', async () => {
  const pickers = Object.fromEntries(
      ['compose', 'dispatch'].flatMap((sheet) =>
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
      $: (selector) => ({ value: selector === '#dispatch-engine' ? 'dsh' : '' }),
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
  assert.deepEqual(pickers['compose-model'].options.map((option) => option.value), ['@kimi']);
  assert.equal(pickers['compose-model'].fallback, '@kimi');
  assert.deepEqual(pickers['dispatch-model'].options.map((option) => option.value), ['p/m']);
  assert.equal(pickers['dispatch-model'].fallback, 'dsh default');
  assert.equal(pickers['dispatch-executor'].fallback, 'planner model');
  assert.equal(pickers['dispatch-engine'].fallback, 'omp');
  // The dsh dispatch sheet offers no execution effort until it names an execution model.
  const values = (id) => Array.from(pickers[id].options, (option) => option.value);
  assert.deepEqual(values('dispatch-executor-effort'), []);
  assert.equal(pickers['dispatch-executor-effort'].fallback, 'needs an execution model');
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
      'dispatch-engine': { options: [], fallback: 'Daemon default' },
      'dispatch-model': { options: [], fallback: 'Daemon default' },
      'dispatch-thinking': { options: [], fallback: 'Daemon default' },
      'dispatch-executor': { options: [], fallback: 'omp smol role' },
      'dispatch-executor-effort': { options: [], fallback: 'Planner effort' },
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
  fields['#dispatch-engine'] = { value: 'dsh' };
  await context.loadModels();
  // dsh waits for an execution model and says so; omp keeps the default model's list.
  assert.deepEqual(options('dispatch-executor-effort'), []);
  assert.equal(pickers['dispatch-executor-effort'].fallback, 'needs an execution model');
  assert.deepEqual(options('compose-executor-effort'), ['low', 'high', 'max']);
  assert.equal(pickers['compose-executor-effort'].fallback, 'Selector level');

  // A named model brings its own levels, and Default in the effort names the planner's level.
  fields['#dispatch-executor'].value = 'p/fast';
  context.refreshEfforts('dispatch');
  assert.deepEqual(options('dispatch-executor-effort'), ['low', 'high']);
  assert.equal(pickers['dispatch-executor-effort'].fallback, 'Planner effort');

  // Back to Default drops the level that belonged to the model.
  fields['#dispatch-executor-effort'].value = 'high';
  fields['#dispatch-executor'].value = '';
  context.refreshEfforts('dispatch');
  assert.deepEqual(options('dispatch-executor-effort'), []);
  assert.deepEqual(picked.at(-1), ['dispatch-executor-effort', '']);

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

test('a catalog that answers late never fills the model field of another engine', async () => {
  const pickers = Object.fromEntries(
      ['compose', 'dispatch'].flatMap((sheet) =>
        ['engine', 'model', 'thinking', 'executor', 'executor-effort'].map((id) => [
          `${sheet}-${id}`,
          { options: [], fallback: '' },
        ]),
      ).concat([
        ['plan-executor', { options: [], fallback: '' }],
        ['plan-executor-effort', { options: [], fallback: '' }],
      ]),
    ),
    fields = { '#compose-engine': { value: '' }, '#dispatch-engine': { value: '' } },
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
  assert.deepEqual(values('dispatch-model'), ['@kimi'], 'a late dsh catalog leaves the omp dispatch field alone');
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

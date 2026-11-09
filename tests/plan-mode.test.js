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
      '#dispatch-models': { hidden: true },
      '#dispatch-note': { value: '', focus() {} },
      '#dispatch-hint': { textContent: '' },
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
      dispatchId: null,
      composeMode: 'goal',
      drafts: new Map([[card.id, { dispatchMode: 'investigate', dispatch: 'Review the plan' }]]),
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
      '#dispatch-thinking': { value: 'high' },
      '#dispatch-executor': { value: '' },
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
    mode: 'plan', note: 'Make a plan', model: 'provider/planner', thinking: 'high',
  });

  requests.length = 0;
  card.id = 'plan-into-card';
  state.dispatchId = 'plan-into-card';
  context.S.cards = [card];
  await context.submitDispatch({ preventDefault() {} });
  assert.equal(requests.length, 1);
  assert.deepEqual(JSON.parse(requests[0].init.body), {
    mode: 'plan-into',
    note: 'Make a plan',
    model: 'provider/planner',
    thinking: 'high',
    executor: null,
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
      '#dispatch-mode-help': { textContent: '' },
      '#dispatch-models': { hidden: true },
      '#dispatch-note': { value: '', focus() {} },
      '#dispatch-model': { value: 'provider/chosen' },
      '#dispatch-thinking': { value: 'xhigh' },
      '#dispatch-executor': { value: 'provider/executor' },
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
          note: 'Do the safe thing', dispatch: '', dispatchMode: 'investigate', dispatchOption: null,
        });
        return state.drafts.get(current.id);
      },
      answerable: () => true,
      showDialog: (current) => { current.open = true; },
      loadModels() {},
      snapshot: () => ({}),
      render() {},
      enc: encodeURIComponent,
      fetch: async (url, init) => {
        requests.push({ url, init });
        return { ok: true, json: async () => ({ ok: true }) };
      },
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
    model: 'provider/chosen', thinking: 'xhigh',
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
      '#compose-model': { value: '' },
      '#compose-thinking': { value: '' },
      '#compose-executor': { value: 'provider/executor' },
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
  await context.submitCompose({ preventDefault() {} });

  const bodies = requests.map(({ init }) => JSON.parse(init.body));
  assert.equal(bodies.length, 3);
  assert.equal(bodies[0].prompt, 'Start a session');
  assert.equal(bodies[1].prompt, 'Start in plan mode');
  assert.equal(bodies[2].prompt, 'Start in plan into mode');
  assert.equal(bodies[0].mode, 'goal');
  assert.equal(bodies[1].mode, 'plan');
  assert.equal(bodies[2].mode, 'plan-into');
  assert.equal(bodies[2].executor, 'provider/executor');
  assert.ok(!('executor' in bodies[0]) && !('executor' in bodies[1]));
});

test('plan picker loads cycle roles without replacing the general model catalog', async () => {
  const pickers = {
      'compose-model': { options: [], fallback: 'Daemon default' },
      'compose-executor': { options: [], fallback: 'omp smol role' },
      'dispatch-model': { options: [], fallback: 'Daemon default' },
      'dispatch-executor': { options: [], fallback: 'omp smol role' },
      'compose-thinking': { options: [], fallback: 'Daemon default' },
      'plan-executor': { options: [], fallback: 'current Plan Review role' },
    },
    modelPickers = ['compose-model', 'compose-executor', 'dispatch-model', 'dispatch-executor', 'plan-executor'],
    data = {
      models: [{ selector: 'provider/model', name: 'Concrete model' }, { selector: '@smol', name: 'smol role' }],
      execution_models: [{ selector: '@default', name: 'default role' }, { selector: '@smol', name: 'smol role' }],
      default: '@kimi',
      thinking: ['low', 'high'],
      default_thinking: 'high',
    },
    context = vm.createContext({
      PICKERS: pickers,
      MODEL_PICKERS: modelPickers,
      pickerOption: (value, label, hint) => ({ value, label, hint }),
      api: async () => data,
      refreshPicker() {},
      current: () => null,
    });
  vm.runInContext(`${functionSource('loadModels', true)}\nglobalThis.loadModels = loadModels;`, context);
  await context.loadModels();
  assert.deepEqual(pickers['plan-executor'].options.map((option) => option.value), ['@default', '@smol']);
  assert.deepEqual(pickers['compose-executor'].options.map((option) => option.value), ['provider/model', '@smol']);
});

test('plan approval sends chosen role while Default and Refine keep their request contracts', () => {
  const approve = { id: 'plan-picked', kind: 'plan', options: [{ label: 'Approve' }, { label: 'Refine' }] },
    defaultApprove = { id: 'plan-default', kind: 'plan', options: [{ label: 'Approve' }] },
    refine = { id: 'plan-refine', kind: 'plan', options: [{ label: 'Approve' }, { label: 'Refine' }] },
    ask = { id: 'ask', kind: 'ask', options: [{ label: 'Yes' }] },
    drafts = new Map([
      [approve.id, { executor: '@slow', other: '' }],
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
    { option: 0, text: null, note: null, executor: '@slow' },
    { option: 0, text: null, note: null, executor: null },
    { option: 1, text: 'Add a rollback step', note: null },
    { option: 0, text: null, note: null },
  ]);
});

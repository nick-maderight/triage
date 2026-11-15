// Every card action lives in one table in ui/app.js. These tests hold the dock, the keys and
// Enter to that table on every card shape. An action then looks and works the same on every
// card that offers it.
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
const slice = (from, to) => {
  const start = app.indexOf(from),
    end = app.indexOf(to, start);
  assert.ok(start >= 0 && end > start, `app.js holds ${from.trim()}`);
  return app.slice(start, end);
};

function node(tag, props = {}, ...children) {
  return {
    tag,
    props: typeof props === 'string' ? { class: props } : props,
    children: children.flat(Infinity).filter((child) => child != null && child !== false && child !== ''),
  };
}
function findNodes(value, predicate, found = []) {
  if (Array.isArray(value)) {
    for (const child of value) findNodes(child, predicate, found);
  } else if (value && typeof value === 'object') {
    if (predicate(value)) found.push(value);
    for (const child of value.children || []) findNodes(child, predicate, found);
  }
  return found;
}

// Loads the real action table with its helpers, the dock and Enter. Whatever an action does
// lands in `calls`.
const calls = [],
  context = vm.createContext({
    S: { pending: null, laterOpen: false, remoteSwapUntil: 0 },
    Date: { now: () => 1000 },
    enc: encodeURIComponent,
    h: node,
    el: (tag, props, ...children) => node(tag, props || {}, ...children),
    button: (_card, _control, className, props, onClick, ...children) =>
      node('button', { ...props, class: className, onClick }, ...children),
    draft: () => ({ step: 0 }),
    noteValue: () => null,
    $: (selector) => (selector === '#note' ? { focus: () => calls.push(['focus note']) } : null),
    openDispatch: (card, mode, { newWindow = false } = {}) =>
      calls.push(['dispatch', card.id, mode, newWindow ? 'own session' : 'card session']),
    act: (card, verb) => calls.push([verb, card.id]),
    openContext: (id) => calls.push(['context', id]),
    toggleLater: () => calls.push(['later']),
    later() {},
    openPeek: (card) => calls.push(['peek', card.id]),
    jump: (card) => calls.push(['jump', card.id]),
    openDsh: () => calls.push(['open dsh']),
    toggleOther: (card) => calls.push(['other', card.id]),
    pick: (card, index) => calls.push(['pick', card.id, index]),
    primaryStep: (card) => calls.push(['step', card.id]),
    recIndex: () => 0,
  });
vm.runInContext(
  `${slice('  const answerable =', '  const current =')}\n` +
    `${['otherAllowed', 'dock', 'primary', 'resultMessage'].map(functionSource).join('\n')}\n` +
    'Object.assign(globalThis, { ACTIONS, actionOf, dock, primary, resultMessage });',
  context,
);
const { ACTIONS } = context;
const ran = (fn) => {
  calls.length = 0;
  fn();
  return calls.map((call) => call.join(' '));
};
const press = (card, key) =>
  ran(() => {
    const id = context.actionOf(card, key);
    if (id) ACTIONS[id].run(card);
  });
const buttonsOf = (card) =>
  findNodes(context.dock(card), (item) => item.tag === 'button').map((item) => ({
    label: item.children.filter((child) => typeof child === 'string').join(''),
    key: item.children.find((child) => child.tag === 'kbd').children.join(''),
    shortcuts: item.props['aria-keyshortcuts'],
    primary: item.props['data-primary'] === 'true',
    click: item.props.onClick,
  }));

const tmux = (pane) => ({ uri: `tmux://pane/${pane}`, pane }),
  dsh = (session) => ({ uri: `dsh://session/${session}` }),
  options = [{ label: 'Yes', recommended: true }, { label: 'No' }],
  questions = [
    { label: 'First', question: 'Which one?', options },
    { label: 'Second', question: 'When?', options },
  ],
  step = { action: 'Grant read access.' };

// One card per shape the daemon queues. The shapes vary kind, harness, session, answerable,
// questions, an open step, a missing final report and an ended session.
const SHAPES = {
  ask: { id: 'ask', kind: 'ask', source: tmux('%1'), allow_other: true, options },
  'ask with questions': { id: 'ask-questions', kind: 'ask', source: tmux('%2'), allow_other: true, options, questions },
  'read-only ask': { id: 'ask-read-only', kind: 'ask', source: tmux('%3'), allow_other: true, answerable: false, options },
  'dsh ask': { id: 'dsh-ask', kind: 'ask', engine: 'dsh', source: dsh('a'), allow_other: true, options },
  'read-only dsh ask': { id: 'dsh-ask-read-only', kind: 'ask', engine: 'dsh', source: dsh('b'), allow_other: true, answerable: false, options },
  plan: { id: 'plan', kind: 'plan', source: tmux('%4'), allow_other: true, options },
  'dsh plan': { id: 'dsh-plan', kind: 'plan', engine: 'dsh', source: dsh('c'), allow_other: true, options },
  finding: { id: 'finding', kind: 'finding', source: { uri: 'urn:rule' }, options },
  'finding with questions': { id: 'finding-questions', kind: 'finding', source: { uri: 'urn:rule' }, allow_other: true, options, questions },
  reply: { id: 'done:%5:entry', kind: 'info', source: tmux('%5') },
  'report with an open step': { id: 'done:%6:entry', kind: 'info', source: tmux('%6'), star: step },
  'reply without a final report': { id: 'done:%7:entry', kind: 'info', source: tmux('%7'), report_due: true },
  'ended session': { id: 'exited:%8:-', kind: 'info', source: tmux('%8') },
  'dsh reply': { id: 'done:dsh:d:1', kind: 'info', engine: 'dsh', source: dsh('d') },
  'dsh report with an open step': { id: 'done:dsh:e:1', kind: 'info', engine: 'dsh', source: dsh('e'), star: step },
  'dsh reply without a final report': { id: 'done:dsh:f:1', kind: 'info', engine: 'dsh', source: dsh('f'), report_due: true },
  notice: { id: 'notice', kind: 'info', source: { uri: 'urn:job' } },
  'notice with an open step': { id: 'notice-step', kind: 'info', source: { uri: 'urn:job' }, star: step },
};

// The buttons each card shows, in order, each with its key. `*` marks the primary, which Enter
// runs. A card without one answers Enter with its recommended option.
const REST = ['Skip s', 'Context t', 'Later l'],
  PROMPT = ['Dispatch i', 'Acknowledge & close x', ...REST],
  REPLY = ['Acknowledge a', ...REST],
  STEP = ['*Plan next step p', 'Dispatch i', 'Do next step d'];
const DOCKS = {
  ask: [...PROMPT, 'Peek v', 'Jump J'],
  'ask with questions': [...PROMPT, 'Peek v', 'Jump J'],
  'read-only ask': ['*Peek v', ...PROMPT, 'Jump J'],
  'dsh ask': [...PROMPT, 'Peek v', 'Open J'],
  'read-only dsh ask': ['*Peek v', ...PROMPT, 'Open J'],
  plan: [...PROMPT, 'Peek v', 'Jump J'],
  'dsh plan': [...PROMPT, 'Peek v', 'Open J'],
  finding: ['Dispatch i', ...REST],
  'finding with questions': ['Dispatch i', ...REST],
  reply: ['*Acknowledge & close x', 'Dispatch i', ...REPLY, 'Peek v', 'Jump J'],
  'report with an open step': [...STEP, 'Acknowledge & close x', ...REPLY, 'Peek v', 'Jump J'],
  'reply without a final report': ['*Acknowledge & close x', 'Dispatch i', 'Request final report r', ...REPLY, 'Peek v', 'Jump J'],
  'ended session': ['*Acknowledge & close x', 'Dispatch i', 'Restore run R', ...REPLY, 'Peek v', 'Jump J'],
  'dsh reply': ['*Acknowledge & close x', 'Dispatch i', ...REPLY, 'Peek v', 'Open J'],
  'dsh report with an open step': [...STEP, 'Acknowledge & close x', ...REPLY, 'Peek v', 'Open J'],
  'dsh reply without a final report': ['*Acknowledge & close x', 'Dispatch i', 'Request final report r', ...REPLY, 'Peek v', 'Open J'],
  notice: ['*Acknowledge a', 'Dispatch i', ...REST],
  'notice with an open step': [...STEP, ...REPLY],
};
// Keys that work without a button. Every card takes the other Dispatch modes and the Note
// field. Other works only where the card takes a custom answer.
const KEYS_WITHOUT_BUTTON = ['p', 'F', 'A', 'P', 'n'],
  TAKES_OTHER = ['ask', 'ask with questions', 'dsh ask', 'plan', 'dsh plan', 'finding with questions'];

// Pinned on purpose, because a key that moves makes the user relearn it.
const KEYS = {
  plan: 'p',
  dispatch: 'i',
  fix: 'F',
  act: 'A',
  planInto: 'P',
  doNext: 'd',
  close: 'x',
  request: 'r',
  restore: 'R',
  ack: 'a',
  skip: 's',
  context: 't',
  later: 'l',
  peek: 'v',
  jump: 'J',
  note: 'n',
  other: 'o',
};

test('every action keeps one key', () => {
  const keys = Object.fromEntries(Object.entries(ACTIONS).map(([id, action]) => [id, action.key]));
  assert.deepEqual(keys, KEYS);
  assert.equal(new Set(Object.values(keys)).size, Object.keys(keys).length, 'no two actions share a key');
});

test('each card shows one button per action it offers, under that action key', () => {
  assert.deepEqual(Object.keys(DOCKS), Object.keys(SHAPES), 'every shape has its dock listed');
  for (const [name, card] of Object.entries(SHAPES)) {
    const buttons = buttonsOf(card);
    assert.deepEqual(
      buttons.map((item) => `${item.primary ? '*' : ''}${item.label} ${item.key}`),
      DOCKS[name],
      `${name}: the dock`,
    );
    for (const item of buttons) {
      assert.equal(item.shortcuts.split(' ')[0], item.key, `${name}: ${item.label} advertises the key it shows`);
      assert.equal(item.shortcuts.endsWith(' Enter'), item.primary, `${name}: only the primary ${item.label} runs on Enter`);
    }
  }
});

test('a button and its key run the same action', () => {
  for (const [name, card] of Object.entries(SHAPES))
    for (const item of buttonsOf(card)) {
      const clicked = ran(item.click);
      assert.equal(clicked.length, 1, `${name}: ${item.label} runs one action`);
      assert.deepEqual(press(card, item.key), clicked, `${name}: ${item.key} runs ${item.label}`);
    }
});

test('a key does nothing on a card that does not offer its action', () => {
  for (const [name, card] of Object.entries(SHAPES)) {
    const live = new Set([
      ...buttonsOf(card).map((item) => item.key),
      ...KEYS_WITHOUT_BUTTON,
      ...(TAKES_OTHER.includes(name) ? ['o'] : []),
    ]);
    for (const key of Object.values(KEYS))
      assert.equal(press(card, key).length, live.has(key) ? 1 : 0, `${name}: ${key} ${live.has(key) ? 'runs' : 'is inert'}`);
  }
});

test('Enter runs the primary button, and the recommended option on a card without one', () => {
  for (const [name, card] of Object.entries(SHAPES)) {
    const lead = buttonsOf(card).find((item) => item.primary),
      expected = lead
        ? ran(lead.click)
        : [card.questions ? `step ${card.id}` : `pick ${card.id} 0`];
    assert.deepEqual(ran(() => context.primary(card)), expected, `${name}: Enter`);
  }
});

test('each key keeps one meaning on every card', () => {
  const { ask, finding, reply, notice } = SHAPES,
    report = SHAPES['report with an open step'];
  assert.deepEqual(press(reply, 'x'), ['close done:%5:entry']);
  assert.deepEqual(press(ask, 'x'), ['close ask'], 'x closes the session of a prompt too');
  assert.deepEqual(press(SHAPES['dsh reply'], 'x'), ['close done:dsh:d:1'], 'x closes a dsh session');
  assert.deepEqual(press(reply, 'a'), ['answer done:%5:entry']);
  assert.deepEqual(press(notice, 'a'), ['answer notice']);
  assert.deepEqual(press(SHAPES['reply without a final report'], 'r'), ['report done:%7:entry']);
  assert.deepEqual(press(SHAPES['ended session'], 'R'), ['restore exited:%8:-']);
  assert.deepEqual(press(finding, 's'), ['skip finding']);
  assert.deepEqual(press(finding, 't'), ['context finding']);
  assert.deepEqual(press(finding, 'l'), ['later']);
  assert.deepEqual(press(finding, 'n'), ['focus note']);
  assert.deepEqual(press(ask, 'o'), ['other ask']);
  assert.deepEqual(press(ask, 'v'), ['peek ask']);
  assert.deepEqual(press(ask, 'J'), ['jump ask']);
  assert.deepEqual(press(SHAPES['dsh ask'], 'J'), ['open dsh'], 'J opens dsh for a dsh card');
  // Dispatch opens in the card's session. A report's open step starts a session of its own.
  for (const [key, mode] of Object.entries({ i: 'investigate', F: 'investigate-fix', A: 'act', p: 'plan', P: 'plan-into' }))
    assert.deepEqual(press(finding, key), [`dispatch finding ${mode} card session`]);
  assert.deepEqual(press(report, 'p'), ['dispatch done:%6:entry plan own session']);
  assert.deepEqual(press(report, 'd'), ['dispatch done:%6:entry act own session']);
  assert.deepEqual(press(report, 'i'), ['dispatch done:%6:entry investigate card session']);
});

test('Dispatch advertises the mode keys that have no button of their own', () => {
  const shortcuts = (card) => buttonsOf(card).find((item) => item.label === 'Dispatch').shortcuts;
  assert.equal(shortcuts(SHAPES.finding), 'i F A p P');
  assert.equal(shortcuts(SHAPES['report with an open step']), 'i F A P', 'p belongs to Plan next step there');
});

test('an action announces the same result on every kind of card', () => {
  for (const kind of ['ask', 'plan', 'finding', 'info']) {
    const card = { id: kind, kind, title: 'Title', options: [{ label: 'Yes' }] },
      said = (verb, body = {}, choice) => context.resultMessage(card, verb, body, choice);
    assert.equal(said('skip'), 'Skipped · Title', kind);
    assert.equal(said('later', { minutes: 60 }, { label: '1 hour' }), 'Later 1 hour · Title', kind);
    assert.equal(said('close'), 'Closed · Title', kind);
    assert.equal(said('dispatch', { mode: 'investigate' }), 'Investigating · Title', kind);
    assert.equal(said('dispatch', { mode: 'act' }), 'Acting · Title', kind);
    assert.equal(said('dispatch', { mode: 'investigate-fix' }), 'Investigating and fixing · Title', kind);
    assert.equal(said('dispatch', { mode: 'plan-into' }), 'Planning · Title', kind);
  }
});

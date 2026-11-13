// A card shortcut names an action, never a button slot: these tests hold every dock button to the
// same key wherever the card places it, and hold the key handler to the same table the dock draws.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const tableSource = (name) => {
  const source = app.match(new RegExp(`^  const ${name} = \\{[\\s\\S]*?^  \\};`, 'm'))?.[0];
  assert.ok(source, `app.js defines the ${name} table`);
  return source;
};
const functionSource = (name) => {
  const source = app.match(new RegExp(`^  function ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'))?.[0];
  assert.ok(source, `app.js defines ${name}`);
  return source;
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
const buttonText = (item) => (item.children || []).filter((child) => typeof child === 'string').join(''),
  hintOf = (item) => {
    const kbd = (item.children || []).find((child) => child?.tag === 'kbd');
    return kbd ? (kbd.children || []).join('') : null;
  };

function loadTables() {
  const context = vm.createContext({});
  vm.runInContext(`${tableSource('KEYS')}\nglobalThis.KEYS = KEYS;`, context);
  return context.KEYS;
}

const KEYS = loadTables();

// Pinned on purpose: a key that moves is a retrained hand.
const EXPECTED = {
  ack: 'a',
  close: 'x',
  plan: 'p',
  doNext: 'd',
  dispatch: 'i',
  act: 'A',
  implement: 'I',
  planInto: 'P',
  context: 't',
  later: 'l',
  note: 'n',
  other: 'o',
  peek: 'v',
  jump: 'J',
  request: 'r',
  restore: 'R',
  skip: 's',
};

test('every action keeps one key', () => {
  assert.deepEqual({ ...KEYS }, EXPECTED, 'the KEYS table is the card shortcut contract');
  assert.equal(new Set(Object.values(KEYS)).size, Object.values(KEYS).length, 'no two actions share a key');
});

function dockFor(card, withPane) {
  const actions = [],
    state = { pending: null, laterOpen: false, remoteSwapUntil: 0 };
  const context = vm.createContext({
    S: state,
    h: node,
    el: (tag, props, ...children) => node(tag, props || {}, ...children),
    button: (_card, _control, className, props, onClick, ...children) =>
      node('button', { ...props, class: className, onClick }, ...children),
    paneOf: (current) => current.source?.pane || null,
    restorable: (current) => /^(exited|reload:exited):/.test(current.id),
    restoreRun: (current) => actions.push(['restore', current]),
    acknowledge: (current) => actions.push(['ack', current]),
    closePane: (current) => actions.push(['close', current]),
    planNext: (current) => actions.push(['plan', current]),
    doRecommended: (current) => actions.push(['do', current]),
    requestReport: (current) => actions.push(['request', current]),
    openDispatch: (current, mode) => actions.push(['dispatch', current, mode]),
    openContext: (id) => actions.push(['context', id]),
    skip: (current) => actions.push(['skip', current]),
    toggleLater() {},
    later() {},
    paneButtons: () => node('div', 'pane-actions'),
  });
  vm.runInContext(
    `${tableSource('KEYS')}\n${functionSource('dock')}\nglobalThis.dock = dock;`,
    context,
  );
  return { dock: context.dock(card, withPane), actions };
}

const keyed = (dock) =>
  findNodes(dock, (item) => item.tag === 'button' && hintOf(item) != null).map((item) => ({
    label: buttonText(item),
    key: hintOf(item),
    shortcuts: item.props['aria-keyshortcuts'],
    primary: item.props['data-primary'] === 'true',
    click: item.props.onClick,
  }));

test('each dock button shows the key of its action, wherever the card puts it', () => {
  const report = { id: 'info-report', kind: 'info', source: { pane: '%1' }, star: { action: 'Grant access.' } },
    shapes = {
      'report with a pane': { card: report, withPane: true },
      'report without a pane': { card: { ...report, source: undefined }, withPane: false },
      'reply with a pane': { card: { id: 'info-pane', kind: 'info', source: { pane: '%2' } }, withPane: true },
      'reply without a pane': { card: { id: 'info-bare', kind: 'info' }, withPane: false },
      'report due': {
        card: { id: 'info-due', kind: 'info', source: { pane: '%3' }, report_due: true },
        withPane: true,
      },
      'restorable reply': {
        card: { id: 'exited:%4', kind: 'info', source: { pane: '%8' } },
        withPane: true,
      },
      ask: { card: { id: 'ask', kind: 'ask', options: [{ label: 'Yes' }] }, withPane: false },
      'ask with a pane': {
        card: { id: 'ask-pane', kind: 'ask', options: [{ label: 'Yes' }], source: { pane: '%5' } },
        withPane: true,
      },
      'plan with a pane': {
        card: { id: 'plan', kind: 'plan', options: [{ label: 'Approve' }], source: { pane: '%6' } },
        withPane: true,
      },
      'dsh card': {
        card: {
          id: 'dsh',
          kind: 'info',
          engine: 'dsh',
          source: { uri: 'dsh://session/abc', pane: 'dsh:abc' },
        },
        withPane: true,
      },
    };
  for (const [name, { card, withPane }] of Object.entries(shapes)) {
    for (const item of keyed(dockFor(card, withPane).dock)) {
      assert.ok(
        Object.values(EXPECTED).includes(item.key),
        `${name}: ${item.label} shows a known key`,
      );
      assert.equal(
        item.shortcuts.split(' ')[0],
        item.key,
        `${name}: ${item.label} advertises the key it shows`,
      );
      if (!item.primary) continue;
      assert.equal(
        item.shortcuts,
        `${item.key} Enter`,
        `${name}: the primary ${item.label} runs on Enter beside its own key`,
      );
    }
  }
});

test('Acknowledge is a and Acknowledge & close is x in every layout', () => {
  const byLabel = (dock, label) =>
    keyed(dock).filter((item) => item.label === label);
  const report = { id: 'info-report', kind: 'info', source: { pane: '%1' }, star: { action: 'Grant access.' } },
    reply = { id: 'info-pane', kind: 'info', source: { pane: '%2' } },
    bare = { id: 'info-bare', kind: 'info' };

  const reportKeys = byLabel(dockFor(report, true).dock, 'Acknowledge');
  assert.equal(reportKeys.length, 1, 'the report offers plain Acknowledge once');
  assert.equal(reportKeys[0].key, 'a');
  assert.equal(reportKeys[0].shortcuts, 'a');
  assert.equal(
    byLabel(dockFor(report, true).dock, 'Plan next step')[0].key,
    'p',
    'Plan next step never takes over the acknowledge key',
  );

  const replyKeys = byLabel(dockFor(reply, true).dock, 'Acknowledge & close');
  assert.equal(replyKeys.length, 1, 'the reply shows the close action once');
  assert.equal(replyKeys[0].key, 'x');
  assert.equal(replyKeys[0].primary, true);
  assert.equal(byLabel(dockFor(reply, true).dock, 'Acknowledge')[0].key, 'a');

  const bareKeys = byLabel(dockFor(bare, false).dock, 'Acknowledge');
  assert.equal(bareKeys.length, 1, 'a bare reply keeps one Acknowledge button');
  assert.equal(bareKeys[0].key, 'a');
  assert.equal(bareKeys[0].primary, true);
});

test('one key runs one action on every card shape', () => {
  const calls = [],
    cards = {
      info: { id: 'info', kind: 'info', source: { pane: '%1' } },
      report: { id: 'report', kind: 'info', source: { pane: '%2' }, star: { action: 'Fix it' } },
      ask: { id: 'ask', kind: 'ask', options: [{ label: 'Yes' }] },
      finding: { id: 'finding', kind: 'finding', options: [{ label: 'Fix' }] },
      plan: { id: 'plan', kind: 'plan', options: [{ label: 'Approve' }] },
    },
    context = vm.createContext({
      $: (selector) => (selector === '#note' ? { focus: () => calls.push(['focus']) } : null),
      acknowledge: (card) => calls.push(['ack', card.id]),
      closePane: (card) => calls.push(['close', card.id]),
      planRecommended: (card) => calls.push(['plan', card.id]),
      openDispatch: (card, mode) => calls.push(['dispatch', card.id, mode]),
      doRecommended: (card) => calls.push(['do', card.id]),
      openContext: (id) => calls.push(['context', id]),
      toggleLater: () => calls.push(['later']),
      otherAllowed: () => true,
      toggleOther: (card) => calls.push(['other', card.id]),
      openPeek: (card) => calls.push(['peek', card.id]),
      jump: (card) => calls.push(['jump', card.id]),
      requestReport: (card) => calls.push(['request', card.id]),
      restoreRun: (card) => calls.push(['restore', card.id]),
      skip: (card) => calls.push(['skip', card.id]),
    });
  vm.runInContext(
    `${tableSource('KEYS')}\n${tableSource('CARD_KEYS')}\n${functionSource('planNext')}\nglobalThis.KEYS = KEYS;\nglobalThis.CARD_KEYS = CARD_KEYS;`,
    context,
  );
  const CARD_KEYS = context.CARD_KEYS,
    press = (action, card) => {
      calls.length = 0;
      CARD_KEYS[KEYS[action]](card);
      return calls;
    };

  for (const action of Object.keys(KEYS))
    assert.equal(typeof CARD_KEYS[KEYS[action]], 'function', `a handler runs ${action}`);
  assert.equal(
    Object.keys(CARD_KEYS).length,
    Object.keys(KEYS).length,
    'a key with no action cannot linger in the handler',
  );

  for (const name of ['info', 'report', 'ask', 'finding', 'plan'])
    assert.deepEqual(press('ack', cards[name]), name === 'info' || name === 'report' ? [['ack', cards[name].id]] : [], `${name}: a acknowledges only`);

  assert.deepEqual(press('close', cards.info), [['close', 'info']]);
  assert.deepEqual(press('skip', cards.ask), [['skip', 'ask']]);
  assert.deepEqual(press('context', cards.info), [['context', 'info']]);
  assert.deepEqual(press('later', cards.info), [['later']]);
  assert.deepEqual(press('note', cards.info), [['focus']]);
  assert.deepEqual(press('other', cards.finding), [['other', 'finding']]);
  assert.deepEqual(press('peek', cards.info), [['peek', 'info']]);
  assert.deepEqual(press('jump', cards.info), [['jump', 'info']]);
  assert.deepEqual(press('request', cards.info), [['request', 'info']]);
  assert.deepEqual(press('restore', cards.info), [['restore', 'info']]);
  assert.deepEqual(press('doNext', cards.report), [['do', 'report']]);
  assert.deepEqual(press('doNext', cards.ask), [], 'do next step needs a report with an open step');
  assert.deepEqual(press('plan', cards.report), [['plan', 'report']], 'p plans a report as its next step');
  assert.deepEqual(press('plan', cards.ask), [['dispatch', 'ask', 'plan']], 'p opens Dispatch with Plan elsewhere');
  for (const [action, mode] of Object.entries({ dispatch: 'investigate', act: 'act', implement: 'implement', planInto: 'plan-into' }))
    assert.deepEqual(press(action, cards.ask), [['dispatch', 'ask', mode]], `${action} opens Dispatch as ${mode}`);
});

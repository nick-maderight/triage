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

const buttonText = (item) =>
  (item.children || []).filter((child) => typeof child === 'string').join('');

function harness() {
  const state = { pending: null, laterOpen: false, remoteSwapUntil: 0 },
    actions = [],
    context = vm.createContext({
      S: state,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_card, _control, className, props, onClick, ...children) =>
        node('button', { ...props, class: className, onClick }, ...children),
      paneOf: (card) => card.source?.pane || null,
      answerable: (card) => (card.kind !== 'ask' && card.kind !== 'plan') || card.answerable !== false,
      closePane: (card) => actions.push(['close', card]),
      acknowledge: (card) => actions.push(['ack', card]),
      planRecommended: (card) => actions.push(['plan', card]),
      requestReport: (card) => actions.push(['report', card]),
      openDispatch() {},
      toggleLater() {},
      later() {},
      paneButtons: () => node('div', 'pane-actions'),
      pick() {},
      recIndex: () => 0,
      openPeek() {},
    });
  vm.runInContext(
    `${functionSource('dock')}\n${functionSource('primary')}\nObject.assign(globalThis, { dock, primary });`,
    context,
  );
  return { context, actions, state };
}

const primaryOf = (dockNode) => dockNode.children.find((child) => child.props?.['data-primary'] === 'true'),
  groupOf = (dockNode) => dockNode.children.find((child) => child.props?.class === 'dock-group');

test('info card with a pane makes Acknowledge & close primary and Acknowledge secondary', () => {
  const { context, actions } = harness(),
    card = { id: 'info-pane', kind: 'info', source: { pane: '%12' } },
    dock = context.dock(card, true),
    primary = primaryOf(dock);
  assert.ok(primary, 'the info card has a primary button');
  assert.equal(buttonText(primary), 'Acknowledge & close');
  primary.props.onClick();
  assert.equal(actions[0][0], 'close', 'the primary button closes the pane');
  const secondary = findNodes(
    groupOf(dock),
    (item) => item.tag === 'button' && item.props?.class === 'btn' && buttonText(item) === 'Acknowledge',
  )[0];
  assert.ok(secondary, 'plain Acknowledge stays a secondary button');
  secondary.props.onClick();
  assert.equal(actions[1][0], 'ack', 'the secondary button acknowledges without closing');
  context.primary(card);
  assert.equal(actions[2][0], 'close', 'Enter runs the primary action and closes the pane');
});

test('info card without a pane keeps plain Acknowledge primary and unique', () => {
  const { context, actions } = harness(),
    card = { id: 'info-bare', kind: 'info' },
    dock = context.dock(card, false),
    primary = primaryOf(dock);
  assert.ok(primary, 'the info card has a primary button');
  assert.equal(buttonText(primary), 'Acknowledge');
  primary.props.onClick();
  assert.equal(actions[0][0], 'ack');
  assert.equal(
    findNodes(dock, (item) => item.tag === 'button' && /Acknowledge/.test(buttonText(item))).length,
    1,
    'no duplicate acknowledge button',
  );
  context.primary(card);
  assert.equal(actions[1][0], 'ack', 'Enter acknowledges when there is no pane to close');
});

test('info card with a recommended action makes Plan recommended step primary', () => {
  const { context, actions } = harness(),
    card = { id: 'info-rec', kind: 'info', source: { pane: '%12' }, star: { action: 'Grant read access.' } },
    dock = context.dock(card, true),
    primary = primaryOf(dock);
  assert.ok(primary, 'the recommended-action card has a primary button');
  assert.equal(buttonText(primary), 'Plan recommended step');
  primary.props.onClick();
  assert.equal(actions[0][0], 'plan', 'the primary button opens a plan dispatch');
  assert.ok(
    findNodes(
      groupOf(dock),
      (item) => item.tag === 'button' && buttonText(item) === 'Acknowledge & close',
    )[0],
    'the dock group holds an Acknowledge & close button',
  );
  context.primary(card);
  assert.equal(actions[1][0], 'plan', 'Enter hands the recommended step back as a plan dispatch');
  const bare = { ...card, source: undefined },
    bareDock = context.dock(bare, false);
  assert.ok(
    findNodes(
      groupOf(bareDock),
      (item) => item.tag === 'button' && buttonText(item) === 'Acknowledge',
    )[0],
    'without a pane the dock group holds a plain Acknowledge button',
  );
});

test('cards answered through options keep Acknowledge & close secondary only', () => {
  const { context, actions } = harness(),
    card = { id: 'plan-pane', kind: 'plan', source: { pane: '%5' } },
    dock = context.dock(card, true);
  assert.equal(primaryOf(dock), undefined, 'no primary acknowledge button on option cards');
  const close = findNodes(
    groupOf(dock),
    (item) => item.tag === 'button' && buttonText(item) === 'Acknowledge & close',
  )[0];
  assert.ok(close, 'the close action stays a secondary dock button');
  close.props.onClick();
  assert.equal(actions[0][0], 'close');
});
test('report-due card offers secondary Request final report without changing primary', () => {
  const { context, actions } = harness(),
    card = { id: 'info-report', kind: 'info', report_due: true, source: { pane: '%12' } },
    dock = context.dock(card, true),
    primary = primaryOf(dock),
    request = findNodes(groupOf(dock), (item) => item.tag === 'button' && buttonText(item) === 'Request final report')[0];
  assert.equal(buttonText(primary), 'Acknowledge & close');
  assert.ok(request, 'the dock offers the report request');
  request.props.onClick();
  assert.equal(actions[0][0], 'report');
  assert.equal(actions[0][1], card);
});

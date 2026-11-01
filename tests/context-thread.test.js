const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const markedSource = fs.readFileSync(path.join(__dirname, '..', 'ui', 'marked.js'), 'utf8');
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

const textOf = (value) =>
  typeof value === 'string' ? value : (value?.children || []).map(textOf).join('');

// Loads the real contextItem with its helpers, the rest of the page stubbed.
function itemContext(state = {}) {
  const context = vm.createContext({
    S: { openBlocks: new Set(), ...state },
    h: node,
    $: () => null,
    toggleBlock() {},
    ...state.stubs,
  });
  vm.runInContext(markedSource, context);
  vm.runInContext(
    slice('  const el =', '\n  function draft') +
      slice('  const tf =', '  const heading =') +
      slice('  const clampable =', '  function block') +
      [functionSource('sourceName'), functionSource('mdNodes'), functionSource('md'), functionSource('block'), functionSource('contextItem')].join('\n') +
      '\nObject.assign(globalThis, { contextItem });',
    context,
  );
  return context;
}

function item(row, anchorId) {
  return itemContext().contextItem(row, anchorId);
}

test('the context thread marks the anchor card and names it', () => {
  const row = { card: { id: 'done:%5:a2', kind: 'info', created_ms: 1700000000000, source: {}, title: 'Restatement' }, swipes: [{ action: 'done', at_ms: 1700000000001 }] };
  const anchor = item(row, 'done:%5:a2');
  assert.ok(anchor.props.class.split(' ').includes('anchor'), 'anchor item carries its class');
  assert.ok(anchor.props.class.split(' ').includes('context-item'));
  assert.equal(anchor.props.tabindex, '-1');
  const tags = findNodes(anchor, (n) => n.props.class === 'tag').map(textOf);
  assert.deepEqual(tags, ['This card']);
  const plain = item({ ...row, card: { ...row.card, id: 'done:%5:a1' } }, 'done:%5:a2');
  assert.ok(plain.props.tabindex == null, 'plain items carry no tabindex');
  assert.ok(!plain.props.class.includes('anchor'), 'other items are not anchors');
});

test('a context item shows the latest swipe with its steering note', () => {
  const row = {
    card: { id: 'done:%5:a2', kind: 'info', created_ms: 1700000000000, source: {}, title: 'Findings stand', options: [{ label: 'Investigate' }, { label: 'Acknowledge' }] },
    swipes: [
      { action: 'done', reason: 'launched %50', at_ms: 1700000000001 },
      { action: 'done', option: 0, text: 'Investigate', note: 'so what should i do?', at_ms: 1700000000002 },
    ],
  };
  const view = item(row, 'other');
  const result = findNodes(view, (n) => n.props.class === 'h-result').map(textOf).join('');
  assert.ok(result.includes('done'), result);
  assert.ok(!result.includes('launched %50'), 'only the latest swipe shows');
  assert.ok(result.includes('Investigate'), result);
  assert.ok(result.includes('Note: so what should i do?'), 'the steering note rides along');
  const queued = item({ card: { id: 'ask:%5:q1', kind: 'ask', created_ms: 1, source: {} }, swipes: [] }, 'other');
  assert.ok(textOf(queued).includes('queued'), 'a card without swipes reads as queued');
});

test('a superseded report dims in the thread', () => {
  const superseded = item(
    { card: { id: 'done:%5:a1', kind: 'info', created_ms: 1, source: {}, title: 'Answer' }, swipes: [{ action: 'done', reason: 'superseded', at_ms: 2 }] },
    'done:%5:a2',
  );
  assert.ok(superseded.props.class.split(' ').includes('superseded'));
  const live = item(
    { card: { id: 'done:%5:a1', kind: 'info', created_ms: 1, source: {}, title: 'Answer' }, swipes: [{ action: 'done', at_ms: 2 }] },
    'done:%5:a2',
  );
  assert.ok(!live.props.class.includes('superseded'));
});

test('a long body clamps behind Show more inside the thread', () => {
  const body = 'Answer: card premise held.\n'.repeat(30);
  const view = item({ card: { id: 'done:%5:a2', kind: 'info', created_ms: 1, source: {}, body }, swipes: [] }, 'done:%5:a2');
  const section = findNodes(view, (n) => (n.props.class || '').includes('clamp'))[0];
  assert.ok(section, 'the report body renders in a clamping block');
  assert.ok(textOf(section).includes(body.trim().split('\n')[0]), 'the body text renders');
  const more = findNodes(section, (n) => (n.props.class || '').includes('more')).map(textOf);
  assert.deepEqual(more, ['Show more']);
  const empty = item({ card: { id: 'ask:%5:q1', kind: 'ask', created_ms: 1, source: {} }, swipes: [] }, 'ask:%5:q1');
  assert.equal(findNodes(empty, (n) => (n.props.class || '').includes('clamp')).length, 0, 'no Report block without a body');
});

test('an open Report block in the context sheet keeps its Show less control', () => {
  const sections = [
    {
      classList: { contains: (name) => name === 'open' },
      closest: () => ({}),
      more: { hidden: false },
    },
    {
      classList: { contains: (name) => name === 'open' },
      closest: () => null,
      more: { hidden: false },
    },
  ];
  const context = vm.createContext({
    document: { querySelectorAll: () => sections },
    matchMedia: () => ({ matches: false }),
    $: (sel, root) => {
      if (sel === '.more') return root.more;
      if (sel === '.block-content > .md') return { scrollHeight: 300, clientHeight: 72 };
      return null;
    },
  });
  vm.runInContext(functionSource('updateClamps'), context);
  vm.runInContext('updateClamps()', context);
  assert.equal(sections[0].more.hidden, false, 'dialog Report stays expandable while open');
  assert.equal(sections[1].more.hidden, true, 'deck blocks outside the sheet behave as before');
});

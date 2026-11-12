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
const asyncFunctionSource = (name) => {
  const source = app.match(new RegExp(`^  async function ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'))?.[0];
  assert.ok(source, `app.js defines async ${name}`);
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

test('custom answers are hidden from findings and available on ask and plan cards', () => {
  const state = { pending: null, drafts: new Map() },
    requests = [],
    functions = ['otherAllowed', 'optionsView', 'questionView', 'otherRow', 'toggleOther', 'sendOther', 'answerStep', 'goStep', 'mdInline', 'mdNodes']
      .map(functionSource)
      .join('\n'),
    context = vm.createContext({
      S: state,
      answerable: (card) => card.answerable !== false,
      draft(card) {
        if (!state.drafts.has(card.id)) state.drafts.set(card.id, { step: 0, answers: [], note: '', other: '', open: false });
        return state.drafts.get(card.id);
      },
      recIndex: () => 0,
      ident: (name, card) => `${name}-${card.id}`,
      paneOf: () => null,
      paneButtons: () => null,
      planExecutorView: () => null,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_card, _action, className, props, onClick, ...children) =>
        node('button', { ...props, class: className, onClick }, ...children),
      render() {},
      micButton: () => null,
      snapshot: () => ({}),
      announce() {},
      focusPrimary() {},
      $: () => ({ focus() {} }),
      noteValue: (card) => state.drafts.get(card.id).note.trim() || null,
      act: (...args) => requests.push(args),
    });
  vm.runInContext(markedSource, context);
  vm.runInContext(
    `${functions}\nObject.assign(globalThis, { optionsView, toggleOther, sendOther });`,
    context,
  );

  const finding = {
      id: 'finding',
      kind: 'finding',
      options: [{ label: 'Keep current plan' }],
      allow_other: false,
    },
    findingQuestion = {
      id: 'finding-question',
      kind: 'finding',
      options: [],
      allow_other: true,
      questions: [{ label: 'q', question: 'Choose', allow_other: true, options: [{ label: 'Keep' }] }],
    },
    ask = {
      id: 'ask',
      kind: 'ask',
      options: [{ label: 'Use default' }],
      allow_other: true,
    },
    plan = {
      id: 'plan',
      kind: 'plan',
      options: [{ label: 'Approve' }, { label: 'Refine' }],
      allow_other: true,
    };
  state.drafts.set(findingQuestion.id, { step: 0, answers: [], note: '', other: 'custom finding answer', open: false });
  state.drafts.set(finding.id, { note: '', other: 'finding answer', open: false });
  state.drafts.set(ask.id, { note: '', other: 'ask answer', open: false });
  state.drafts.set(plan.id, { note: 'Plan note', other: 'Plan answer', open: false });

  assert.equal(
    findNodes(context.optionsView(finding), (item) => item.props?.id === 'other-btn').length,
    0,
    'finding exposes no Other button',
  );
  context.toggleOther(finding);
  context.sendOther(finding);
  assert.equal(state.drafts.get(finding.id).open, false);
  assert.equal(requests.length, 0, 'finding custom answer is never submitted');
  const findingQuestionButton = findNodes(context.optionsView(findingQuestion), (item) => item.props?.id === 'other-btn')[0];
  assert.ok(findingQuestionButton, 'finding questions expose Other');
  context.sendOther(findingQuestion);
  assert.equal(state.drafts.get(findingQuestion.id).answers[0].text, 'custom finding answer');

  const askButton = findNodes(context.optionsView(ask), (item) => item.props?.id === 'other-btn')[0];
  assert.ok(askButton, 'Ask cards retain Other');
  askButton.props.onClick();
  assert.ok(
    findNodes(context.optionsView(ask), (item) => item.props?.class === 'other-form').length,
    'Ask cards can open the Other form',
  );
  context.sendOther(ask);
  assert.equal(requests.length, 1);
  assert.equal(requests[0][0], ask);
  assert.equal(requests[0][1], 'answer');
  assert.equal(requests[0][2].option, null);
  assert.equal(requests[0][2].text, 'ask answer');
  const planButton = findNodes(context.optionsView(plan), (item) => item.props?.id === 'other-btn')[0];
  assert.ok(planButton, 'Plan cards retain Other');
  planButton.props.onClick();
  assert.ok(
    findNodes(context.optionsView(plan), (item) => item.props?.class === 'other-form').length,
    'Plan cards can open the Other form',
  );
  context.sendOther(plan);
  assert.equal(requests.length, 2);
  assert.equal(requests[1][0], plan);
  assert.equal(requests[1][1], 'answer');
  assert.equal(requests[1][2].option, null);
  assert.equal(requests[1][2].text, 'Plan answer');
  assert.equal(requests[1][2].note, 'Plan note');
});
test('plan cards from the queue render and answer through the answer route', async () => {
  const card = {
      id: 'plan-card',
      kind: 'plan',
      title: 'Plan ready: Split parser helpers',
      body: 'Move parsing into small helpers.',
      answerable: true,
      allow_other: true,
      options: [{ label: 'Approve', recommended: true }, { label: 'Refine' }],
    },
    queue = { cards: [card], version: 1 },
    state = {
      cards: [],
      counts: {},
      version: 0,
      idx: 0,
      shownId: null,
      drafts: new Map(),
      pending: null,
      initialized: false,
      history: { loaded: true, error: null },
      remoteSwapUntil: 0,
      inflight: new Set(),
      direction: 1,
      actionRevision: 0,
      dirty: false,
      refreshPromise: null,
      refreshRetryTimer: 0,
      refreshRetryDelay: 1000,
      streamOpen: false,
      openBlocks: new Set(),
      shakeId: null,
      laterOpen: false,
    },
    requests = [],
    toasts = [],
    functions = [
      'applyQueue',
      'countsFrom',
      'setConnection',
      'draft',
      'refresh',
      'optionsView',
      'otherAllowed',
      'questionView',
      'otherRow',
      'noteRow',
      'toggleOther',
      'sendOther',
      'pick',
      'cardView',
    ]
      .map(functionSource)
      .join('\n'),
    helpersStart = app.indexOf('  const heading ='),
    helpersEnd = app.indexOf('  const recIndex =', helpersStart),
    viewStart = app.indexOf('  const prose ='),
    viewEnd = app.indexOf('  function titleRow', viewStart),
    shared = app.slice(helpersStart, helpersEnd) + app.slice(viewStart, viewEnd),
    context = vm.createContext({
      S: state,
      snapshot: () => ({}),
      byPriority: () => 0,
      toast: (message, error = false) => toasts.push({ message, error }),
      announce() {},
      api: async (url, body) => {
        requests.push({ url, body });
        return url === '/v1/queue' ? queue : {};
      },
      resultMessage: () => 'Sent answer',
      render() {},
      micButton: () => null,
      emptyHistory() {},
      window: { matchMedia: () => ({ matches: true }) },
      $: (selector) => (selector === '#other-text' || selector === '#other-btn' ? { focus() {} } : null),
      reloadIfStale: () => false,
      enc: encodeURIComponent,
      draft(item) {
        if (!state.drafts.has(item.id))
          state.drafts.set(item.id, { note: '', other: '', open: false, dispatch: '', dispatchMode: 'investigate' });
        return state.drafts.get(item.id);
      },
      recIndex: (item) => Math.max(0, item.options.findIndex((option) => option.recommended)),
      ident: (name, item) => `${name}-${item.id}`,
      paneButtons: () => null,
      planExecutorView: () => null,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_item, _action, className, props, onClick, ...children) =>
        node('button', { ...props, class: className, onClick }, ...children),
      queueLine: () => null,
      facts: () => null,
      titleRow: (item) => [node('h2', { id: 'card-title' }, item.title)],
      dock: () => null,
      report: () => null,
      toggleBlock() {},
      noteValue: (item) => state.drafts.get(item.id).note.trim() || null,
      Date: { now: () => 1000 },
    });
  vm.runInContext(markedSource, context);
  vm.runInContext(
    `${shared}\n${functions}\n${asyncFunctionSource('act')}\n${asyncFunctionSource('settle')}\nObject.assign(globalThis, { refresh, cardView, optionsView });`,
    context,
  );
  let rendered = null;
  context.render = () => {
    const current = state.cards[state.idx];
    rendered = current ? context.cardView(current, false) : null;
  };

  await context.refresh();

  assert.equal(requests[0].url, '/v1/queue');
  assert.equal(state.cards[0].kind, 'plan');
  assert.equal(state.counts.plan, 1);
  assert.ok(
    findNodes(rendered, (item) => item.tag === 'h2' && item.children.includes(card.title)).length,
    'the plan title is shown',
  );
  assert.ok(
    findNodes(rendered, (item) => item.tag === 'p' && item.children.includes(card.body)).length,
    'the plan body is shown',
  );
  const labels = findNodes(rendered, (item) => item.props?.class === 'opt-label').map((item) => item.children[0]);
  assert.ok(labels.includes('Approve'));
  assert.ok(labels.includes('Refine'));
  assert.ok(findNodes(rendered, (item) => item.props?.id === 'other-btn').length, 'Other is available');
  assert.ok(findNodes(rendered, (item) => item.props?.id === 'note').length, 'the note field is available');

  const findRefine = () => findNodes(rendered, (item) => item.props?.id === 'opt-1')[0];
  findRefine().props.onClick();
  assert.deepEqual(toasts, [
    { message: 'Add refinement text in Other before choosing Refine.', error: true },
  ]);
  assert.equal(requests.filter((request) => request.url.endsWith('/answer')).length, 0);

  const other = findNodes(rendered, (item) => item.props?.id === 'other-text')[0],
    note = findNodes(rendered, (item) => item.props?.id === 'note')[0];
  other.props.oninput({ target: { value: 'Split the parser into smaller helpers' } });
  note.props.oninput({ target: { value: 'Keep existing callers' } });
  findRefine().props.onClick();

  const answer = requests.find((request) => request.url === '/v1/cards/plan-card/answer');
  assert.equal(answer.body.option, 1);
  assert.equal(answer.body.text, 'Split the parser into smaller helpers');
  assert.equal(answer.body.note, 'Keep existing callers');
  assert.equal('executor' in answer.body, false);
  await new Promise((resolve) => setImmediate(resolve));
});

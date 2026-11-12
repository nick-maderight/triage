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

test('multi-question asks are answered step by step and sent from the review', async () => {
  const card = {
      id: 'ask-multi',
      kind: 'ask',
      title: 'Landing copy',
      answerable: true,
      allow_other: true,
      options: [{ label: 'Approve', recommended: true }, { label: 'Reject' }],
      questions: [
        {
          label: 'f1',
          question: 'F1 — which tone should the hero headline use?',
          options: [{ label: 'Approve', recommended: true }, { label: 'Reject' }],
        },
        {
          label: 'f2',
          question: 'F2 — which billing period shows first?',
          options: [{ label: 'Approve', recommended: true }, { label: 'Reject' }],
        },
        {
          label: 'color',
          question: 'Which color should the button be?',
          options: [{ label: 'Red' }, { label: 'Green', recommended: true }, { label: 'Blue' }],
        },
      ],
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
      'otherAllowed',
      'applyQueue',
      'countsFrom',
      'refresh',
      'draft',
      'optionsView',
      'questionView',
      'reviewView',
      'stepTitle',
      'pickStep',
      'answerStep',
      'goStep',
      'sendAnswers',
      'primaryStep',
      'otherRow',
      'noteRow',
      'toggleOther',
      'sendOther',
      'titleRow',
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
      resultMessage: () => 'Sent answers',
      render() {},
      micButton: () => null,
      emptyHistory() {},
      window: { matchMedia: () => ({ matches: true }) },
      $: (selector) => (selector === '#other-text' || selector === '#other-btn' ? { focus() {} } : null),
      reloadIfStale: () => false,
      enc: encodeURIComponent,
      ident: (name, item) => `${name}-${item.id}`,
      paneButtons: () => null,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_item, _action, className, props, onClick, ...children) =>
        node('button', { ...props, class: className, onClick }, ...children),
      queueLine: () => null,
      facts: () => null,
      dock: () => null,
      report: () => null,
      toggleTitle() {},
      focusPrimary() {},
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

  const byId = (id) => findNodes(rendered, (item) => item.props?.id === id)[0],
    stepText = () =>
      findNodes(rendered, (item) => item.props?.class === 'question-step').map((item) => item.children[0])[0],
    reviewLabels = () =>
      findNodes(rendered, (item) => item.props?.class === 'opt-label').map((item) => item.children[0]);

  assert.equal(state.cards[0].kind, 'ask');
  assert.equal(stepText(), 'Question 1 of 3');
  assert.ok(byId('opt-0'), 'the first question renders its options');

  byId('opt-0').props.onClick();
  assert.equal(stepText(), 'Question 2 of 3');

  byId('opt-1').props.onClick();
  assert.equal(stepText(), 'Question 3 of 3');

  byId('other-btn').props.onClick();
  byId('other-text').props.oninput({ target: { value: 'Violet' } });
  findNodes(rendered, (item) => item.props?.class === 'other-form')[0].props.onsubmit({ preventDefault() {} });

  assert.ok(
    findNodes(rendered, (item) => item.props?.class === 'options review').length,
    'the review step renders',
  );
  assert.deepEqual(reviewLabels(), ['Approve', 'Reject', '“Violet”']);
  assert.equal(byId('send-answers').props.disabled, false);

  byId('answer-1').props.onClick();
  assert.equal(stepText(), 'Question 2 of 3');
  byId('opt-0').props.onClick();
  assert.ok(
    findNodes(rendered, (item) => item.props?.class === 'options review').length,
    'changing an answer returns to the review step',
  );
  assert.deepEqual(reviewLabels(), ['Approve', 'Approve', '“Violet”']);

  assert.equal(
    requests.filter((request) => request.url.endsWith('/answer')).length,
    0,
    'no answer is posted before Send',
  );
  byId('send-answers').props.onClick();
  const answer = requests.find((request) => request.url === '/v1/cards/ask-multi/answer');
  assert.ok(answer, 'Send posts the answers');
  assert.equal(answer.body.note, null);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(JSON.parse(JSON.stringify(answer.body.answers)), [
    { option: 0 },
    { option: 0 },
    { text: 'Violet' },
  ]);
});

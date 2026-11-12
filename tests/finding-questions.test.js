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

test('finding question review allows per-question Other and opens Act dispatch', () => {
  const card = {
      id: 'finding',
      kind: 'finding',
      allow_other: true,
      questions: [{
        label: 'Finding one',
        question: 'Recommended action: rotate the key',
        allow_other: true,
        options: [{ label: 'Do as recommended', recommended: true }, { label: 'Skip' }],
      }],
    },
    state = { pending: null, drafts: new Map() },
    calls = [],
    context = vm.createContext({
      S: state,
      draft(current) {
        if (!state.drafts.has(current.id)) state.drafts.set(current.id, { step: 0, answers: [], other: '', note: '' });
        return state.drafts.get(current.id);
      },
      answerable: () => true,
      ident: (name, current) => `${name}-${current.id}`,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_current, _action, className, props, onClick, ...children) => node('button', { ...props, class: className, onClick }, ...children),
      mdInline: (text) => text,
      noteValue: () => null,
      openDispatch: (...args) => calls.push(args),
      goStep() {},
    });
  vm.runInContext(
    `${functionSource('otherAllowed')}\n${functionSource('otherRow')}\n${functionSource('questionView')}\n${functionSource('reviewView')}\n${functionSource('sendAnswers')}\nglobalThis.ui = { otherAllowed, questionView, reviewView, sendAnswers };`,
    context,
  );

  const questionRows = context.ui.questionView(card, 0),
    otherButton = findNodes(questionRows, (item) => item.props?.id === 'other-btn')[0];
  assert.ok(otherButton, 'finding question exposes its Other answer');
  assert.match(card.questions[0].question, /Recommended action: rotate the key/);
  card.questions[0].allow_other = false;
  assert.equal(context.ui.otherAllowed(card), false, 'question metadata disables Other');
  assert.equal(findNodes(context.ui.questionView(card, 0), (item) => item.props?.id === 'other-btn').length, 0);
  card.questions[0].allow_other = true;
  state.drafts.get(card.id).answers = [{ option: 0 }];
  const review = context.ui.reviewView(card),
    dispatchButton = findNodes(review, (item) => item.props?.id === 'send-answers')[0];
  assert.equal(dispatchButton.children.includes('Dispatch answers'), true);
  dispatchButton.props.onClick();
  assert.deepEqual(calls, [[card, 'act', true, null, [{ option: 0 }]]]);
});
test('finding question without Other hides custom text', () => {
  const card = {
      id: 'finding-no-other',
      kind: 'finding',
      allow_other: true,
      questions: [{ label: 'q', question: 'Select one', allow_other: false, options: [{ label: 'Keep' }] }],
    },
    state = { drafts: new Map([[card.id, { step: 0, answers: [] }]]) },
    context = vm.createContext({
      S: state,
      draft: (current) => state.drafts.get(current.id),
      answerable: () => true,
      ident: (name, current) => `${name}-${current.id}`,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_current, _action, className, props, onClick, ...children) => node('button', { ...props, class: className, onClick }, ...children),
      mdInline: (text) => text,
    });
  vm.runInContext(
    `${functionSource('otherAllowed')}\n${functionSource('otherRow')}\n${functionSource('questionView')}\nglobalThis.ui = { questionView };`,
    context,
  );
  assert.equal(findNodes(context.ui.questionView(card, 0), (item) => item.props?.id === 'other-btn').length, 0);
});

test('Other on a finding question records typed text and moves to the next question', () => {
  const question = (action) => ({
      label: action,
      question: `Recommended action: ${action}`,
      options: [{ label: 'Do as recommended', recommended: true }, { label: 'Skip' }],
    }),
    card = {
      id: 'finding-other',
      kind: 'finding',
      allow_other: true,
      questions: [question('rotate the key'), question('drop the rule')],
    },
    state = { pending: null, remoteSwapUntil: 0, drafts: new Map() },
    context = vm.createContext({
      S: state,
      draft(current) {
        if (!state.drafts.has(current.id)) state.drafts.set(current.id, { step: 0, answers: [], other: '', note: '', open: false });
        return state.drafts.get(current.id);
      },
      answerable: () => true,
      ident: (name, current) => `${name}-${current.id}`,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_current, _action, className, props, onClick, ...children) => node('button', { ...props, class: className, onClick }, ...children),
      mdInline: (text) => text,
      render() {},
      micButton: () => null,
      $: () => ({ focus() {} }),
      goStep: (current, step) => Object.assign(state.drafts.get(current.id), { step, open: false }),
    });
  vm.runInContext(
    `${['otherAllowed', 'otherRow', 'questionView', 'toggleOther', 'sendOther', 'answerStep'].map((name) => functionSource(name)).join('\n')}\nglobalThis.ui = { questionView };`,
    context,
  );
  const nodeWith = (predicate) => findNodes(context.ui.questionView(card, state.drafts.get(card.id)?.step ?? 0), predicate)[0];

  nodeWith((item) => item.props?.id === 'other-btn').props.onClick();
  const form = nodeWith((item) => item.props?.class === 'other-form');
  assert.ok(form, 'Other opens its text field');
  findNodes(form, (item) => item.props?.id === 'other-text')[0].props.oninput({ target: { value: 'debug and fix' } });
  form.props.onsubmit({ preventDefault() {} });
  const answered = state.drafts.get(card.id);
  assert.deepEqual(JSON.parse(JSON.stringify(answered.answers)), [{ text: 'debug and fix' }]);
  assert.equal(answered.step, 1, 'the custom answer moves to the next question');
});

test('an empty Other submit keeps the field open and says what it wants', () => {
  const question = (action) => ({
      label: action,
      question: `Recommended action: ${action}`,
      options: [{ label: 'Do as recommended', recommended: true }, { label: 'Skip' }],
    }),
    card = {
      id: 'finding-other-empty',
      kind: 'finding',
      allow_other: true,
      questions: [question('rotate the key'), question('drop the rule')],
    },
    state = { pending: null, remoteSwapUntil: 0, drafts: new Map() },
    said = [],
    toasts = [],
    steps = [],
    context = vm.createContext({
      S: state,
      draft(current) {
        if (!state.drafts.has(current.id)) state.drafts.set(current.id, { step: 0, answers: [], other: '  ', note: '', open: true });
        return state.drafts.get(current.id);
      },
      answerable: () => true,
      ident: (name, current) => `${name}-${current.id}`,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_current, _action, className, props, onClick, ...children) => node('button', { ...props, class: className, onClick }, ...children),
      mdInline: (text) => text,
      noteValue: () => null,
      toast: (text, error) => toasts.push([text, error]),
      announce: (text) => said.push(text),
      render() {},
      micButton: () => null,
      $: () => ({ focus() {} }),
      goStep: (current, step) => {
        steps.push(step);
        Object.assign(state.drafts.get(current.id), { step, open: false });
      },
    });
  vm.runInContext(
    `${['otherAllowed', 'otherRow', 'questionView', 'toggleOther', 'sendOther', 'answerStep'].map((name) => functionSource(name)).join('\n')}\nglobalThis.ui = { questionView };`,
    context,
  );
  const form = findNodes(context.ui.questionView(card, 0), (item) => item.props?.class === 'other-form')[0];
  form.props.onsubmit({ preventDefault() {} });
  const draft = state.drafts.get(card.id);
  assert.deepEqual(JSON.parse(JSON.stringify(draft.answers)), [], 'an empty custom answer records nothing');
  assert.equal(draft.step, 0, 'an empty custom answer stays on its question');
  assert.equal(draft.open, true, 'the field stays open');
  assert.deepEqual(steps, [], 'no step change');
  assert.deepEqual(toasts, [['Type an answer first.', true]]);
  assert.deepEqual(said, ['Error: Type an answer first.']);
});

test('the review step offers no Other for a question card', () => {
  const card = {
      id: 'finding-other-review',
      kind: 'finding',
      allow_other: true,
      questions: [{ label: 'one', question: 'q', options: [{ label: 'Do as recommended', recommended: true }] }],
    },
    state = { pending: null, remoteSwapUntil: 0, drafts: new Map([[card.id, { step: 1, answers: [{ option: 0 }], other: '', note: '', open: false }]]) },
    context = vm.createContext({
      S: state,
      draft: (current) => state.drafts.get(current.id),
      answerable: () => true,
      announce() {},
      render() {},
      $: () => ({ focus() {} }),
    });
  vm.runInContext(`${functionSource('otherAllowed')}\nglobalThis.ui = { otherAllowed };`, context);
  assert.equal(context.ui.otherAllowed(card), false, 'the review step has no Other answer');
  card.kind = 'ask';
  assert.equal(context.ui.otherAllowed(card), false, 'an ask review step has no Other answer either');
  state.drafts.get(card.id).step = 0;
  assert.equal(context.ui.otherAllowed(card), true, 'the question step still offers Other');
});

test('finding answer dispatch forwards answers, image, mode and plan-into effort', async () => {
  const card = { id: 'finding-dispatch', kind: 'finding' },
    answers = [{ option: 0 }, { text: 'Use a safer key' }],
    elements = {
      '#dispatch-start': { disabled: false },
      '#dispatch-hint': { textContent: '' },
      '#dispatch-note': { value: 'Confirm rollout' },
      '#dispatch-model': { value: 'provider/planner' },
      '#dispatch-thinking': { value: 'high' },
      '#dispatch-engine': { value: 'omp' },
      '#dispatch-executor': { value: '@fast' },
      '#dispatch-executor-effort': { value: 'max' },
      '#dispatch': { close() {} },
    },
    draft = { dispatchMode: 'plan-into', dispatchAnswers: answers, dispatchOption: null },
    requests = [],
    context = vm.createContext({
      S: { cards: [card], dispatchId: card.id, drafts: new Map([[card.id, draft]]), pending: null, remoteSwapUntil: 0 },
      $: (selector) => elements[selector],
      draft: () => draft,
      sheetImageBody: async () => ({ media_type: 'image/png', data: 'encoded' }),
      act: async (...args) => { requests.push(args); return true; },
    });
  vm.runInContext(`${functionSource('submitDispatch', true)}\nglobalThis.submitDispatch = submitDispatch;`, context);

  await context.submitDispatch({ preventDefault() {} });
  assert.equal(requests.length, 1);
  assert.equal(requests[0][0], card);
  assert.equal(requests[0][1], 'answer');
  assert.deepEqual(JSON.parse(JSON.stringify(requests[0][2])), {
    answers,
    note: 'Confirm rollout',
    mode: 'plan-into',
    engine: 'omp',
    model: 'provider/planner',
    thinking: 'high',
    image: { media_type: 'image/png', data: 'encoded' },
    executor: '@fast',
    executor_thinking: 'max',
  });
});

test('a finding question with its own star is titled by its label and opens at the top', () => {
  const card = {
      id: 'finding-star',
      kind: 'finding',
      questions: [
        {
          label: 'Rotate the deploy key',
          question: 'Title: Rotate the deploy key\nSituation: The key is 120 days old.',
          star: { situation: 'The key is 120 days old.' },
          options: [{ label: 'Do as recommended', recommended: true }, { label: 'Skip' }],
        },
        { label: 'Supplier', question: 'Delete the supplier?', options: [{ label: 'Keep' }] },
      ],
    },
    state = {
      drafts: new Map([[card.id, { step: 0, answers: [{ option: 0 }, { option: 0 }], other: '', open: false }]]),
    },
    scrolls = [],
    context = vm.createContext({
      S: state,
      draft: (current) => state.drafts.get(current.id),
      ident: (name, current) => `${name}-${current.id}`,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_current, _action, className, props, onClick, ...children) => node('button', { ...props, class: className, onClick }, ...children),
      mdInline: (text) => text,
      snapshot: () => ({ scroll: 240 }),
      render: (view) => scrolls.push(view.scroll),
      focusPrimary() {},
      announce() {},
      sendAnswers() {},
    });
  vm.runInContext(
    `${['stepTitle', 'reviewView', 'goStep'].map((name) => functionSource(name)).join('\n')}\nglobalThis.ui = { stepTitle, reviewView, goStep };`,
    context,
  );
  const title = (step) => {
    state.drafts.get(card.id).step = step;
    return context.ui.stepTitle(card);
  };

  assert.equal(title(0), 'Rotate the deploy key', 'its own star moves the finding text out of the title');
  assert.equal(title(1), 'Delete the supplier?', 'a question without a star stays its own title');
  assert.equal(title(2), 'Review answers');
  assert.deepEqual(
    findNodes(context.ui.reviewView(card), (item) => item.props?.class === 'opt-desc').map((item) => item.children[0]),
    ['Rotate the deploy key', 'Delete the supplier?'],
    'the review names a starred finding by its label',
  );
  for (const step of [0, 1, 2]) context.ui.goStep(card, step);
  assert.deepEqual(scrolls, [0, 240, 240], 'only a question with its own text resets the scroll');
});

test('Other opens when an action is in flight and brings its field into view', () => {
  const card = {
      id: 'finding-other-pending',
      kind: 'finding',
      allow_other: true,
      questions: [{ label: 'q', question: 'Choose', options: [{ label: 'Keep' }] }],
    },
    state = {
      pending: { id: 'another-card', control: 'skip' },
      remoteSwapUntil: Date.now() + 60000,
      drafts: new Map([[card.id, { step: 0, answers: [], other: '', note: '', open: false }]]),
    },
    focused = [],
    scrolled = [],
    context = vm.createContext({
      S: state,
      draft: (current) => state.drafts.get(current.id),
      answerable: () => true,
      ident: (name, current) => `${name}-${current.id}`,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_current, _action, className, props, onClick, ...children) => node('button', { ...props, class: className, onClick }, ...children),
      mdInline: (text) => text,
      render() {},
      $: () => ({
        focus: (...args) => focused.push(args),
        scrollIntoView: (...args) => scrolled.push(args),
      }),
    });
  vm.runInContext(
    `${['otherAllowed', 'otherRow', 'questionView', 'toggleOther'].map((name) => functionSource(name)).join('\n')}\nglobalThis.ui = { toggleOther };`,
    context,
  );
  context.ui.toggleOther(card);
  const draft = state.drafts.get(card.id);
  assert.equal(draft.open, true, 'a click on Other opens the field even while another card acts');
  assert.equal(scrolled.length, 1, 'the field is brought into view');
  assert.equal(scrolled[0][0].block, 'nearest');
  assert.equal(focused[0][0].preventScroll, true);
});

test('the digit after the last option opens Other', () => {
  const card = {
      id: 'finding-other-digit',
      kind: 'finding',
      allow_other: true,
      questions: [{ label: 'q', question: 'Choose', options: [{ label: 'One' }, { label: 'Two' }, { label: 'Skip' }] }],
    },
    state = { pending: null, drafts: new Map([[card.id, { step: 0, answers: [], other: '', note: '', open: false }]]) },
    context = vm.createContext({
      S: state,
      draft: (current) => state.drafts.get(current.id),
      answerable: () => true,
      ident: (name, current) => `${name}-${current.id}`,
      h: node,
      el: (tag, props, ...children) => node(tag, props || {}, ...children),
      button: (_current, _action, className, props, onClick, ...children) => node('button', { ...props, class: className, onClick }, ...children),
      mdInline: (text) => text,
      render() {},
      $: () => ({ focus() {}, scrollIntoView() {} }),
      answerStep: () => assert.fail('the Other digit records no option'),
      goStep: () => assert.fail('the Other digit does not move a question'),
    });
  vm.runInContext(
    `${['otherAllowed', 'otherRow', 'questionView', 'toggleOther', 'pickStep'].map((name) => functionSource(name)).join('\n')}\nglobalThis.ui = { pickStep };`,
    context,
  );
  context.ui.pickStep(card, 3);
  assert.equal(state.drafts.get(card.id).open, true, 'key 4 opens the fourth, Other button');
});

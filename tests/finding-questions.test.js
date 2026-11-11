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

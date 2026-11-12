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

// Loads the real cardView and block helpers, plus any named functions, with the rest of the page stubbed.
function cardContext(state = {}, stubs = {}, functions = ['cardView']) {
  const context = vm.createContext({
    S: { pending: null, shakeId: null, direction: 1, openBlocks: new Set(), ...state },
    h: node,
    el: (tag, props, ...children) => node(tag, props || {}, ...children),
    ident: (name, item) => `${name}-${item.id}`,
    queueLine: () => null,
    titleRow: (item) => [node('h2', { id: 'card-title' }, item.title)],
    facts: () => null,
    optionsView: (item) => (item.options || []).map((option) => node('button', 'option', option.label)),
    noteRow: () => null,
    dock: () => null,
    report: (item) => node('div', 'report-wrap', item.body),
    toggleBlock() {},
    ...stubs,
  });
  vm.runInContext(markedSource, context);
  vm.runInContext(
    `${slice('  const heading =', '  const recIndex =')}${slice('  const prose =', '  function titleRow')}` +
      `${functions.map(functionSource).join('\n')}\nObject.assign(globalThis, { ${functions.join(', ')} });`,
    context,
  );
  return context;
}

// Renders one card and returns its sections keyed by label.
function sections(card) {
  const view = cardContext().cardView(card, false);
  return {
    view,
    byLabel: Object.fromEntries(
      findNodes(view, (item) => item.tag === 'section').map((section) => [textOf(section.children[0]), section]),
    ),
  };
}

test('the first card paints in place, and a card that replaces another view slides in', () => {
  const main = {
      child: null,
      replaceChildren(child) {
        this.child = child;
      },
    },
    ask = (id) => ({ id, kind: 'ask', title: `Question ${id}?`, star: {}, options: [{ label: 'Yes' }] }),
    context = cardContext(
      { cards: [ask('a'), ask('b')], idx: 0, shownId: undefined, initialized: true },
      {
        $: (selector) => (selector === '#main' ? main : null),
        snapshot: () => ({}),
        updateHeader() {},
        emptyView: () => node('section', 'empty'),
        observeReports() {},
        updateClamps() {},
        restore() {},
        announce() {},
      },
      ['cardView', 'render'],
    ),
    slides = () => main.child.props.class.split(' ').includes('enter');

  context.render();
  assert.equal(slides(), false, 'the first paint shows the card in place');
  context.S.idx = 1;
  context.render();
  assert.equal(slides(), true, 'moving to the next card slides it in');
  context.S.cards = [];
  context.S.idx = 0;
  context.render();
  context.S.cards = [ask('c')];
  context.render();
  assert.equal(slides(), true, 'a card arriving in an empty inbox slides in');
});

test('an info card shows its final reply once, under Result, and never as a Plan', () => {
  const body = 'Shipped the parser split and the release build passed.',
    { view, byLabel } = sections({ id: 'info', kind: 'info', title: 'Done: parser split', body, star: {} });

  assert.equal(byLabel.Plan, undefined, 'an info body is not a plan');
  assert.ok(textOf(byLabel.Result).includes(body), 'the reply sits under Result');
  assert.equal(findNodes(view, (item) => item.children.includes(body)).length, 1, 'the reply renders once');
  // A stray images field renders no thumbnails: the daemon no longer sends it.
  const stray = sections({ id: 'info', kind: 'info', title: 'Done: parser split', body, star: {}, images: [{ id: 'a'.repeat(64), media_type: 'image/png' }] }).view;
  assert.equal(findNodes(stray, (item) => item.tag === 'img' || item.props?.class === 'card-images-block').length, 0);
});

test('a session report states its outcome above a title without the status prefix', () => {
  const { titleRow } = cardContext({}, { stepTitle: () => '' }, ['titleRow']),
    head = (card) => {
      const row = titleRow(card),
        status = findNodes(row, (item) => item.props.class === 'report-status')[0];
      return {
        status: status ? [textOf(status), status.props['data-tone']] : null,
        title: textOf(findNodes(row, (item) => item.tag === 'h2')[0]),
      };
    },
    info = (title, body = 'Details follow.') => ({ id: 'report', kind: 'info', title, body, star: {} });

  assert.deepEqual(head(info('Done: Shipped the parser split')), {
    status: ['Complete', 'positive'],
    title: 'Shipped the parser split',
  });
  assert.deepEqual(head(info('Agent done: Push still open')), {
    status: ['Step left open', 'warning'],
    title: 'Push still open',
  });
  assert.deepEqual(head(info('Incomplete: The preview host was down')), {
    status: ['Incomplete', 'negative'],
    title: 'The preview host was down',
  });
  assert.deepEqual(head(info('Unreported: Working on the deploy')), {
    status: ['No final report', 'warning'],
    title: 'Working on the deploy',
  });
  assert.deepEqual(head(info('Plan ready: Keep autorules out of triage')), {
    status: ['Plan ready', undefined],
    title: 'Keep autorules out of triage',
  });
  assert.deepEqual(
    head(info('Plan ready: The plan', '## The plan\n\nSteps follow.')),
    { status: ['Plan ready', undefined], title: 'The plan' },
    'a plan-mode report states the plan is ready, never complete',
  );
  assert.deepEqual(
    head(info('Done: ## Shipped', '## Shipped\n\nThe parser split is live.')),
    { status: ['Complete', 'positive'], title: 'Shipped' },
    'a report that opens with a heading keeps its outcome',
  );
  assert.deepEqual(
    head(info('Nightly backup finished')),
    { status: null, title: 'Nightly backup finished' },
    'a job result carries no session outcome',
  );
  assert.deepEqual(
    head({ id: 'ask', kind: 'ask', title: 'Done: which branch ships?', star: {}, options: [{ label: 'main' }] }),
    { status: null, title: 'Done: which branch ships?' },
    'only an info card reads its title as an outcome',
  );
});

test('a plan card renders its Markdown body under Plan', () => {
  const body = '## Steps\n\n- Move parsing into **small** helpers\n- Keep `parse()` callers',
    { byLabel } = sections({
      id: 'plan',
      kind: 'plan',
      title: 'Plan ready: Split parser helpers',
      body,
      star: {},
      options: [{ label: 'Approve' }, { label: 'Refine' }],
    });

  assert.equal(findNodes(byLabel.Plan, (item) => item.tag === 'li').length, 2, 'two list items render');
  assert.ok(textOf(byLabel.Plan).includes('Move parsing into small helpers'), 'emphasis renders as text');
  assert.ok(!textOf(byLabel.Plan).includes('**'), 'no raw Markdown markers stay');
});

test('an ask whose task repeats its title has no Task block', () => {
  const title = 'Which port should the dev daemon bind?',
    ask = (task) =>
      sections({
        id: 'ask',
        kind: 'ask',
        title,
        star: { situation: 'Two daemons want 7340.', task },
        options: [{ label: '7341' }, { label: '7342' }],
      }).byLabel;

  const repeated = ask(title);
  assert.equal(repeated.Task, undefined, 'the title already asks the question');
  assert.ok(repeated.Situation, 'the situation still shows');
  assert.ok(repeated.Action, 'the options still show');

  const distinct = ask('Pick a port that stays clear of the live daemon.');
  assert.ok(textOf(distinct.Task).includes('Pick a port'), 'a task that adds detail still shows');
});

test('the source name drops the tmux window prefix and leading emoji from a session label', () => {
  const context = vm.createContext({});
  vm.runInContext(`${functionSource('sourceName')}\nObject.assign(globalThis, { sourceName });`, context);
  const { sourceName } = context;

  assert.equal(
    sourceName({
      label: 'random:9 📞 CALLER: Update caller wiki from Rishav and Hendrix email',
      target: 'random:9.0',
    }),
    'CALLER: Update caller wiki from Rishav and Hendrix email',
  );
  assert.equal(sourceName({ label: 'work:3 ☎️ Calls · step 2', target: 'work:3.1' }), 'Calls');
  assert.equal(
    sourceName({ label: 'random:9 Notes', target: 'other:2.0' }),
    'random:9 Notes',
    'a prefix that names another window stays',
  );
  assert.equal(
    sourceName({ label: 'random:9 📞', target: 'random:9.0', cwd: '/home/u/projects/triage/' }),
    'triage',
    'a label with no title falls back to the directory name',
  );
  assert.equal(sourceName({ uri: 'test://smoke' }), 'test://smoke');
  assert.equal(sourceName({}), 'unknown');
});

test('a finding question with its own star shows that finding in place of the card', () => {
  const drafts = new Map(),
    { cardView } = cardContext({ drafts }, {}, ['cardView', 'draft']),
    card = {
      id: 'audit',
      kind: 'finding',
      title: 'Rule audit 2026-11-11, 2 findings',
      star: {
        situation: 'Every finding at once.',
        task: 'Answer every finding on this card.',
        action: 'Read audit.json.',
        result: 'One session does the accepted work.',
      },
      questions: [
        {
          label: 'Rotate the deploy key',
          question: 'Title: Rotate the deploy key\nSituation: The key is 120 days old.',
          options: [{ label: 'Do as recommended' }],
          star: {
            situation: 'The key is 120 days old.',
            task: 'Keep deploys on a fresh key.',
            action: 'Run rotate.sh.',
            result: 'Deploys keep working.',
          },
        },
        { label: 'Supplier', question: 'Delete the supplier?', options: [{ label: 'Keep' }] },
      ],
    },
    shown = () =>
      Object.fromEntries(
        findNodes(cardView(card, false), (item) => item.tag === 'section').map((section) => [
          textOf(section.children[0]),
          textOf(section.children[1]),
        ]),
      );

  const own = shown();
  assert.ok(own.Situation.includes('120 days old'), 'the step shows the finding situation');
  assert.ok(!own.Situation.includes('Every finding at once'), 'the card situation stays off that step');
  assert.ok(own.Task.includes('fresh key'), 'the step shows the finding task');
  assert.ok(own.Result.includes('Deploys keep working'), 'the step shows the finding result');
  assert.ok(own.Action.includes('rotate.sh'), 'the step shows the finding action');

  drafts.get(card.id).step = 1;
  assert.ok(shown().Situation.includes('Every finding at once'), 'a question without a star keeps the card situation');

  drafts.get(card.id).step = 2;
  assert.ok(shown().Action.includes('Read audit.json'), 'the review step shows the card again');
});

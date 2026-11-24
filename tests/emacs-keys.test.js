// The web inbox takes the Emacs client's keys. These tests read the Emacs tables. A key that
// changes in one client and not the other fails here.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const emacs = (name) => fs.readFileSync(path.join(__dirname, '..', 'emacs', name), 'utf8');
const from = (text, start) => {
  const at = text.indexOf(start);
  assert.ok(at >= 0, `ui/app.js holds ${start.trim()}`);
  return at;
};
const slice = (start, end) => {
  const at = from(app, start),
    to = app.indexOf(end, at);
  assert.ok(to > at, `ui/app.js holds ${end.trim()} after ${start.trim()}`);
  return app.slice(at, to);
};

// The page's three key sources are the action table, the choice mnemonic and the sheet options.
const table = slice('  const answerable =', '  const current ='),
  choices = app.match(/^  function choiceKeys\([^\n]*\) \{[\s\S]*?^  \}/m)?.[0],
  sheet = slice('  const SHEET_KEYS =', "  $('#compose').addEventListener('keydown'");
assert.ok(choices, 'app.js defines choiceKeys');

const context = vm.createContext({});
vm.runInContext(
  `${table}\n${choices}\n${sheet}\nObject.assign(globalThis, { ACTIONS, choiceKeys, SHEET_KEYS });`,
  context,
);

test('every card action keeps the key and order the Emacs client gives it', () => {
  const actions = [...emacs('triage-card.el').matchAll(/\(list '([a-z-]+) :key "([^"]+)"/g)].map(
    ([, id, key]) => `${id.replace(/-([a-z])/g, (_, ch) => ch.toUpperCase())} ${key}`,
  );
  assert.ok(actions.length > 0, 'triage-card.el holds triage-card-actions');
  assert.deepEqual(
    Object.entries(context.ACTIONS).map(([id, action]) => `${id} ${action.key}`),
    actions,
    'ui/app.js lists the same actions, in the same order, under the same keys',
  );
});

test('a choice takes the mnemonic key the Emacs client gives it', () => {
  const cases = [
    [['Goal', 'Plan', 'Plan into'], 'g p P'],
    [['Goal', 'Plan into'], 'g p'],
    [['Investigate', 'Fix', 'Act', 'Verify', 'Simplify'], 'i f a v s'],
    [['Default', 'omp', 'dsh', 'ompsdk'], 'd o D O'],
    [
      ['Default (omp default)', 'auto', 'off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'],
      'd a o m l M h x A',
    ],
  ];
  for (const [labels, keys] of cases)
    assert.equal(context.choiceKeys(labels).join(' '), keys, labels.join(' / '));
});

test('the session sheet takes the option keys of the Emacs launch sheet', () => {
  const source = emacs('triage-launch.el'),
    at = source.indexOf('(defvar-keymap triage-launch-options-map');
  assert.ok(at >= 0, 'triage-launch.el defines triage-launch-options-map');
  const block = source.slice(at, source.indexOf('\n\n', at)),
    keys = [...block.matchAll(/^  "([^"]+)" #'/gm)].map(([, key]) => key);
  assert.ok(keys.length > 0, 'triage-launch-options-map binds its options');
  // The web pastes with Ctrl+V, and the handler takes q and Tab itself.
  assert.deepEqual(
    Object.keys(context.SHEET_KEYS),
    keys.filter((key) => !['y', 'q', 'TAB'].includes(key)),
    'ui/app.js binds the launch sheet options under the same keys',
  );
});

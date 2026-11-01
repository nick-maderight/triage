const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8'),
  start = app.indexOf('  // Theme: the inline head script sets data-theme first; the toggle swaps and persists it.'),
  end = app.indexOf("  $('#peek-mode').addEventListener", start);
assert.ok(start >= 0 && end > start);
const themeCode = app.slice(start, end);

test('theme initialization does not save a preference, but an explicit toggle does', () => {
  const writes = [],
    button = {
      addEventListener: (event, handler) => {
        assert.equal(event, 'click');
        button.onClick = handler;
      },
      setAttribute() {},
    },
    meta = {},
    document = {
      documentElement: { dataset: { theme: 'dark' } },
      querySelector: () => meta,
    },
    context = vm.createContext({
      document,
      localStorage: { setItem: (...args) => writes.push(args) },
      $: (selector) => (selector === '#btn-theme' ? button : null),
    });

  vm.runInContext(themeCode, context);

  assert.deepEqual(writes, []);
  assert.equal(document.documentElement.dataset.theme, 'dark');

  button.onClick();

  assert.equal(document.documentElement.dataset.theme, 'light');
  assert.deepEqual(writes, [['triage-theme', 'light']]);
});

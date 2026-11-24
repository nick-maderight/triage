const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const ui = path.join(__dirname, '..', 'ui'),
  app = fs.readFileSync(path.join(ui, 'app.js'), 'utf8'),
  start = app.indexOf('  const icon = (d) =>'),
  end = app.indexOf("  $('#peek-mode').addEventListener", start),
  head = fs.readFileSync(path.join(ui, 'index.html'), 'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];
assert.ok(start >= 0 && end > start);
const themeCode = app.slice(start, end);

function page(dark) {
  const changes = [],
    writes = [],
    meta = { content: '#f2f4f7' },
    button = {
      addEventListener: (event, handler) => {
        assert.equal(event, 'click');
        button.click = handler;
      },
      setAttribute() {},
    },
    matchMedia = (query) => ({
      matches: query === '(prefers-color-scheme: dark)' && dark,
      addEventListener: (event, handler) => {
        if (query === '(prefers-color-scheme: dark)' && event === 'change') changes.push(handler);
      },
    }),
    document = { documentElement: { dataset: {} }, querySelector: () => meta },
    context = vm.createContext({
      document,
      matchMedia,
      window: { matchMedia },
      localStorage: {
        getItem: () => (dark ? 'light' : 'dark'),
        setItem: (...args) => writes.push(args),
      },
      $: (selector) => (selector === '#btn-theme' ? button : null),
    });
  return {
    context,
    button,
    writes,
    theme: () => [document.documentElement.dataset.theme, meta.content],
    system: (next) => {
      dark = next;
      for (const handler of changes) handler({ matches: next });
    },
  };
}

test('system appearance sets the theme on every load', () => {
  for (const dark of [true, false]) {
    const p = page(dark);
    vm.runInContext(head, p.context);
    assert.deepEqual(p.theme(), dark ? ['dark', '#0a0a0a'] : ['light', '#f2f4f7']);
  }
});

test('the open inbox follows system changes after a temporary toggle', () => {
  const p = page(false);
  vm.runInContext(head, p.context);
  vm.runInContext(themeCode, p.context);
  p.button.click();
  assert.deepEqual(p.writes, []);
  p.system(true);
  p.system(false);
  assert.deepEqual(p.theme(), ['light', '#f2f4f7']);
  p.system(true);
  assert.deepEqual(p.theme(), ['dark', '#0a0a0a']);
});

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
const listenerSource = (type) => {
  const start = app.indexOf(`  document.addEventListener('${type}', (e) => {`),
    end = app.indexOf('\n  });', start);
  assert.ok(start >= 0 && end > start, `app.js defines the document ${type} handler`);
  return app.slice(start, end + '\n  });'.length);
};

function node(parent, props = {}) {
  const n = {
    parent,
    selectors: [],
    ...props,
    contains(other) {
      for (let x = other; x; x = x.parent) if (x === n) return true;
      return false;
    },
    closest(selectors) {
      for (let x = n; x; x = x.parent)
        if (selectors.split(',').some((s) => x.selectors.includes(s.trim()))) return x;
      return null;
    },
  };
  return n;
}

// Runs the real document click and focusin handlers and closeLater against a card that
// render() rebuilds, the way the page does.
function harness() {
  const listeners = {},
    state = { laterOpen: false, countsExpanded: false },
    page = { state, chosen: [] },
    root = node(null),
    main = node(root);
  let active = null;
  const connected = (n) => {
    while (n.parent) n = n.parent;
    return n === root;
  };
  function build() {
    if (page.card) page.card.parent = null;
    const card = node(main, { selectors: ['article', '.card'], tabindex: '-1' }),
      dock = node(card, { selectors: ['footer', '.dock'] }),
      later = node(dock, { selectors: ['.later'] });
    Object.assign(page, {
      card,
      situation: node(card, { selectors: ['p'] }),
      skip: node(dock, { selectors: ['button'] }),
      menu: null,
      choice: null,
    });
    if (!state.laterOpen) return;
    page.menu = node(later, { selectors: ['#later-menu', '.menu'] });
    page.choice = node(page.menu, {
      selectors: ['#later-0', 'button'],
      onclick() {
        page.chosen.push('1 hour');
        state.laterOpen = false;
      },
    });
  }
  const focus = (n) => {
    if (n === active) return;
    active = n;
    listeners.focusin({ target: n });
  };
  const context = vm.createContext({
    S: state,
    HTMLDialogElement: class {},
    $: (selector) => (selector === '#later-menu' && page.menu && connected(page.menu) ? page.menu : null),
    render: build,
    closeCounts() {},
    document: {
      addEventListener(type, listener) {
        listeners[type] = listener;
      },
    },
  });
  vm.runInContext([functionSource('closeLater'), listenerSource('click'), listenerSource('focusin')].join('\n'), context);
  page.open = () => {
    state.laterOpen = true;
    build();
    focus(page.choice);
  };
  page.focus = focus;
  // Safari on macOS and iOS does not focus a pressed button without tabindex. Mousedown focuses
  // the nearest focusable ancestor instead, and a press whose button is gone by mouseup clicks nothing.
  page.press = (target) => {
    let focusable = target;
    while (focusable && focusable.tabindex == null) focusable = focusable.parent;
    if (focusable) focus(focusable);
    if (!connected(target)) return false;
    target.closest('button')?.onclick?.();
    listeners.click({ target });
    return true;
  };
  return page;
}

test('a Safari press on a Later choice reaches the choice', () => {
  const page = harness();
  page.open();
  assert.equal(page.press(page.choice), true, 'the click reaches the pressed choice');
  assert.deepEqual(page.chosen, ['1 hour']);
});

test('the Later menu closes when focus moves to another control or a press lands outside it', () => {
  const page = harness();
  page.open();
  page.focus(page.skip);
  assert.equal(page.state.laterOpen, false, 'Tab to another control closes the menu');
  page.open();
  page.press(page.situation);
  assert.equal(page.state.laterOpen, false, 'a press on the card text closes the menu');
  assert.deepEqual(page.chosen, []);
});

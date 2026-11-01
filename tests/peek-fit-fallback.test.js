const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const start = app.indexOf('  function peekAdvanceRatio(pre) {');
const end = app.indexOf('  function resizePeek() {', start);
assert.ok(start >= 0 && end > start);
const peekFunctions = app.slice(start, end);

test('Fit falls back to Readable when font would drop below 4px', () => {
  const state = {
      peekMode: 'fit',
      peekWidth: 1000,
      peekHeight: 40,
      peekMonoRatio: 0.6,
      peekFollowBottom: false,
    },
    columns = '─'.repeat(1000),
    pre = {
      clientWidth: 600,
      clientHeight: 300,
      scrollHeight: 900,
      scrollTop: 20,
      style: {},
      firstElementChild: {
        classList: { contains: (name) => name === 'peek-content' },
        style: {},
      },
      get scrollWidth() {
        return Math.ceil(columns.length * Number.parseFloat(this.style.fontSize) * state.peekMonoRatio);
      },
    };
  let modeButtonUpdates = 0;
  const context = vm.createContext({
    S: state,
    getComputedStyle: () => ({ paddingLeft: '0px', paddingRight: '0px' }),
    updatePeekModeButton: () => modeButtonUpdates++,
  });
  vm.runInContext(
    `${peekFunctions}\nObject.assign(globalThis, { peekFitSize, applyPeekLayout });`,
    context,
  );

  context.applyPeekLayout(pre, false, pre.scrollTop);

  assert.equal(state.peekMode, 'readable', 'Fit falls back to Readable below 4px');
  assert.equal(pre.style.fontSize, '12px', 'Readable fallback uses 12px text');
  assert.equal(modeButtonUpdates, 1);
  assert.ok(pre.scrollWidth > pre.clientWidth, 'wide Readable text can scroll horizontally');
});

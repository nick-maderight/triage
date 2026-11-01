const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const start = app.indexOf('  function historyDayStart() {');
const end = app.indexOf('  // render', start);
assert.ok(start >= 0 && end > start);
const historyFunctions = app.slice(start, end);

const fixedNow = new Date(2026, 0, 15, 12, 30).getTime();
class FixedDate extends Date {
  constructor(...args) {
    super(...(args.length ? args : [fixedNow]));
  }
}

test('history summary uses today_count beyond page limit', async () => {
  const state = {
      cards: [],
      historyPromise: null,
      historyPromiseDate: null,
      history: { loading: false, loaded: false, dayStart: null, error: null, today: 0, lastAt: null },
    },
    cards = Array.from({ length: 50 }, () => ({ swipe: { at_ms: null } }));
  let requestUrl;
  const context = vm.createContext({
    S: state,
    Date: FixedDate,
    URLSearchParams,
    api: async (url) => {
      requestUrl = url;
      return { cards, today_count: 51 };
    },
    stamp: (value) => (value == null || value === '' ? null : Number(value)),
    render() {},
  });

  vm.runInContext(
    `${historyFunctions}\nObject.assign(globalThis, { historyRows, emptyHistory });`,
    context,
  );
  await context.emptyHistory();

  const localStart = new Date(fixedNow);
  localStart.setHours(0, 0, 0, 0);
  const localEnd = new Date(localStart);
  localEnd.setDate(localEnd.getDate() + 1);
  const params = new URL(requestUrl, 'http://triage.test').searchParams;

  assert.equal(state.history.today, 51, 'Handled today uses server today_count');
  assert.equal(state.history.loaded, true);
  assert.deepEqual([...params.keys()].sort(), ['limit', 'today_end_ms', 'today_start_ms']);
  assert.equal(Number(params.get('today_start_ms')), localStart.getTime());
  assert.equal(Number(params.get('today_end_ms')), localEnd.getTime());
});

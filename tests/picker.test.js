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
const context = vm.createContext({});
vm.runInContext(
  `${functionSource('pickerOption')}\n${functionSource('pickerRows')}\nObject.assign(globalThis, { pickerOption, pickerRows });`,
  context,
);
const { pickerOption, pickerRows } = context;
const plain = (value) => JSON.parse(JSON.stringify(value));
const values = (result) => plain(result.rows).map((row) => row.value);

const models = {
    free: true,
    fallback: 'GLM-5.3 (Global)',
    options: [
      pickerOption(
        'amazon-bedrock/anthropic.claude-opus-5-5',
        'Claude Opus 5.5',
        'amazon-bedrock/anthropic.claude-opus-5-5',
      ),
      pickerOption(
        'amazon-bedrock/global.zai.glm-5.3',
        'GLM-5.3 (Global)',
        'amazon-bedrock/global.zai.glm-5.3',
      ),
      pickerOption(
        'bedrock-mantle/openai.gpt-6-luna',
        'GPT-6 Luna (272K)',
        'bedrock-mantle/openai.gpt-6-luna',
      ),
    ],
  },
  cwds = {
    free: true,
    fallback: 'Home directory',
    options: [pickerOption('/home/me/tmp/x'), pickerOption('/home/me/tmp')],
  },
  thinking = {
    free: false,
    fallback: 'max',
    options: ['low', 'high', 'xhigh', 'max'].map((level) => pickerOption(level)),
  };

test('an empty query lists Default first, then every option, with the current value active', () => {
  const result = pickerRows(thinking, '', 'xhigh');
  assert.deepEqual(values(result), ['', 'low', 'high', 'xhigh', 'max']);
  assert.equal(plain(result.rows)[0].hint, 'max');
  assert.equal(result.active, 3);
  assert.equal(pickerRows(thinking, '', '').active, 0);
});

test('every query word must appear in the name or the selector', () => {
  const opus = pickerRows(models, 'opus 5.5', '');
  assert.deepEqual(values(opus), ['opus 5.5', 'amazon-bedrock/anthropic.claude-opus-5-5']);
  assert.equal(opus.active, 1);
  const mantle = pickerRows(models, 'MANTLE luna', '');
  assert.deepEqual(values(mantle), ['MANTLE luna', 'bedrock-mantle/openai.gpt-6-luna']);
  assert.equal(mantle.active, 1);
});

test('a free field offers typed text, but Enter keeps a match first', () => {
  assert.deepEqual(plain(pickerRows(cwds, '/srv/new', '/home/me/tmp')), {
    rows: [{ value: '/srv/new', label: 'Use “/srv/new”', hint: null }],
    active: 0,
  });
  const existing = pickerRows(cwds, '/home/me/tmp', '');
  assert.deepEqual(values(existing), ['/home/me/tmp/x', '/home/me/tmp']);
  assert.equal(existing.active, 1);
});

test('a strict field never offers typed text', () => {
  assert.deepEqual(plain(pickerRows(thinking, 'turbo', '')), { rows: [], active: -1 });
  const high = pickerRows(thinking, 'HIGH', '');
  assert.deepEqual(values(high), ['high', 'xhigh']);
  assert.equal(high.active, 0);
});

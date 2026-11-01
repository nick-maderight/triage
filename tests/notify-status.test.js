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
vm.runInContext(`${functionSource('notifyStatus')}\nglobalThis.notifyStatus = notifyStatus;`, context);

test('the sheet only offers the switch when this device can really subscribe', () => {
  const cases = [
    [{ supported: false, ios: true }, { toggle: false }, /Home Screen/],
    [{ supported: false, ios: false }, { toggle: false }, /HTTPS/],
    [{ supported: true, permission: 'denied', loaded: true, level: 2 }, { on: false, toggle: false }],
    [{ supported: true, permission: 'default', loaded: false, level: null }, { toggle: false }],
    [
      { supported: true, permission: 'granted', loaded: false, error: 'the service worker did not start', level: null },
      { toggle: false },
      /unavailable/,
    ],
    // Not asked yet: Turn on is the tap that asks for permission.
    [{ supported: true, permission: 'default', loaded: true, level: null }, { on: false, toggle: true }],
    [{ supported: true, permission: 'granted', loaded: true, level: null }, { on: false, toggle: true }],
    [{ supported: true, permission: 'granted', loaded: true, level: 1 }, { on: true, toggle: true }],
  ];
  for (const [env, expected, text] of cases) {
    const status = context.notifyStatus(env);
    for (const [field, value] of Object.entries(expected))
      assert.equal(status[field], value, `${field} for ${JSON.stringify(env)}`);
    if (text) assert.match(status.text, text, JSON.stringify(env));
  }
});

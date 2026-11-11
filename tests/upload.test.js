const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const functionSource = (name, async = false) => {
  const source = app.match(
    new RegExp(`^  ${async ? 'async ' : ''}function ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'),
  )?.[0];
  assert.ok(source, `app.js defines ${name}`);
  return source;
};

function compose(requests, image) {
  const prompt = { value: 'Describe this.' },
    start = { disabled: false },
    hint = { textContent: '' },
    close = () => {},
    elements = {
      '#compose-prompt': prompt,
      '#compose-cwd': { value: '' },
      '#compose-session': { value: '' },
      '#compose-engine': { value: 'omp' },
      '#compose-model': { value: '' },
      '#compose-thinking': { value: '' },
      '#compose-start': start,
      '#compose-hint': hint,
      '#compose': { close },
    },
    context = vm.createContext({
      S: { composeMode: 'goal' },
      $: (selector) => elements[selector] || null,
      fetch: async (url, init) => {
        requests.push({ url, init });
        return { ok: true, json: async () => ({ pane: '%7' }) };
      },
      announce() {},
      toast() {},
      sheetImageBody: image,
    });
  vm.runInContext(
    `${functionSource('api', true)}\n${functionSource('submitCompose', true)}\nglobalThis.submitCompose = submitCompose;`,
    context,
  );
  return { context, hint, start };
}

test('an attached image rides the New session request', async () => {
  const requests = [],
    image = { media_type: 'image/png', data: 'iVBORw0KGgo=', name: 'shot.png' },
    { context } = compose(requests, async () => image);
  await context.submitCompose({ preventDefault() {} });
  assert.equal(requests.length, 1);
  const body = JSON.parse(requests[0].init.body);
  assert.deepEqual(body.image, image);
  assert.equal(body.prompt, 'Describe this.');
});

test('a session without an image sends no image field', async () => {
  const requests = [],
    { context } = compose(requests, async () => null);
  await context.submitCompose({ preventDefault() {} });
  assert.equal(requests.length, 1);
  assert.equal('image' in JSON.parse(requests[0].init.body), false);
});

test('an unreadable image stops the request with the reason', async () => {
  const requests = [],
    { context, hint, start } = compose(requests, async () => {
      throw new Error('Could not read the image.');
    });
  await context.submitCompose({ preventDefault() {} });
  assert.equal(requests.length, 0);
  assert.equal(hint.textContent, 'Could not read the image.');
  assert.equal(start.disabled, false);
});

test('a second submit while the image is read starts one session', async () => {
  const requests = [];
  let release;
  const reading = new Promise((resolve) => {
    release = () => resolve({ media_type: 'image/png', data: 'iVBORw0KGgo=', name: 'shot.png' });
  });
  const { context } = compose(requests, () => reading);
  const first = context.submitCompose({ preventDefault() {} }),
    second = context.submitCompose({ preventDefault() {} });
  release();
  await Promise.all([first, second]);
  assert.equal(requests.length, 1);
});

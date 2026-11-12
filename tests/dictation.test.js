const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const functionSource = (name, async = false) => {
  const declaration = async ? 'async function' : 'function',
    source = app.match(new RegExp(`^  ${declaration} ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'))?.[0];
  assert.ok(source, `app.js defines ${name}`);
  return source;
};
const json = (value) => JSON.stringify(value);

class Event {
  constructor(type, init = {}) {
    Object.assign(this, { type }, init);
  }
}

const field = (value, caret = value.length, dialog = null) => ({
  value,
  selectionStart: caret,
  selectionEnd: caret,
  disabled: false,
  events: [],
  focused: false,
  closest: (selector) => (selector === 'dialog' ? dialog : null),
  dispatchEvent(event) {
    this.events.push([event.type, event.bubbles]);
  },
  focus() {
    this.focused = true;
  },
});

// A browser speech engine that answers available() with the given status and records every call.
const fakeSpeech = (status) => {
  const made = [],
    calls = [];
  class Recognition {
    static available(options) {
      calls.push(['available', options]);
      return Promise.resolve(status);
    }
    static install(options) {
      calls.push(['install', options]);
      return Promise.resolve(true);
    }
    constructor() {
      made.push(this);
    }
    start() {
      this.started = true;
    }
    stop() {
      this.stopped = true;
      this.onend();
    }
    abort() {
      this.aborted = true;
      this.onend();
    }
  }
  Recognition.prototype.processLocally = false;
  return { Recognition, made, calls };
};
const result = (transcript, isFinal) => Object.assign([{ transcript }], { isFinal });

const dictationContext = (window, note = field('check')) => {
  const toasts = [],
    context = vm.createContext({
      Event,
      S: { dictating: null, dictionary: [] },
      $: (selector) => ({ '#note': note })[selector] ?? null,
      document: { querySelectorAll: () => [] },
      current: () => null,
      announce: () => {},
      checkSpeech: () => {},
      toast: (message, error) => {
        toasts.push([message, error]);
        return { remove() {} };
      },
      navigator: { language: 'en-GB' },
      window: { isSecureContext: true, ...window },
    });
  vm.runInContext(
    [
      ...[
        'localSpeech',
        'speechOptions',
        'paintMic',
        'paintMics',
        'endDictation',
        'placeDictation',
        'insertText',
        'parseDictionary',
        'applyDictionary',
      ].map((name) => functionSource(name)),
      ...['toggleDictation', 'installSpeech'].map((name) => functionSource(name, true)),
    ].join('\n'),
    context,
  );
  return { context, note, toasts };
};
const onDevice = { langs: ['en-GB'], processLocally: true };

test('dictated words join the text at the caret with one space between words', () => {
  const { context } = dictationContext({});
  const end = field('Fix the build');
  context.insertText(end, 'then rerun the tests');
  assert.equal(end.value, 'Fix the build then rerun the tests');
  assert.equal(end.selectionStart, end.value.length);
  assert.equal(json(end.events), json([['input', true]]));
  assert.ok(end.focused);
  const middle = field('Fix the tests', 8);
  context.insertText(middle, 'failing');
  assert.equal(middle.value, 'Fix the failing tests');
  assert.equal(middle.selectionStart, 16);
});

test('words for a field on another card or in a closed sheet go to an error toast', () => {
  const toasts = [],
    note = field(''),
    prompt = field('', 0, { open: false }),
    context = vm.createContext({
      Event,
      $: (selector) => ({ '#note': note, '#compose-prompt': prompt })[selector] ?? null,
      current: () => ({ id: 'card-b' }),
      toast: (message, error) => toasts.push([message, error]),
    });
  vm.runInContext(`${functionSource('insertText')}\n${functionSource('placeDictation')}`, context);
  assert.equal(context.placeDictation({ id: 'note', card: 'card-a' }, 'rotate the key'), false);
  assert.equal(note.value, '');
  assert.ok(toasts[0][0].includes('rotate the key') && toasts[0][1] === true, 'the words reach an error toast');
  assert.equal(context.placeDictation({ id: 'compose-prompt', card: null }, 'ship it'), false);
  assert.equal(prompt.value, '');
  assert.equal(toasts.length, 2);
  assert.equal(context.placeDictation({ id: 'note', card: 'card-b' }, 'rotate the key'), true);
  assert.equal(note.value, 'rotate the key');
  assert.equal(toasts.length, 2);
});

test('the dictionary swaps whole heard phrases, longest first, in one pass', () => {
  const { context } = dictationContext({}),
    entries = context.parseDictionary('red mine = Redmine\nred mine ticket = Redmine issue\n\nissue = bug\nforge jo = Forgejo\n');
  for (const [heard, written] of [
    ['Update the red mine ticket.', 'Update the Redmine issue.'],
    ['Red Mine is down, check Forge Jo.', 'Redmine is down, check Forgejo.'],
    ['A forge jog, bred mine and red miner stay.', 'A forge jog, bred mine and red miner stay.'],
  ])
    assert.equal(context.applyDictionary(heard, entries), written);
  for (const [text, line] of [
    ['red mine = Redmine\nforge jo', 2],
    [' = Forgejo', 1],
    ['forge jo =', 1],
  ])
    assert.throws(() => context.parseDictionary(text), new RegExp(`dictionary line ${line} as`));
});

test('the microphone listens only through on-device recognition and types each final phrase', async () => {
  const { Recognition, made, calls } = fakeSpeech('available'),
    { context, note, toasts } = dictationContext({ SpeechRecognition: Recognition });
  context.S.dictionary = context.parseDictionary('forge jo = Forgejo');
  await context.toggleDictation('note');
  assert.equal(json(calls), json([['available', onDevice]]));
  assert.equal(made.length, 1);
  const [recognition] = made;
  assert.deepEqual(
    {
      lang: recognition.lang,
      processLocally: recognition.processLocally,
      continuous: recognition.continuous,
      interimResults: recognition.interimResults,
      started: recognition.started,
    },
    { lang: 'en-GB', processLocally: true, continuous: true, interimResults: false, started: true },
  );
  assert.equal(context.S.dictating.phase, 'listening');
  const first = result('then push to forge jo', true);
  recognition.onresult({ resultIndex: 0, results: [first] });
  recognition.onresult({ resultIndex: 1, results: [first, result('and tag it', true)] });
  recognition.onresult({ resultIndex: 2, results: [first, result('and tag it', true), result('and tag', false)] });
  assert.equal(note.value, 'check then push to Forgejo and tag it');
  await context.toggleDictation('note');
  assert.ok(recognition.stopped);
  assert.equal(context.S.dictating, null);
  assert.equal(toasts.length, 0);
});

test('a press before the speech model is set up installs it and does not listen', async () => {
  const { Recognition, made, calls } = fakeSpeech('downloadable'),
    { context, toasts } = dictationContext({ SpeechRecognition: Recognition });
  await context.toggleDictation('note');
  assert.equal(made.length, 0);
  assert.equal(json(calls), json([['available', onDevice], ['install', onDevice]]));
  assert.equal(context.S.dictating, null);
  assert.equal(toasts.at(-1)[1], false);
});

test('a browser that cannot require on-device recognition never starts listening', async () => {
  const local = fakeSpeech('available'),
    remoteOnly = fakeSpeech('available');
  delete remoteOnly.Recognition.prototype.processLocally;
  for (const window of [
    { webkitSpeechRecognition: local.Recognition },
    { SpeechRecognition: remoteOnly.Recognition },
    { SpeechRecognition: local.Recognition, isSecureContext: false },
  ]) {
    const { context, note } = dictationContext(window);
    await context.toggleDictation('note');
    assert.equal(context.S.dictating, null);
    assert.equal(note.value, 'check');
  }
  assert.equal(local.made.length + remoteOnly.made.length, 0);
});

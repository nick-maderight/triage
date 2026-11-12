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
const constSource = (name) => {
  const source = app.match(new RegExp(`^  const ${name} = (?:\\{[\\s\\S]*?^  \\};|\\[[^\\n]*\\];|[^\\n]*;)$`, 'm'))?.[0];
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
// What Safari and Chromium hand back: `SpeechRecognitionResult` and its list are array-like, never iterable.
const list = (...results) => Object.assign({ length: results.length }, results);
const final = (...transcripts) =>
  list(...transcripts.map((transcript) => ({ 0: { transcript }, length: 1, isFinal: true })));

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
      ...['DICTATION_STATUS', 'DICTATION_READY', 'DICTATION_ERRORS', 'DICTATION_PAUSES'].map(constSource),
      ...[
        'speechApi',
        'speechEngine',
        'speechLang',
        'speechOptions',
        'paintMic',
        'paintMics',
        'endDictation',
        'startDictation',
        'readResults',
        'afterDictation',
        'stopDictation',
        'finishDictation',
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

// Safari's engine: webkitSpeechRecognition with no available(), install() or processLocally. It ends a run at the
// first pause and calls stop() itself, so a stub that keeps the run alive is not a Safari stub.
const fakeSafari = () => {
  const made = [];
  class Recognition {
    constructor() {
      made.push(this);
    }
    start() {
      if (this.running) throw new Error('The recognition has already started.');
      this.started = (this.started ?? 0) + 1;
      this.running = true;
    }
    stop() {
      this.stopped = true;
      this.end();
    }
    abort() {
      this.aborted = true;
      this.end();
    }
    end() {
      this.running = false;
      this.onend();
    }
  }
  return { Recognition, made };
};

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

test('the dictionary builds no lookbehind, which Safari before 16.4 rejects', () => {
  const { context } = dictationContext({});
  assert.ok(!app.includes('(?<!'), 'app.js has no lookbehind');
  assert.equal(context.applyDictionary('ship red mine now', context.parseDictionary('red mine = Redmine')), 'ship Redmine now');
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
  assert.equal(note.value, 'check then push to Forgejo and tag it', 'iterable lists work');
  note.value = 'check';
  note.selectionStart = note.selectionEnd = 5;
  recognition.onresult({ resultIndex: 0, results: final('then push to forge jo') });
  recognition.onresult({ resultIndex: 1, results: final('then push to forge jo', 'and tag it') });
  assert.equal(note.value, 'check then push to Forgejo and tag it', 'the browsers array-like lists work too');
  assert.equal(toasts.length, 0, 'no result read fails');
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
  const remoteOnly = fakeSpeech('available');
  delete remoteOnly.Recognition.prototype.processLocally;
  for (const window of [
    { SpeechRecognition: remoteOnly.Recognition, isSecureContext: false },
    { webkitSpeechRecognition: remoteOnly.Recognition, isSecureContext: false },
    {},
    { webkitSpeechRecognition: 'SpeechRecognition' },
  ]) {
    const { context, note } = dictationContext(window);
    await context.toggleDictation('note');
    assert.equal(context.S.dictating, null);
    assert.equal(note.value, 'check');
  }
  assert.equal(remoteOnly.made.length, 0);
});

test('Safari starts the microphone inside the press and types the words it hears', async () => {
  const { Recognition, made } = fakeSafari(),
    { context, note, toasts } = dictationContext({ webkitSpeechRecognition: Recognition });
  context.S.dictionary = context.parseDictionary('forge jo = Forgejo');
  const pressed = context.toggleDictation('note');
  // Safari starts recognition only inside the press, so the engine starts before the promise settles.
  assert.equal(made.length, 1, 'the recognition starts without waiting for another turn');
  const [recognition] = made;
  assert.deepEqual(
    {
      lang: recognition.lang,
      processLocally: recognition.processLocally,
      continuous: recognition.continuous,
      interimResults: recognition.interimResults,
      started: recognition.started,
    },
    { lang: 'en-GB', processLocally: undefined, continuous: false, interimResults: false, started: 1 },
  );
  await pressed;
  assert.equal(context.S.dictating.phase, 'listening');
  recognition.onresult({ resultIndex: 0, results: final('then push to forge jo') });
  assert.equal(note.value, 'check then push to Forgejo');
  // Safari ends the run at the first pause, and the run starts again so one press collects a sentence.
  recognition.end();
  assert.equal(made.length, 2, 'the run listens again after Safari ends it');
  assert.equal(made[1].started, 1);
  assert.equal(context.S.dictating.phase, 'listening');
  made[1].onresult({ resultIndex: 0, results: final('and tag it') });
  assert.equal(note.value, 'check then push to Forgejo and tag it');
  await context.toggleDictation('note');
  assert.ok(made[1].stopped, 'a second press stops the run');
  assert.equal(context.S.dictating, null);
  assert.equal(made.length, 2, 'a pressed stop does not listen again');
  assert.equal(toasts.length, 0);
});

test('Safari shows the microphones and says its speech service hears the words', async () => {
  const { Recognition } = fakeSafari(),
    status = { textContent: '' };
  // Safari before 15.4 has no Object.hasOwn, so the sheet must read its status without one.
  const context = vm.createContext({
      window: { isSecureContext: true, webkitSpeechRecognition: Recognition },
      navigator: { language: 'en-GB' },
      document: { documentElement: { dataset: {} } },
      $: (selector) => (selector === '#dictation-status' ? status : null),
    });
  vm.runInContext('Object.hasOwn = undefined;', context);
  vm.runInContext(
    [
      ...['DICTATION_STATUS', 'DICTATION_READY'].map(constSource),
      ...['speechApi', 'speechEngine', 'speechOptions'].map((name) => functionSource(name)),
      functionSource('checkSpeech', true),
    ].join('\n'),
    context,
  );
  await context.checkSpeech();
  assert.equal(context.document.documentElement.dataset.dictation, 'on');
  assert.match(status.textContent, /speech service/);
  assert.match(status.textContent, /leave this device/);
});

test('a result list the browser cannot read ends the run with its message', async () => {
  const { Recognition, made } = fakeSafari(),
    { context, note, toasts } = dictationContext({ webkitSpeechRecognition: Recognition });
  await context.toggleDictation('note');
  made[0].onresult({ resultIndex: 0, results: [] });
  assert.equal(note.value, 'check', 'an empty list adds nothing');
  assert.equal(context.S.dictating.phase, 'listening', 'an empty list leaves the run listening');
  made[0].onresult({ resultIndex: 0, get results() { throw new TypeError('the results threw'); } });
  assert.equal(context.S.dictating, null, 'an unreadable list ends the run instead of listening on');
  assert.deepEqual(toasts, [['Dictation stopped (the results threw).', true]], 'it says what the browser did');
});

test('a Safari run that heard nothing lets go at the pause', async () => {
  const { Recognition, made } = fakeSafari(),
    { context, toasts } = dictationContext({ webkitSpeechRecognition: Recognition });
  await context.toggleDictation('note');
  made[0].onerror({ error: 'no-speech' });
  made[0].end();
  assert.equal(made.length, 1, 'a quiet room does not keep the microphone open');
  assert.equal(context.S.dictating, null);
  assert.deepEqual(toasts, [['The microphone heard no speech.', true]], 'it says the microphone heard nothing');
});

test('a Safari run that hits an error says what happened and keeps the words', async () => {
  const { Recognition, made } = fakeSafari(),
    { context, note, toasts } = dictationContext({ webkitSpeechRecognition: Recognition });
  await context.toggleDictation('note');
  made[0].onerror({ error: 'not-allowed' });
  made[0].end();
  assert.equal(context.S.dictating, null);
  assert.equal(made.length, 1, 'a failed run does not listen again');
  assert.equal(note.value, 'check');
  assert.deepEqual(toasts, [['Please allow the microphone for this site to dictate.', true]]);
});

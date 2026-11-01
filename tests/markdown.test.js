const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');
const markedSource = fs.readFileSync(path.join(__dirname, '..', 'ui', 'marked.js'), 'utf8');
const functionSource = (name) => {
  const source = app.match(new RegExp(`^  function ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^  \\}`, 'm'))?.[0];
  assert.ok(source, `app.js defines ${name}`);
  return source;
};

function node(tag, props = {}, ...children) {
  return {
    tag,
    props: typeof props === 'string' ? { class: props } : props,
    children: children.flat(Infinity).filter((child) => child != null && child !== false && child !== ''),
  };
}

function findNodes(value, predicate, found = []) {
  if (Array.isArray(value)) {
    for (const child of value) findNodes(child, predicate, found);
  } else if (value && typeof value === 'object') {
    if (predicate(value)) found.push(value);
    for (const child of value.children || []) findNodes(child, predicate, found);
  }
  return found;
}

const textOf = (value) =>
  Array.isArray(value)
    ? value.map(textOf).join('')
    : typeof value === 'string'
      ? value
      : (value?.children || []).map(textOf).join('');

const context = vm.createContext({
  h: node,
  el: (tag, props, ...children) => node(tag, props || {}, ...children),
});
vm.runInContext(markedSource, context);
vm.runInContext(
  `${['md', 'mdInline', 'mdNodes'].map(functionSource).join('\n')}\nObject.assign(globalThis, { md, mdInline })`,
  context,
);
const { md, mdInline } = context;
const tags = (nodes, tag) => findNodes(nodes, (item) => item.tag === tag);

test('agent Markdown renders as headings, lists, tables, quotes and code', () => {
  const src = [
    '# Plan',
    '## Steps',
    '1. Parse **input** with `marked`',
    '2. Render',
    '   - nested *item*',
    '',
    '| Name | Count |',
    '|:-----|------:|',
    '| a    | 1     |',
    '',
    '> quoted',
    '',
    '```sh',
    'cargo test && echo ok',
    '```',
    '',
    'line one',
    'line two',
  ].join('\n');
  const out = md(src);

  assert.deepEqual(
    tags(out, 'h4').map((h) => [h.props.class, textOf(h.children)]),
    [['md-h1', 'Plan'], ['md-h2', 'Steps']],
    'every heading is an h4 with a size class',
  );
  const ol = tags(out, 'ol')[0];
  assert.equal(tags(ol, 'li').length, 3, 'the ordered list holds two items plus the nested one');
  assert.equal(tags(ol, 'strong')[0].children[0], 'input');
  assert.equal(tags(ol, 'code')[0].children[0], 'marked');
  assert.equal(textOf(tags(tags(ol, 'ul')[0], 'em')), 'item', 'the nested list keeps its emphasis');
  assert.deepEqual(
    tags(out, 'th').map((th) => textOf(th.children)),
    ['Name', 'Count'],
  );
  const lastCell = tags(out, 'td').at(-1);
  assert.equal(lastCell.props.style, 'text-align: right', 'the column alignment comes from the cell style');
  assert.equal(textOf(tags(out, 'blockquote')), 'quoted');
  const code = tags(out, 'pre')[0];
  assert.equal(code.tag, 'pre');
  assert.equal(tags(out, 'pre')[0].children[0].children[0], 'cargo test && echo ok');
  const lastP = out.filter(Boolean).at(-1);
  assert.equal(lastP.tag, 'p', 'the trailing paragraph renders');
  assert.deepEqual(
    lastP.children.map((child) => (typeof child === 'string' ? child : child.tag)),
    ['line one', 'br', 'line two'],
    'a single newline stays a line break',
  );
});

test('raw HTML stays literal text and only web and mail links become anchors', () => {
  const out = md(
    '<script>alert(1)</script>\n\nClick <img src=x onerror=alert(1)> [here](javascript:alert(1)), [docs](https://example.com/docs),\nmail someone@example.com or see ![shot](https://example.com/shot.png).',
  );

  assert.equal(tags(out, 'script').length, 0, 'no script node is built');
  assert.equal(tags(out, 'img').length, 0, 'no img node is built');
  const text = textOf(out);
  assert.ok(text.includes('<script>alert(1)</script>'), 'block HTML shows as literal text');
  assert.ok(text.includes('<img src=x onerror=alert(1)>'), 'inline HTML shows as literal text');
  assert.ok(text.includes('here'), 'the link text survives');
  assert.deepEqual(
    tags(out, 'a').map((a) => [a.props.href, textOf(a.children), a.props.target, a.props.rel]),
    [
      ['https://example.com/docs', 'docs', '_blank', 'noreferrer'],
      ['mailto:someone@example.com', 'someone@example.com', '_blank', 'noreferrer'],
      ['https://example.com/shot.png', 'shot', '_blank', 'noreferrer'],
    ],
    'only http, https and mailto links become anchors and images show as links',
  );
});

test('inline fields show code and emphasis but never link', () => {
  const out = mdInline('Ship `v2` **now** via [the form](https://example.com)');

  assert.equal(tags([out], 'code')[0].children[0], 'v2');
  assert.equal(tags([out], 'strong')[0].children[0], 'now');
  assert.equal(tags([out], 'a').length, 0, 'titles and options never contain anchors');
  assert.equal(textOf([out]), 'Ship v2 now via the form');
});

test('a JSON body keeps its indentation in one code block', () => {
  const body = '{\n  "findings": [\n    {"title": "Apply queue is unavailable"}\n  ]\n}';
  const out = md(body).filter(Boolean);

  assert.equal(out.length, 1, 'the whole body renders as one block');
  assert.equal(out[0].tag, 'pre', 'that block is a code block');
  assert.equal(textOf(out), body, 'the text stays verbatim, indentation included');
});

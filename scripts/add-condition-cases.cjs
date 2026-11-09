#!/usr/bin/env node
'use strict';
// One-off generator: appends condition cases to the corpus; expected values come from the Node oracle.
const fs = require('node:fs');
const path = require('node:path');
require('../lib'); // registers the built-in formatters
const builder = require('../lib/builder');
const input = require('../lib/input');

const file = path.join(__dirname, '../tests/fixtures/template-parity.json');
const corpus = JSON.parse(fs.readFileSync(file, 'utf8'));
const data = { car: 'delorean', speed: 88, none: null, empty: [], list: ['a', 'b'], nums: [1, 2], text: 'hello world', zero: 0 };
const markers = {
  'if-eq-true': "{d.car:ifEQ('delorean'):show('yes'):elseShow('no')}",
  'if-eq-false': "{d.car:ifEQ('tesla'):show('yes'):elseShow('no')}",
  'if-ne': "{d.car:ifNE('tesla'):show('yes'):elseShow('no')}",
  'if-gt': "{d.speed:ifGT(80):show('fast'):elseShow('slow')}",
  'if-gte-lt-lte': "{d.speed:ifGTE(88):show('a')}{d.speed:ifLT(88):show('b'):elseShow('c')}{d.speed:ifLTE(88):show('d')}",
  'if-gt-nan': "{d.car:ifGT(1):show('yes'):elseShow('no')}",
  'or-chain': "{d.car:ifEQ('x'):ifEQ('delorean'):show('yes'):elseShow('no')}",
  'and-chain': "{d.car:ifEQ('delorean'):and:ifEQ('x'):show('yes'):elseShow('no')}",
  'and-first': "{d.car:and:ifEQ('delorean'):show('yes'):elseShow('no')}",
  'if-in-string': "{d.text:ifIN('world'):show('yes'):elseShow('no')}",
  'if-in-array': "{d.list:ifIN('b'):show('yes'):elseShow('no')}",
  'if-in-number-array': "{d.nums:ifIN(2):show('yes'):elseShow('no')}",
  'if-nin': "{d.list:ifNIN('z'):show('yes'):elseShow('no')}",
  'if-em': "{d.empty:ifEM:show('empty'):elseShow('full')}{d.none:ifEM:show('E')}{d.list:ifNEM:show('N')}",
  'show-truthiness': "[{d.car:show('t')}][{d.zero:show('t'):elseShow('f')}][{d.none:elseShow('f')}][{d.empty:show('arr')}]",
  'stop-propagation': "{d.car:ifEQ('delorean'):show('x'):upperCase}",
  'continue-after-condition': "{d.car:ifEQ('tesla'):show('x'):upperCase}",
  'legacy-if-empty': "{d.none:ifEmpty('none')}{d.car:ifEmpty('none')}{d.none:ifEmpty('none', true):prepend('>')}",
  'legacy-if-equal': "{d.car:ifEqual('delorean', 'same')}{d.car:ifEqual('x', 'same')}",
  'legacy-if-contain': "{d.text:ifContain('hello', 'has')}{d.list:ifContain('a', 'has')}{d.zero:ifContain('a', 'has')}",
  'condition-escapes-message': "{d.car:ifEQ('delorean'):show('<b> & co')}",
};
const added = [];
for (const [name, marker] of Object.entries(markers)) {
  const template = `<xml>${marker}</xml>`;
  builder.buildXML(template, data, { formatters: input.formatters }, (error, expected) => {
    if (error) throw new Error(`${name}: ${error}`);
    added.push({
      id: `condition-${name}`,
      feature: 'conditions',
      source: 'formatters/condition.js',
      reference_api: 'builder.buildXML',
      template,
      data,
      options: {},
      expected,
    });
  });
}
setImmediate(() => {
  const ids = new Set(added.map((c) => c.id));
  corpus.cases = corpus.cases.filter((c) => !ids.has(c.id)).concat(added);
  fs.writeFileSync(file, JSON.stringify(corpus, null, 2) + '\n');
  console.log(`Wrote ${added.length} condition cases`);
});

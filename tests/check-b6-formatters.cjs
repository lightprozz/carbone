#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const parser = require('../lib/parser');
const builder = require('../lib/builder');
const array = require('../formatters/array');
const string = require('../formatters/string');
const number = require('../formatters/number');
const packageInfo = require('../package.json');
const carbone = require('../lib');

const args = process.argv.slice(2);
assert.ok(args.length === 0 || (args.length === 1 && args[0] === '--update'),
  'Usage: node tests/check-b6-formatters.cjs [--update]');
const update = args[0] === '--update';
const filename = path.join(__dirname, 'formatter-fixtures/b6-extras.json');
const corpus = JSON.parse(fs.readFileSync(filename, 'utf8'));
assert.equal(corpus.reference.version, packageInfo.version, 'Node reference version changed');

function escape(value) {
  return typeof value === 'string'
    ? value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
      .replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f]/g, '')
    : value;
}

function reference(test) {
  if (test.kind === 'render_limitation') {
    const options = JSON.parse(JSON.stringify(test.options));
    return new Promise((resolve, reject) => carbone.renderXML(
      test.template, test.data, options, (error, result) => error ? reject(error) : resolve(result)));
  }
  if (test.kind === 'arithmetic') {
    const code = parser.parseMathematicalExpression(test.expression, operand =>
      JSON.stringify(test.operands && Object.hasOwn(test.operands, operand)
        ? test.operands[operand] : operand));
    // Execute only the original Node parser's generated oracle, never native code.
    return String(code ? vm.runInNewContext(code, {}, { timeout: 1000 }) : NaN);
  }
  if (test.kind === 'calculation') return number[test.name](test.value, ...test.args);
  const context = test.context;
  if (test.name === 'count') {
    const token = array.count(test.value, '123', ...test.args);
    const counters = {};
    let part;
    for (let count = 0; count <= context.count; count++) {
      part = { str: token, rowShow: true };
      builder.getLoopIteration(counters, part);
    }
    return part.str;
  }
  if (test.name === 'convEnum') {
    return string.convEnum.call(context, test.value, ...test.args);
  }
  assert.equal(test.name, 'convCRLF', `Unknown formatter ${test.name}`);
  return string.convCRLF.call(context, test.rendered ? escape(test.value) : test.value);
}

async function main() {
  const failures = [];
  for (const test of corpus.cases) {
    let actual;
    let error;
    try {
      actual = await reference(test);
    } catch (caught) {
      error = caught.message;
    }
    try {
      if (update) {
        delete test.expected;
        delete test.error;
        delete test.nonfinite;
        if (error !== undefined) test.error = error;
        else if (typeof actual === 'number' && !Number.isFinite(actual)) test.nonfinite = String(actual);
        else test.expected = actual;
      } else if (Object.hasOwn(test, 'nonfinite')) {
        assert.equal(error, undefined, test.id);
        assert.equal(String(actual), test.nonfinite, test.id);
      } else if (Object.hasOwn(test, 'error')) {
        assert.equal(error, test.error, test.id);
      } else {
        assert.equal(error, undefined, test.id);
        assert.deepEqual(actual, test.expected, test.id);
      }
    } catch (caught) {
      failures.push(caught.message);
    }
  }
  assert.equal(failures.length, 0, failures.join('\n'));
  if (update) fs.writeFileSync(filename, JSON.stringify(corpus, null, 2) + '\n');
  console.log(`${update ? 'Updated' : 'Verified'} ${corpus.cases.length} Node ${packageInfo.version} B6 formatter cases.`);

}
main().catch(error => { console.error(error); process.exitCode = 1; });

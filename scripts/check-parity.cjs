#!/usr/bin/env node
'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const carbone = require('../lib');
const builder = require('../lib/builder');
const input = require('../lib/input');
const packageInfo = require('../package.json');

async function main() {
  const args = process.argv.slice(2);
  if (args.length > 1 || (args.length === 1 && args[0] !== '--update')) {
    throw new Error('Usage: node scripts/check-parity.cjs [--update]');
  }
  const update = args[0] === '--update';
  const directory = path.join(__dirname, '../tests/fixtures');
  for (const entry of fs.readdirSync(directory).filter(name => name.endsWith('.json'))) {
  const filename = path.join(directory, entry);
  const corpus = JSON.parse(fs.readFileSync(filename, 'utf8'));
  assert.equal(corpus.reference.version, packageInfo.version, 'Node reference version changed');
  const failures = [];
  for (const test of corpus.cases) {
    try {
      // parseOptions mutates complement, so each reference call gets fresh options.
      const options = JSON.parse(JSON.stringify(test.options));
      const actual = await new Promise((resolve, reject) => {
        const callback = (error, result) => error ? reject(error) : resolve(result);
        if (test.reference_api === 'carbone.renderXML') {
          carbone.renderXML(test.template, test.data, options, callback);
        } else if (test.reference_api === 'builder.buildXML') {
          builder.buildXML(test.template, test.data, {...options, formatters: input.formatters}, callback);
        } else {
          reject(new Error(`Unknown reference API: ${test.reference_api}`));
        }
      });
      assert.equal(typeof actual, 'string', `${test.id}: reference did not return text`);
      if (update) test.expected = actual;
      else assert.equal(actual, test.expected, test.id);
    } catch (error) {
      failures.push(`${test.id}: ${error.message}`);
    }
  }
  assert.equal(failures.length, 0, failures.join('\n'));
  if (update) fs.writeFileSync(filename, JSON.stringify(corpus, null, 2) + '\n');
  console.log(`${update ? 'Updated' : 'Verified'} ${corpus.cases.length} Node ${packageInfo.version} reference cases (${entry}).`);
  }
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});

#!/usr/bin/env node
'use strict';
/**
 * Run the shared corpus through the Node client and print the results.
 *
 * Emitter contract (see README.md): read `vectors.json` from argv[2], write
 * `{client, results: [{id, ok, ...}]}` to stdout. Nothing is judged here —
 * `check.py` does the comparing, so an emitter can never quietly grade itself.
 */
const fs = require('node:fs');
const path = require('node:path');

const { parseProxy } = require(path.join(__dirname, '..', 'node', 'src', 'core'));

function run(testCase) {
  const { value } = testCase.input;
  try {
    const spec = parseProxy(value);
    return {
      id: testCase.id,
      ok: true,
      server: spec.server,
      username: spec.username,
      password: spec.password,
      authenticated: spec.authenticated,
    };
  } catch (err) {
    return { id: testCase.id, ok: false, error: `${err.name}: ${err.message}` };
  }
}

const corpus = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
process.stdout.write(
  JSON.stringify({ client: 'node', results: corpus.cases.map(run) }),
);

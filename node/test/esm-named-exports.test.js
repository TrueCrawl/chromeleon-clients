'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { pathToFileURL } = require('node:url');

// Regression guard for the 0.1.0 -> 0.1.1 fix.
//
// Node can name-import from a CommonJS module only when cjs-module-lexer
// statically detects the export names, and it cannot see through member
// expressions. Writing `module.exports = { launch: adapters.launch, ... }`
// therefore yields a module whose only usable ESM shape is the default import,
// and every `import { launch } from 'chromeleon'` in the wild breaks.
//
// The failure is invisible from CommonJS — require() works identically either
// way — so nothing in the rest of this suite would catch a regression. Assert
// the ESM namespace directly.

const ENTRY = pathToFileURL(require.resolve('../src/index.js')).href;

test('every documented export is name-importable from ESM', async () => {
  const ns = await import(ENTRY);

  // A CJS module ALWAYS gets a default export, so its presence proves nothing.
  assert.ok(ns.default, 'sanity: the module loaded');

  const expected = [
    // core protocol
    'LAUNCH_ARGS', 'CREDENTIALS_METHOD', 'ProxySpec', 'normalizeServer',
    'parseProxy', 'credentialsParams', 'checkRegistration',
    'withProxyRegistration',
    // driver adapters
    'launch', 'browserProcessEnv', 'newProxyContext',
    'newProxyContextPuppeteer', 'newProxyContextWith',
    // captcha solver
    'CAPTCHA_SOLVER_SWITCH', 'CAPTCHA_MODEL_PATH_SWITCH', 'ENABLE_METHOD',
    'DISABLE_METHOD', 'SOLVER_EVAL_METHOD', 'CAPTCHA_DETECTED',
    'CAPTCHA_SOLVING', 'CAPTCHA_SOLVED', 'CAPTCHA_FAILED',
    'SOLVER_EVAL_RESULT', 'CAPTCHA_EVENTS', 'captchaLaunchArgs',
    'solverEvalParams', 'enableCaptcha', 'disableCaptcha', 'solverEval',
  ];

  const missing = expected.filter((name) => ns[name] === undefined);
  assert.deepEqual(
    missing, [],
    'not name-importable from ESM — index.js must destructure into locals and ' +
    're-export them as shorthand, never as `name: module.name`',
  );
});

test('the ESM namespace and the CommonJS export agree', async () => {
  const ns = await import(ENTRY);
  const cjs = require('../src/index.js');
  for (const name of Object.keys(cjs)) {
    assert.equal(ns[name], cjs[name], `${name} differs between ESM and CJS`);
  }
});

'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const {
  LAUNCH_ARGS,
  ENABLE_METHOD,
  DISABLE_METHOD,
  SOLVER_EVAL_METHOD,
  CAPTCHA_EVENTS,
  CAPTCHA_SOLVED,
  captchaLaunchArgs,
  solverEvalParams,
  launch,
  enableCaptcha,
  disableCaptcha,
  solverEval,
} = require('../src');

function stubChromium() {
  return { kw: null, async launch(kw) { this.kw = kw; return 'browser'; } };
}

function stubCdp() {
  return { calls: [], send(method, params) { this.calls.push([method, params]); return Promise.resolve('ok'); } };
}

// --- launch flags ---------------------------------------------------------

test('captchaLaunchArgs default is just the solver switch', () => {
  assert.deepEqual(captchaLaunchArgs(), ['--captcha-solver']);
});

test('captchaLaunchArgs with a model-path override', () => {
  assert.deepEqual(captchaLaunchArgs('/models'),
    ['--captcha-solver', '--captcha-model-path=/models']);
});

test('launch({captcha:true}) appends --captcha-solver and keeps the WebRTC policy', async () => {
  const ch = stubChromium();
  await launch(ch, '/x', { captcha: true });
  assert.ok(ch.kw.args.includes('--captcha-solver'));
  assert.ok(ch.kw.args.includes(LAUNCH_ARGS[0]));
  assert.ok(!ch.kw.args.some((a) => a.startsWith('--captcha-model-path')));
});

test('launch({captchaModelPath}) implies the solver and adds the path', async () => {
  const ch = stubChromium();
  await launch(ch, '/x', { captchaModelPath: '/m' });
  assert.ok(ch.kw.args.includes('--captcha-solver'));
  assert.ok(ch.kw.args.includes('--captcha-model-path=/m'));
});

test('launch() without captcha has no solver flag', async () => {
  const ch = stubChromium();
  await launch(ch, '/x');
  assert.ok(!ch.kw.args.some((a) => a.includes('captcha')));
});

test('launch does not duplicate a caller-supplied captcha flag', async () => {
  const ch = stubChromium();
  await launch(ch, '/x', { captcha: true, args: ['--captcha-solver'] });
  assert.equal(ch.kw.args.filter((a) => a === '--captcha-solver').length, 1);
});

// --- CDP domain helpers ---------------------------------------------------

test('enableCaptcha / disableCaptcha send the right commands', async () => {
  const cdp = stubCdp();
  await enableCaptcha(cdp);
  await disableCaptcha(cdp);
  assert.deepEqual(cdp.calls, [[ENABLE_METHOD, undefined], [DISABLE_METHOD, undefined]]);
});

test('solverEval sends expression and frame selector', async () => {
  const cdp = stubCdp();
  await solverEval(cdp, 'document.title', 'checkout');
  assert.deepEqual(cdp.calls,
    [[SOLVER_EVAL_METHOD, { expression: 'document.title', frameUrlContains: 'checkout' }]]);
});

test('solverEvalParams defaults to the primary main frame', () => {
  assert.deepEqual(solverEvalParams('x'), { expression: 'x', frameUrlContains: '' });
});

test('CAPTCHA_EVENTS are the four lifecycle events', () => {
  assert.ok(CAPTCHA_EVENTS.includes(CAPTCHA_SOLVED));
  assert.equal(CAPTCHA_EVENTS.length, 4);
});

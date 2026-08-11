// Async Playwright example: Chromeleon's built-in reCAPTCHA/hCaptcha solver.
// Launch with the solver on, open a captcha page, and watch it solve over the
// Chromeleon CDP events. Run with:
//
//   CHROMELEON=/path/to/chrome node examples/captcha.mjs
//
// Requires `playwright` installed alongside this package. The solver runs
// AUTOMATICALLY once enabled at launch — it detects the widget, solves it
// (audio first, image fallback), and writes the token; the CDP domain below
// only reports the lifecycle so you know when to proceed.
import { chromium } from 'playwright';
import {
  launch,
  enableCaptcha,
  disableCaptcha,
  CAPTCHA_DETECTED,
  CAPTCHA_SOLVING,
  CAPTCHA_SOLVED,
  CAPTCHA_FAILED,
} from 'chromeleon';

const CHROMELEON = process.env.CHROMELEON;
const URL =
  process.env.CAPTCHA_URL ??
  'https://recaptcha-demo.appspot.com/recaptcha-v2-checkbox.php';

if (!CHROMELEON) {
  console.error('set CHROMELEON to the Chromeleon chrome binary path');
  process.exit(2);
}

// captcha: true appends --captcha-solver; the release binary embeds the models.
const browser = await launch(chromium, CHROMELEON, { captcha: true });
try {
  const page = await browser.newPage();
  const cdp = await page.context().newCDPSession(page);

  const solved = new Promise((resolve, reject) => {
    cdp.on(CAPTCHA_DETECTED, (p) =>
      console.log('detected  sitekey', p.sitekey.slice(0, 12) + '...'));
    cdp.on(CAPTCHA_SOLVING, (p) => console.log('solving   via', p.method));
    cdp.on(CAPTCHA_SOLVED, (p) => {
      console.log(`solved    in ${Math.round(p.timeMs)}ms, ${p.attempts} attempt(s)`);
      resolve(p);
    });
    cdp.on(CAPTCHA_FAILED, (p) =>
      reject(new Error(`captcha failed: ${p.reason}`)));
  });

  await enableCaptcha(cdp);
  await page.goto(URL, { waitUntil: 'domcontentloaded' });

  // Wait for the solved event (or fall back to a timeout / the response
  // textarea, whichever your flow prefers).
  await Promise.race([
    solved,
    new Promise((_, r) => setTimeout(() => r(new Error('timed out')), 90_000)),
  ]);

  await disableCaptcha(cdp);
} finally {
  await browser.close();
}

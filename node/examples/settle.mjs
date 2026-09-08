// Async Playwright example: wait for a page to actually be FINISHED, and tell a
// bot wall apart from a page. Run with:
//
//   CHROMELEON=/path/to/chrome node examples/settle.mjs https://example.com
//
// Requires `playwright` installed alongside this package.
//
// `Chromeleon.waitForSettle` settles when the main frame's rendered text has
// held still for a quiet window, sampled in the BROWSER process — nothing runs
// in the page. It needs --page-settle at launch (pageSettle: true below), and
// without it this falls back to Blink's networkAlmostIdle by itself.
//
// Through a proxy — which is what this client is for — networkAlmostIdle never
// fires on 36% of loads against waitForSettle's 7% (960 navigations, 80 sites).
// Un-proxied, networkAlmostIdle is 1.8x faster: pass
// { prefer: 'networkAlmostIdle' } to settleWatch and skip the command.
import { chromium } from 'playwright';
import { launch, settleWatch } from 'chromeleon';

const CHROMELEON = process.env.CHROMELEON;
const URL = process.argv[2] ?? 'https://example.com';

if (!CHROMELEON) {
  console.error('set CHROMELEON to the Chromeleon chrome binary path');
  process.exit(2);
}

const browser = await launch(chromium, CHROMELEON, { pageSettle: true });
try {
  const page = await browser.newPage();

  // ARM BEFORE NAVIGATING. The challenge header is recorded at commit time, and
  // a session attached to an already-committed document cannot see it.
  const watch = await settleWatch(page);
  try {
    await page.goto(URL, { waitUntil: 'commit' });
    const state = await watch.wait({ timeoutMs: 30_000 });

    console.log(
      `${state.outcome.padEnd(9)} via ${state.via.padEnd(17)} ` +
      `${state.elapsedMs ?? '?'}ms  ${state.textLength ?? '?'} chars  ` +
      `HTTP ${state.httpStatus ?? '?'}`,
    );
    if (state.blocked) {
      // 12.5% of proxied navigations in production measurement. A wall shorter
      // than minChars reports "timeout" and still carries its 403, which is why
      // this reads `blocked` and not `outcome`.
      console.error('bot wall, not a page:', state.reason ?? '(no reason given)');
      process.exitCode = 1;
    } else if (state.outcome === 'timeout') {
      console.error('never settled — on proxied traffic that is normal, not an error');
    }
  } finally {
    await watch.close();
  }
} finally {
  await browser.close();
}

// Async Playwright example: one Chromeleon browser, two contexts each behind a
// different authenticated per-context proxy. Run with:
//
//   CHROMELEON=/path/to/chrome \
//   PROXY_A=http://user:pass@gwA:12321 \
//   PROXY_B=http://user:pass@gwB:12321 \
//   node examples/per-context-proxy.mjs
//
// Requires `playwright` installed alongside this package.
import { chromium } from 'playwright';
import { launch, newProxyContext } from 'chromeleon';

const CHROMELEON = process.env.CHROMELEON;
const PROXY_A = process.env.PROXY_A;
const PROXY_B = process.env.PROXY_B ?? PROXY_A;

if (!CHROMELEON || !PROXY_A) {
  console.error('set CHROMELEON and PROXY_A (and optionally PROXY_B)');
  process.exit(2);
}

const browser = await launch(chromium, CHROMELEON);
try {
  // Each context gets its own exit. newProxyContext runs the setProxyCredentials
  // -> createBrowserContext handshake under the lock; awaiting it is all you do.
  const [ctxA, ctxB] = await Promise.all([
    newProxyContext(browser, PROXY_A),
    newProxyContext(browser, PROXY_B),
  ]);

  const [ipA, ipB] = await Promise.all(
    [ctxA, ctxB].map(async (ctx) => {
      const page = await ctx.newPage();
      await page.goto('https://api.ipify.org?format=json', { waitUntil: 'domcontentloaded' });
      const body = await page.evaluate(() => document.body.innerText);
      return JSON.parse(body).ip;
    }),
  );

  console.log('context A exit IP:', ipA);
  console.log('context B exit IP:', ipB);
} finally {
  await browser.close();
}

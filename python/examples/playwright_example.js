#!/usr/bin/env node
/**
 * Chromeleon Playwright Example
 *
 * Usage:
 *     node playwright_example.js --chromeleon-path /path/to/chrome
 *     node playwright_example.js --chromeleon-path /path/to/chrome --headless
 *     node playwright_example.js --chromeleon-path /path/to/chrome --proxy http://user:pass@host:port
 *     node playwright_example.js --chromeleon-path /path/to/chrome --fingerprint-os windows
 *     node playwright_example.js --chromeleon-path /path/to/chrome --test  # Run fingerprint tests
 */

const { chromium } = require('playwright');
const fs = require('fs');
const path = require('path');

// Fingerprint detection test sites
const TEST_SITES = [
    { url: 'https://demo.fingerprint.com/playground', name: 'FingerprintJS' },
    { url: 'https://overpoweredjs.com/', name: 'OverpoweredJS' },
];

async function extractFingerprintJSScores(page) {
    const bodyText = await page.evaluate(() => document.body.innerText);
    const results = {};
    const lines = bodyText.split('\n');

    for (let i = 0; i < lines.length; i++) {
        const line = lines[i].trim();
        if (line.includes('CONFIDENCE SCORE')) {
            for (let j = i + 1; j < Math.min(i + 3, lines.length); j++) {
                if (lines[j].trim()) {
                    results['Confidence Score'] = lines[j].trim();
                    break;
                }
            }
        }
        if (line.includes('SUSPECT SCORE')) {
            for (let j = i + 1; j < Math.min(i + 3, lines.length); j++) {
                if (lines[j].trim() && /^\d+$/.test(lines[j].trim())) {
                    results['Suspect Score'] = lines[j].trim();
                    break;
                }
            }
        }
        if (line.includes('Chrome') && line.includes(' on ')) {
            results['Detected Browser'] = line.trim();
        }
        if (line === 'OPERATING SYSTEM') {
            for (let j = i + 1; j < Math.min(i + 3, lines.length); j++) {
                const val = lines[j].trim();
                if (val && ['Windows', 'macOS', 'Linux', 'Android', 'iOS'].includes(val)) {
                    results['Detected OS'] = val;
                    break;
                }
            }
        }
    }
    return results;
}

async function runTests(page, outputDir = '.') {
    console.log('\n=== Running Fingerprint Detection Tests ===\n');

    for (const { url, name } of TEST_SITES) {
        console.log(`Testing: ${name}`);
        console.log(`  URL: ${url}`);

        try {
            await page.goto(url, { timeout: 60000, waitUntil: 'domcontentloaded' });
            await new Promise(r => setTimeout(r, 8000)); // Wait for JS fingerprinting

            // Extract FingerprintJS scores
            if (name === 'FingerprintJS') {
                const scores = await extractFingerprintJSScores(page);
                for (const [key, val] of Object.entries(scores)) {
                    console.log(`  ${key}: ${val}`);
                }
            }

            const filename = `${outputDir}/test_${name.toLowerCase().replace(/ /g, '_')}.png`;
            await page.screenshot({ path: filename, fullPage: true });
            console.log(`  Screenshot: ${filename}`);
        } catch (e) {
            console.log(`  Error: ${e.message}`);
        }

        console.log();
    }
}

function parseAuthenticatedProxy(proxyUrl) {
    const normalized = proxyUrl.includes('://') ? proxyUrl : `http://${proxyUrl}`;
    const parsed = new URL(normalized);
    if (!['http:', 'https:'].includes(parsed.protocol) || !parsed.hostname) {
        throw new Error('--proxy must be one valid HTTP(S) proxy URL');
    }
    if (!parsed.username || !normalized.includes('@')) {
        throw new Error('--proxy must include username:password@');
    }
    if ((parsed.pathname && parsed.pathname !== '/') || parsed.search || parsed.hash) {
        throw new Error('--proxy must not contain a path, query, or fragment');
    }
    return {
        server: `${parsed.protocol}//${parsed.host}`,
        username: decodeURIComponent(parsed.username),
        password: decodeURIComponent(parsed.password),
    };
}

function browserProcessEnv() {
    return Object.fromEntries(
        Object.entries(process.env).filter(
            ([key]) => !key.toUpperCase().includes('PROXY')
        )
    );
}

async function main() {
    const args = parseArgs();

    if (!args.chromeleonPath) {
        console.log('Error: --chromeleon-path is required');
        process.exit(1);
    }

    // Resolve to absolute path
    const chromeleonPath = path.resolve(args.chromeleonPath);

    if (!fs.existsSync(chromeleonPath)) {
        console.log(`Error: Chromeleon binary not found: ${chromeleonPath}`);
        process.exit(1);
    }

    // Build launch args
    // --webrtc-ip-handling-policy=disable_non_proxied_udp:
    // Required whenever a proxy is in play. Context-level proxy attachment
    // (newContext({proxy:...})) routes HTTP through the proxy but WebRTC's
    // UDP lives at browser level and uses the default route, leaking the
    // real public IP via ICE srflx candidates. This flag suppresses
    // unproxied UDP gathering for the server-only context created below.
    const launchArgs = [
        '--disable-blink-features=AutomationControlled',
        '--disable-infobars',
        '--no-first-run',
        '--disable-background-networking',
        '--webrtc-ip-handling-policy=disable_non_proxied_udp',
    ];

    if (args.fingerprintOs) {
        launchArgs.push(`--fingerprint-os=${args.fingerprintOs}`);
    }

    if (args.headless) {
        launchArgs.push('--headless=new');
    }

    console.log(`Chromeleon: ${chromeleonPath}`);
    console.log(`Headless: ${args.headless}`);
    console.log(`Proxy: ${args.proxy ? 'configured' : 'none'}`);
    console.log(`Fingerprint OS: ${args.fingerprintOs || 'default'}`);

    const browser = await chromium.launch({
        executablePath: chromeleonPath,
        args: launchArgs,
        headless: args.headless,
        env: browserProcessEnv(),
    });

    const contextOptions = { viewport: { width: 1920, height: 1080 } };
    let context;
    if (args.proxy) {
        const proxy = parseAuthenticatedProxy(args.proxy);
        const cdp = await browser.newBrowserCDPSession();
        try {
            const result = await cdp.send('Target.setProxyCredentials', {
                proxyServer: proxy.server,
                username: proxy.username,
                password: proxy.password,
            });
            if (Object.keys(result).length !== 0) {
                throw new Error(`unexpected credential registration: ${JSON.stringify(result)}`);
            }
        } finally {
            await cdp.detach();
        }
        context = await browser.newContext({
            ...contextOptions,
            proxy: { server: proxy.server },
        });
    } else {
        context = await browser.newContext(contextOptions);
    }

    const page = await context.newPage();

    if (args.test) {
        // Run fingerprint detection tests
        await runTests(page);
    } else {
        // Single page visit
        const url = args.url || 'https://browserleaks.com/javascript';
        await page.goto(url);
        await page.waitForLoadState('networkidle');

        await page.screenshot({ path: 'screenshot.png' });
        console.log('\nScreenshot saved: screenshot.png');

        const title = await page.title();
        console.log(`Page title: ${title}`);
    }

    if (!args.headless) {
        console.log('\nBrowser open. Press Ctrl+C to close...');
        await new Promise(resolve => setTimeout(resolve, 300000));
    }

    await browser.close();
}

function parseArgs() {
    const args = {
        chromeleonPath: null,
        headless: false,
        proxy: null,
        fingerprintOs: null,
        test: false,
        url: null,
    };

    for (let i = 2; i < process.argv.length; i++) {
        const arg = process.argv[i];

        if (arg === '--chromeleon-path' && process.argv[i + 1]) {
            args.chromeleonPath = process.argv[++i];
        } else if (arg === '--headless') {
            args.headless = true;
        } else if (arg === '--proxy' && process.argv[i + 1]) {
            args.proxy = process.argv[++i];
        } else if (arg === '--fingerprint-os' && process.argv[i + 1]) {
            const os = process.argv[++i];
            if (!['windows', 'linux', 'macos'].includes(os)) {
                console.log('Error: --fingerprint-os must be windows, linux, or macos');
                process.exit(1);
            }
            args.fingerprintOs = os;
        } else if (arg === '--test') {
            args.test = true;
        } else if (arg === '--url' && process.argv[i + 1]) {
            args.url = process.argv[++i];
        } else if (arg === '--help' || arg === '-h') {
            console.log(`
Usage:
    node playwright_example.js --chromeleon-path /path/to/chrome [options]

Options:
    --chromeleon-path PATH     Path to Chromeleon chrome binary (required)
    --headless                 Run in headless mode
    --proxy URL                Authenticated HTTP(S) proxy URL
    --fingerprint-os OS        OS to spoof: windows, linux, macos
    --test                     Run fingerprint detection tests
    --url URL                  Custom URL to visit
`);
            process.exit(0);
        }
    }

    return args;
}

main().catch(err => {
    console.error(err);
    process.exit(1);
});

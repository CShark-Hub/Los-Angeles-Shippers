import { defineConfig, type Project } from '@playwright/test';

/**
 * The smoke tests of the web client, against a running Compose stack: Caddy serving the built client
 * and the core behind it (`just it --web` starts one and sets MERCURY_E2E_* for it). Not part of
 * `pnpm test`: they need the stack.
 *
 * The browsers are projects: `chromium`, `edge`, `firefox` and `webkit` (Safari's engine). Which run
 * is MERCURY_E2E_BROWSERS, a comma-separated list (`chromium` by default; `just it --web` passes it
 * through); each needs its browser (`pnpm exec playwright install chromium firefox webkit`, and
 * `msedge` for Edge where it isn't installed). MERCURY_E2E_CHANNEL=msedge (or chrome) runs the
 * `chromium` project in a browser already installed on the machine.
 */
const BROWSERS: Record<string, Project['use']> = {
	// the full Chromium in its new headless mode (`chromium`), whose permissions are a real browser's;
	// the headless shell denies notifications
	chromium: { browserName: 'chromium', channel: process.env.MERCURY_E2E_CHANNEL || 'chromium' },
	edge: { browserName: 'chromium', channel: 'msedge' },
	firefox: { browserName: 'firefox' },
	webkit: { browserName: 'webkit' }
};

const wanted = (process.env.MERCURY_E2E_BROWSERS || 'chromium')
	.split(',')
	.map((name) => name.trim())
	.filter(Boolean);
for (const name of wanted) {
	if (!(name in BROWSERS)) {
		throw new Error(
			`MERCURY_E2E_BROWSERS names ${name}, not one of ${Object.keys(BROWSERS).join(', ')}`
		);
	}
}

export default defineConfig({
	testDir: './e2e',
	outputDir: './test-results',
	// the core limits sign-ins by address, so one signed-in person at a time
	workers: 1,
	// no retries: a test that passes only the second time is a finding
	retries: 0,
	timeout: 60_000,
	expect: { timeout: 15_000 },
	reporter: [['list']],
	use: {
		baseURL: process.env.MERCURY_E2E_URL ?? 'http://localhost:18080',
		// a trace records what is typed, the token included, so none is kept
		trace: 'off',
		screenshot: 'off',
		video: 'off'
	},
	projects: wanted.map((name) => ({ name, use: BROWSERS[name] }))
});

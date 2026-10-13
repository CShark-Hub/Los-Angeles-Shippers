import { describe, expect, it } from 'vitest';

/**
 * Every source file of the app, as text. Nothing sensitive may reach browser storage, a cookie
 * the page writes or a URL, so the places that could are listed, and the list is one file long.
 */
const sources = import.meta.glob('/src/**/*.{ts,svelte}', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;

const app = Object.entries(sources).filter(
	([path]) => !/\.(spec|test)\.ts$/.test(path) && !path.endsWith('/schema.d.ts')
);

function mentioning(pattern: RegExp): string[] {
	return app.filter(([, text]) => pattern.test(text)).map(([path]) => path);
}

describe('browser storage', () => {
	it('is touched by the preferences module and nothing else', () => {
		expect(app.length).toBeGreaterThan(5);
		const storage = /\b(localStorage|sessionStorage|indexedDB|caches|document\.cookie)\b/;
		expect(mentioning(storage)).toEqual(['/src/lib/preferences.ts']);
	});

	it('holds one key, the look the person picked', () => {
		const text = sources['/src/lib/preferences.ts'];
		const code = text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '');
		expect(code.match(/setItem\(/g)).toHaveLength(1);
		expect(code).toContain("'mercury.theme'");
		// nothing about the person or what they asked is anywhere near it
		expect(code).not.toMatch(/token|session|question|answer|email/i);
	});

	it('is never given a page snapshot, which the framework writes to sessionStorage', () => {
		expect(mentioning(/export\s+(const|let|function)\s+snapshot\b/)).toEqual([]);
	});

	it('never puts a token in a URL or a hand-written cookie', () => {
		expect(mentioning(/[?&]token=|[?&]session=/i)).toEqual([]);
		expect(mentioning(/\bSet-Cookie\b|document\.cookie/i)).toEqual([]);
	});
});

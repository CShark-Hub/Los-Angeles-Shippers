import { describe, expect, it } from 'vitest';

/**
 * The page's policy lets nothing load from outside its own origin (`csp.js`), so a source that asks
 * another site for a script, a font, an image or an API would only break in the browser. This
 * finds it earlier, in the source: the markup, the styles and the code, as text.
 */
const sources = import.meta.glob('/src/**/*.{ts,svelte,css,html}', {
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

const REMOTE = String.raw`(?:https?:)?//`;

describe('what the app loads', () => {
	it('has sources to look at', () => {
		expect(app.map(([path]) => path)).toContain('/src/app.html');
		expect(app.length).toBeGreaterThan(20);
	});

	it('names no script, stylesheet, image or frame on another site', () => {
		expect(
			mentioning(new RegExp(String.raw`\b(src|href|poster|srcset)\s*=\s*["']?${REMOTE}[\w.-]`, 'i'))
		).toEqual([]);
	});

	it('imports no style and no font from another site', () => {
		expect(mentioning(new RegExp(String.raw`url\(\s*["']?${REMOTE}[\w.-]`, 'i'))).toEqual([]);
		expect(
			mentioning(new RegExp(String.raw`@import\s+(url\(\s*)?["']?${REMOTE}[\w.-]`, 'i'))
		).toEqual([]);
	});

	it('asks no other origin for anything', () => {
		const call = String.raw`\b(fetch|EventSource|WebSocket|XMLHttpRequest|sendBeacon|import)\b\s*\(?[^;\n]*["'\x60]((https?|wss?):)?//[\w.-]`;
		expect(mentioning(new RegExp(call))).toEqual([]);
	});
});

import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

const layout = readFileSync(new URL('../routes/layout.css', import.meta.url), 'utf8');

/**
 * WCAG 2.2 AA over the two looks (the neutral light one and `demo`, the dark one): 4.5:1 for every
 * text colour the app uses on every surface it may sit on, 3:1 for the border of a control and for
 * the focus indicator. The colours are read from the tokens in `layout.css`, and the text colours
 * in use from the classes in the sources, so a colour added later is checked without a line here.
 */

const sources = import.meta.glob('/src/**/*.{ts,svelte}', {
	query: '?raw',
	import: 'default',
	eager: true
}) as Record<string, string>;
const app = Object.entries(sources).filter(
	([path]) => !/\.(spec|test)\.ts$/.test(path) && !path.endsWith('/schema.d.ts')
);

type Look = Record<string, string>;

function variables(block: string): Look {
	return Object.fromEntries(
		[...block.matchAll(/--([\w-]+):\s*([^;]+);/g)].map(([, name, value]) => [name, value.trim()])
	);
}

const rootAt = layout.indexOf(':root');
const demoAt = layout.indexOf("[data-theme='demo']");
const themeAt = layout.indexOf('@theme');
const neutral = variables(layout.slice(rootAt, demoAt));
const looks: Record<string, Look> = {
	neutral,
	demo: { ...neutral, ...variables(layout.slice(demoAt, themeAt)) }
};

type Rgb = [number, number, number];

/** A token as a colour: `#rrggbb`, `var(--other)`, or `rgb(r g b / a)` laid over `under`. */
function colour(look: Look, name: string, under?: Rgb): Rgb {
	const value = look[name];
	if (value === undefined) throw new Error(`no token --${name}`);
	const reference = /^var\(--([\w-]+)\)$/.exec(value);
	if (reference) return colour(look, reference[1], under);
	const hex = /^#([0-9a-f]{6})$/i.exec(value);
	if (hex) return [0, 2, 4].map((i) => parseInt(hex[1].slice(i, i + 2), 16)) as Rgb;
	const rgb = /^rgb\((\d+) (\d+) (\d+)(?: \/ ([\d.]+))?\)$/.exec(value);
	if (!rgb) throw new Error(`--${name} is a colour this check can't read: ${value}`);
	const [r, g, b] = [rgb[1], rgb[2], rgb[3]].map(Number);
	const alpha = rgb[4] === undefined ? 1 : Number(rgb[4]);
	if (alpha === 1) return [r, g, b];
	if (!under) throw new Error(`--${name} is see-through: say what it lies on`);
	return [r, g, b].map((c, i) => Math.round(c * alpha + under[i] * (1 - alpha))) as Rgb;
}

function luminance([r, g, b]: Rgb): number {
	const [lr, lg, lb] = [r, g, b].map((c) => {
		const s = c / 255;
		return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
	});
	return 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
}

function ratio(a: Rgb, b: Rgb): number {
	const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x);
	return (light + 0.05) / (dark + 0.05);
}

/** Where text and controls sit: the page, a card, and a card's inset. */
const SURFACES = ['bg', 'surface', 'surface-2'];

/** The text colours the sources use as `text-<token>` (`text-text-2` is the token `text-2`). */
const TEXT = ['text-3', 'text-2', 'text', 'accent-text', 'danger', 'warning', 'success', 'info'];
function textTokensInUse(): string[] {
	const used = new Set<string>();
	for (const [, text] of app) {
		for (const [, name] of text.matchAll(
			/(?<![\w-])text-(text-3|text-2|text|accent-text|danger|warning|success|info)(?![\w-])/g
		))
			used.add(name);
	}
	return TEXT.filter((name) => used.has(name));
}

const each = Object.entries(looks);

describe('contrast, WCAG 2.2 AA', () => {
	it('reads the tokens of both looks, and finds the text colours the app uses', () => {
		expect(Object.keys(neutral)).toContain('text-3');
		expect(looks.demo.bg).toBe('#0a0b0f');
		const used = textTokensInUse();
		// the scan is not empty, and sees the secondary text and the error colour
		expect(used).toEqual(expect.arrayContaining(['text', 'text-2', 'text-3', 'danger']));
	});

	describe.each(each)('in the %s look', (_name, look) => {
		it('gives every text colour in use 4.5:1 on the page, a card and a card inset', () => {
			for (const token of textTokensInUse()) {
				for (const surface of SURFACES) {
					const got = ratio(colour(look, token), colour(look, surface));
					expect(got, `text-${token} on ${surface}`).toBeGreaterThanOrEqual(4.5);
				}
			}
		});

		it('gives the text on a filled button 4.5:1, resting and hovered', () => {
			for (const [text, fill] of [
				['on-accent', 'accent'],
				['on-accent', 'accent-hover'],
				['on-accent-2', 'accent-2']
			]) {
				const got = ratio(colour(look, text), colour(look, fill));
				expect(got, `${text} on ${fill}`).toBeGreaterThanOrEqual(4.5);
			}
		});

		it('gives the text on a highlighted sentence 4.5:1', () => {
			for (const surface of ['surface', 'surface-2']) {
				const under = colour(look, surface);
				const got = ratio(colour(look, 'text'), colour(look, 'accent-soft', under));
				expect(got, `text on accent-soft over ${surface}`).toBeGreaterThanOrEqual(4.5);
			}
		});

		it('gives the border of a control 3:1 against the page and the cards', () => {
			for (const surface of SURFACES) {
				const got = ratio(colour(look, 'border-strong'), colour(look, surface));
				expect(got, `border-strong on ${surface}`).toBeGreaterThanOrEqual(3);
			}
		});

		it('gives the focus indicator 3:1 against the page and the cards', () => {
			for (const surface of SURFACES) {
				const got = ratio(colour(look, 'ring'), colour(look, surface));
				expect(got, `ring on ${surface}`).toBeGreaterThanOrEqual(3);
			}
		});
	});

	it('draws the placeholder in a text colour that passes, not in the text colour at half strength', () => {
		const rule = /::placeholder\s*\{([^}]*)\}/.exec(layout)?.[1] ?? '';
		expect(rule).toContain('var(--text-3)');
		expect(rule).toMatch(/opacity:\s*1/);
	});

	it('shows the focus of every control with the one indicator, and hides it nowhere', () => {
		expect(layout).toContain('@utility focus-ring');
		const hidden = app
			.filter(([, text]) => /focus-visible:(outline-none|ring-)|focus:ring-/.test(text))
			.map(([path]) => path);
		expect(hidden).toEqual([]);
	});
});

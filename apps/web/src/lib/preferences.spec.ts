import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	applyTheme,
	forgetBuiltTheme,
	loadTheme,
	saveTheme,
	THEME_KEY,
	type ThemeChoice
} from './preferences';

/** A `Storage` in memory that records what was written. */
class MemoryStorage {
	items = new Map<string, string>();
	getItem = (key: string) => this.items.get(key) ?? null;
	setItem = (key: string, value: string) => void this.items.set(key, value);
	removeItem = (key: string) => void this.items.delete(key);
}

let memory: MemoryStorage;

beforeEach(() => {
	memory = new MemoryStorage();
	vi.stubGlobal('localStorage', memory);
	forgetBuiltTheme();
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('the theme preference', () => {
	it('saves a choice under one key and reads it back', () => {
		expect(loadTheme()).toBe('default');
		saveTheme('demo');
		const items = [...memory.items];
		expect(items).toHaveLength(1);
		expect(items[0]).toEqual([THEME_KEY, 'demo']);
		expect(loadTheme()).toBe('demo');
		saveTheme('neutral');
		expect(loadTheme()).toBe('neutral');
	});

	it('forgets the choice when it goes back to the built look', () => {
		saveTheme('demo');
		saveTheme('default');
		expect(memory.items.size).toBe(0);
	});

	it('ignores a saved value that is not a choice', () => {
		memory.items.set(THEME_KEY, 'hotpink');
		expect(loadTheme()).toBe('default');
	});

	it('works without storage, which a private window or a blocked site may refuse', () => {
		vi.stubGlobal('localStorage', {
			getItem() {
				throw new DOMException('denied', 'SecurityError');
			},
			setItem() {
				throw new DOMException('denied', 'SecurityError');
			},
			removeItem() {
				throw new DOMException('denied', 'SecurityError');
			}
		});
		expect(loadTheme()).toBe('default');
		expect(() => saveTheme('demo')).not.toThrow();
		vi.stubGlobal('localStorage', undefined);
		expect(loadTheme()).toBe('default');
	});

	it('puts the look on the page, and gives back the built one for the default', () => {
		const root = { dataset: { theme: 'demo' } as { theme?: string } };
		const apply = (choice: ThemeChoice) => {
			applyTheme(choice, root);
			return root.dataset.theme;
		};
		expect(apply('neutral')).toBe('neutral');
		expect(apply('demo')).toBe('demo');
		// the page was built with the demo look, and `default` is that, not the last choice
		expect(apply('default')).toBe('demo');

		forgetBuiltTheme();
		const plain = { dataset: {} as { theme?: string } };
		applyTheme('demo', plain);
		applyTheme('default', plain);
		expect(plain.dataset.theme).toBe('');
	});
});

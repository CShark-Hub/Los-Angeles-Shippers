/**
 * The one thing the app keeps in the browser: a UI preference, the look the person picked. It is
 * the only module that touches browser storage (`storage.spec.ts` checks that), and it keeps
 * nothing about the person, the token, the session or any question.
 */

export const THEME_KEY = 'mercury.theme';

/** `default` is the look the app was built with (`PUBLIC_MERCURY_THEME`). */
export const THEME_CHOICES = ['default', 'neutral', 'demo'] as const;
export type ThemeChoice = (typeof THEME_CHOICES)[number];

export function isThemeChoice(value: unknown): value is ThemeChoice {
	return (THEME_CHOICES as readonly unknown[]).includes(value);
}

/** Storage can be missing, full or refused (a private window), and the app works without it. */
function storage(): Storage | undefined {
	try {
		return globalThis.localStorage;
	} catch {
		return undefined;
	}
}

/** The saved choice, or `default` when there is none or it can't be read. */
export function loadTheme(): ThemeChoice {
	try {
		const saved = storage()?.getItem(THEME_KEY);
		return isThemeChoice(saved) ? saved : 'default';
	} catch {
		return 'default';
	}
}

/** Remembers the choice; `default` forgets it. A failure to save is not worth telling anyone. */
export function saveTheme(choice: ThemeChoice): void {
	try {
		if (choice === 'default') storage()?.removeItem(THEME_KEY);
		else storage()?.setItem(THEME_KEY, choice);
	} catch {
		// the look still changes for this visit
	}
}

/** The root element's attribute the tokens in `layout.css` key on. */
interface Root {
	dataset: { theme?: string };
}

let built: string | undefined;

/**
 * Puts the look on the page. `default` gives back the one the page was built with, which was on the
 * root element before anything here ran.
 */
export function applyTheme(choice: ThemeChoice, root: Root = document.documentElement): void {
	built ??= root.dataset.theme ?? '';
	root.dataset.theme = choice === 'default' ? built : choice;
}

/** For tests: forget the built look, as a fresh page would. */
export function forgetBuiltTheme(): void {
	built = undefined;
}

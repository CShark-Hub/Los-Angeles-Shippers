import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';

import CopyCommand, { copyKeys, toClipboard } from './CopyCommand.svelte';

const COMMAND = 'user grant --email dana.reyes@example.com --projects delta';

/** The command's box as the server renders it: its markup, and the words a person reads. */
function box(props: { command: string; label?: string }) {
	const { body } = render(CopyCommand, { props });
	const words = body
		.replace(/<!--[\s\S]*?-->/g, '')
		.replace(/<[^>]+>/g, ' ')
		.replace(/\s+/g, ' ')
		.trim();
	return { body, words };
}

describe('a command to copy', () => {
	it('shows the command whole, its label, and a button named for what it copies', () => {
		const { body, words } = box({ command: COMMAND, label: 'Grant access' });
		expect(words).toContain('Grant access');
		expect(words).toContain(COMMAND);
		expect(body).toContain(`aria-label="Copy: ${COMMAND}"`);
		// the admin shell's prompt is decoration: a screen reader doesn't read it before the command
		expect(body).toMatch(/aria-hidden="true"[^>]*>mercury(&gt;|>)<\/span>/);
	});

	it('has its polite status on the page before a press, saying nothing yet', () => {
		const { body, words } = box({ command: COMMAND });
		const status = body.match(/<p role="status" aria-live="polite"[^>]*>([\s\S]*?)<\/p>/);
		expect(status).not.toBeNull();
		expect(status?.[1].replace(/<!--[\s\S]*?-->/g, '').trim()).toBe('');
		expect(words).not.toContain('Copied');
		expect(words).not.toContain('press');
	});
});

describe('copying', () => {
	it('copies the command when the browser has a clipboard', async () => {
		const writeText = vi.fn<(text: string) => Promise<void>>(() => Promise.resolve());
		expect(await toClipboard(COMMAND, { writeText })).toBe(true);
		expect(writeText).toHaveBeenCalledWith(COMMAND);
	});

	it('says it could not when there is no clipboard or the browser refuses', async () => {
		expect(await toClipboard(COMMAND, undefined)).toBe(false);
		const writeText = () => Promise.reject(new DOMException('Write permission denied.'));
		expect(await toClipboard(COMMAND, { writeText })).toBe(false);
	});

	it("names the keys that copy by hand on the person's computer", () => {
		expect(
			copyKeys('Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6) AppleWebKit/605.1.15 Safari/605.1.15')
		).toBe('⌘C');
		expect(
			copyKeys(
				'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/140.0 Safari/537.36'
			)
		).toBe('Ctrl+C');
		expect(copyKeys('Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0')).toBe(
			'Ctrl+C'
		);
	});
});

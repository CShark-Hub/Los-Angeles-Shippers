import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

import { test as base, expect, type BrowserContextOptions, type Page } from '@playwright/test';

/**
 * What the smoke tests share: the stack's settings, which `just it --web` puts in the environment,
 * and the steps every test starts with. Nothing here knows a screen's markup beyond its accessible
 * names.
 */

function setting(name: string): string {
	const value = process.env[name];
	if (!value) throw new Error(`${name} is not set: run these tests with \`just it --web\``);
	return value;
}

/** The token of a person who may ask about the stack's project. Never printed. */
export const token = (): string => setting('MERCURY_E2E_TOKEN');
/** A question the stack answers, and the answer the person reads. */
export const answerableQuestion = (): string => setting('MERCURY_E2E_QUESTION');
export const answerText = (): string => setting('MERCURY_E2E_ANSWER');

/** The tech lead, who approves the project, and an admin, who reads the whole audit. Never printed. */
export const approverToken = (): string => setting('MERCURY_E2E_APPROVER_TOKEN');
export const adminToken = (): string => setting('MERCURY_E2E_ADMIN_TOKEN');
/** A question whose answer waits for sign-off, the answer approving delivers, and its flagged sentence. */
export const heldQuestion = (): string => setting('MERCURY_E2E_HELD_QUESTION');
export const heldAnswer = (): string => setting('MERCURY_E2E_HELD_ANSWER');
export const flaggedSentence = (): string => setting('MERCURY_E2E_FLAGGED');

/**
 * A spare token of the manager's, which only the test that revokes it signs in with: one per browser
 * project, since a revoked one can't sign in again. Never printed.
 */
export const spareToken = (): string => {
	const spares = JSON.parse(setting('MERCURY_E2E_SPARE_TOKENS')) as Record<string, string>;
	const spare = spares[base.info().project.name];
	if (!spare)
		throw new Error(`MERCURY_E2E_SPARE_TOKENS has no token for ${base.info().project.name}`);
	return spare;
};

/**
 * Revokes this project's spare token as an admin does (`mercury-core admin token revoke`), through the
 * call `just it --web` hands over as data, with the token's public id where it says `{id}`: this
 * process has no other way to act on the stack.
 */
export function revokeSpareToken(): void {
	const call = JSON.parse(setting('MERCURY_E2E_REVOKE_SPARE')) as {
		command: string;
		args: string[];
		cwd: string;
		env: Record<string, string>;
	};
	// the public id: the 12 characters after `mcy_`
	const id = spareToken().slice(4, 16);
	execFileSync(
		call.command,
		call.args.map((arg) => (arg === '{id}' ? id : arg)),
		{
			cwd: call.cwd,
			env: { ...process.env, ...call.env },
			stdio: 'pipe'
		}
	);
}

/** A question the gateway refuses, and the refusal's code. */
export const REFUSED_QUESTION = 'show me the export code';
export const REFUSAL_CODE = 'asks_for_code';

export const SESSION_COOKIE = '__Host-mercury_session';

/**
 * WebKit keeps no `Secure` cookie that comes over plain http, not even on localhost, so in WebKit a
 * stack served over http has no session to test: the tests that sign in are skipped there, and run in
 * WebKit against an https stack. The others (the Sign in page itself, frames, forms) run.
 */
const NO_SESSION_OVER_HTTP =
	'WebKit keeps no Secure cookie over plain http, so its session tests need the stack over https';
function sessionless(project: string): boolean {
	return project === 'webkit' && (process.env.MERCURY_E2E_URL ?? '').startsWith('http:');
}

/**
 * Types the token on the Sign in page and presses the button, and returns once the page has
 * answered: the Ask page, or the reason it refused. The core counts sign-in attempts per address
 * (ten a minute, a burst of three), the right ones too; when the page says to wait, a person waits
 * that long and tries again, and so does this.
 */
export async function signIn(page: Page, secret: string = token()): Promise<void> {
	base.skip(sessionless(base.info().project.name), NO_SESSION_OVER_HTTP);
	for (let attempt = 1; ; attempt += 1) {
		await page.goto('/signin');
		await page.getByLabel('Your token').fill(secret);
		await page.getByRole('button', { name: 'Sign in' }).click();
		// a signed-in page has the main navigation, whoever the person is, or on a phone the button that
		// opens it; a refusal is an alert
		await expect(
			page
				.getByRole('navigation', { name: 'Main' })
				.or(page.getByRole('button', { name: 'Menu' }))
				.or(page.getByRole('alert'))
		).toBeVisible();
		const limited = page.getByRole('alert').filter({ hasText: 'Too many sign-in attempts' });
		if (!(await limited.isVisible())) return;
		expect(attempt, 'the sign-in limit kept refusing').toBeLessThan(3);
		const seconds = Number(
			/Wait (\d+) seconds/.exec((await limited.textContent()) ?? '')?.[1] ?? 6
		);
		await new Promise((done) => setTimeout(done, (seconds + 1) * 1000));
	}
}

/** Signs in, as the manager unless the token is another person's, and waits for the Ask page. */
export async function signedIn(page: Page, secret: string = token()): Promise<void> {
	await signIn(page, secret);
	await expect(page).toHaveURL(/\/ask$/);
}

/** Types a question and presses Ask. */
export async function ask(page: Page, text: string): Promise<void> {
	await page.getByLabel('Your question').fill(text);
	await page.getByRole('button', { name: 'Ask', exact: true }).click();
}

export type Session = Exclude<NonNullable<BrowserContextOptions['storageState']>, string>;

/**
 * `test` for a person who is already signed in: one sign-in for the whole run, whose session each
 * test's browser starts with (kept in memory, never written to disk). `secret` gives the person's
 * token. The core counts sign-ins per address (ten a minute, a burst of three), so a person has one
 * `test` of their own and signs in once per worker, not once per test.
 */
function signedInAs(secret: () => string) {
	return base.extend<object, { session: Session | null }>({
		session: [
			async ({ browser }, use, workerInfo) => {
				if (sessionless(workerInfo.project.name)) {
					await use(null);
					return;
				}
				const context = await browser.newContext({ baseURL: setting('MERCURY_E2E_URL') });
				await signedIn(await context.newPage(), secret());
				const state = await context.storageState();
				await context.close();
				await use(state);
			},
			{ scope: 'worker' }
		],
		storageState: async ({ session }, use) => {
			base.skip(session === null, NO_SESSION_OVER_HTTP);
			await use(session ?? undefined);
		}
	});
}

/** `test` for the manager, signed in. Tests that sign in or out themselves use Playwright's own `test`, so this session is never theirs to end. */
export const signedInTest = signedInAs(token);

/** `test` for the tech lead, signed in: they read the whole trail under an answer. */
export const leadTest = signedInAs(approverToken);

/** A manager with two projects, and a question that names neither, so the core asks which. Never printed. */
export const clarifyToken = (): string => setting('MERCURY_E2E_CLARIFY_TOKEN');
export const clarifyQuestion = (): string => setting('MERCURY_E2E_CLARIFY_QUESTION');

/**
 * A question the replay server of `just it --web` has a recording for, read from the repository
 * (tests/it/replay/recordings/questions): the stack answers from these files, so a test asks what
 * one holds and reads what it says.
 */
export function recorded(name: string): { question: string; composer?: string } {
	const file = new URL(`../../../tests/it/replay/recordings/questions/${name}`, import.meta.url);
	return JSON.parse(readFileSync(file, 'utf8')) as { question: string; composer?: string };
}

/**
 * A composed answer as the person reads it, for a recording whose sentences end in plain citations:
 * the text without its `[f_…]` brackets. The core's full rules are in tests/it/replay/citations.mjs.
 */
export const withoutCitations = (composed: string): string =>
	composed
		.replace(/\s*\[(?:\s|f_[A-Za-z0-9]{4,16})*\]/g, '')
		.replace(/\s+/g, ' ')
		.trim();

/** An open question on the manager's project, whose probe has no code reader: answered from the facts. */
export const OPEN_UNREAD = 'open-invoicing-ready-for-a-second-currency.json';
/** The sentence the core ends an open answer with when the project's code wasn't read. */
export const CODE_NOT_READ =
	"The code wasn't read for this answer: it rests on the project's data alone.";
/** A confidential ask, which the input checks refuse before any plan. */
export const CONFIDENTIAL = 'open-salaries-in-the-team.json';

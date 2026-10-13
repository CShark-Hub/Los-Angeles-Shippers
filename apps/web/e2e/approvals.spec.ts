import { expect, test, type Browser, type Page } from '@playwright/test';
import {
	approverToken,
	ask,
	flaggedSentence,
	heldAnswer,
	heldQuestion,
	signIn,
	token
} from './support';

/** A window of its own for one person, signed in: its own cookie, so two people can act at once. */
async function personOn(browser: Browser, secret: string): Promise<Page> {
	const context = await browser.newContext({ baseURL: process.env.MERCURY_E2E_URL });
	const page = await context.newPage();
	await signIn(page, secret);
	return page;
}

test.describe('Signing off a held answer', () => {
	test.describe('Given a project manager whose answer waits for an approver', () => {
		test.describe('When the tech lead approves it in a second window', () => {
			test("Then the project manager's Ask page shows the answer exactly as the tech lead saw it", async ({
				browser
			}) => {
				test.setTimeout(120_000);
				const asker = await personOn(browser, token());
				await asker.goto('/ask');
				await ask(asker, heldQuestion());
				await expect(
					asker.getByRole('status').filter({ hasText: 'Waiting for an approver' })
				).toBeVisible();

				// a second window, the approver's, beside the manager's on the same screen
				const lead = await personOn(browser, approverToken());
				await lead
					.getByRole('navigation', { name: 'Main' })
					.getByRole('link', { name: /^Approvals/ })
					.click();
				// the newest of the held answers with this text (an earlier browser's may still wait), by its id
				const newest = lead.getByRole('article').filter({ hasText: heldAnswer() }).last();
				await expect(newest).toBeVisible();
				const held = lead.locator(
					`article[aria-labelledby="${await newest.getAttribute('aria-labelledby')}"]`
				);
				await expect(held).toBeVisible();
				await expect(held.locator('mark')).toHaveText(flaggedSentence());
				await expect(held).toContainText('sentence_contains_code_or_identifier');

				await held.getByRole('button', { name: 'Approve' }).click();
				await expect(lead.getByRole('status').filter({ hasText: 'Approved' })).toBeVisible();
				await expect(held).toHaveCount(0);

				// the Ask page asks every 5 seconds while it waits
				const card = asker.getByRole('region', { name: 'Answer' });
				await expect(card).toContainText(heldAnswer(), { timeout: 30_000 });

				// a reload replays the stream past the hold: answered, never waiting
				await asker.reload();
				await expect(asker.getByRole('region', { name: 'Answer' })).toContainText(heldAnswer());
				await expect(
					asker.getByRole('status').filter({ hasText: 'Waiting for an approver' })
				).toHaveCount(0);
			});
		});

		test.describe('When the project manager, who approves the project too, looks for it on their own Approvals screen', () => {
			test('Then it waits for another approver, with nothing of its draft, no flag and no button to approve it', async ({
				browser
			}) => {
				const asker = await personOn(browser, token());
				await asker.goto('/ask');
				await ask(asker, heldQuestion());
				await expect(
					asker.getByRole('status').filter({ hasText: 'Waiting for an approver' })
				).toBeVisible();

				await asker.getByRole('link', { name: /^Approvals/ }).click();
				const own = asker
					.getByRole('article')
					.filter({ hasText: 'Your own question waits for another approver of the project.' });
				await expect(own.first()).toBeVisible();
				await expect(own.first().getByRole('button', { name: 'Approve' })).toHaveCount(0);
				await expect(own.first().getByLabel(/reject it/)).toHaveCount(0);

				// the draft is the approver's to read, never the asker's before the decision: none of the
				// manager's own held answers shows its text, a mark or a flag
				await expect(own.filter({ hasText: heldAnswer() })).toHaveCount(0);
				await expect(own.filter({ hasText: flaggedSentence() })).toHaveCount(0);
				await expect(own.filter({ hasText: 'sentence_contains_code_or_identifier' })).toHaveCount(
					0
				);
				await expect(own.locator('mark')).toHaveCount(0);
				await expect(own.getByRole('region', { name: 'The answer' })).toHaveCount(0);
			});
		});
	});
});

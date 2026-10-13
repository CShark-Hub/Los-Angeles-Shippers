import { expect } from '@playwright/test';
import { answerText, answerableQuestion, ask, signedInTest as test } from './support';

const TRAIL = 'What Mercury did with the question';

test.describe('Asking a question', () => {
	test.describe('Given a signed-in person on the Ask page', () => {
		test.beforeEach(async ({ page }) => {
			await page.goto('/ask');
			await expect(page.getByLabel('Project')).toBeVisible();
		});

		test.describe('When they ask a question the project can answer', () => {
			test.beforeEach(async ({ page }) => {
				await ask(page, answerableQuestion());
			});

			test('Then the answer card shows the answer the gateway delivered', async ({ page }) => {
				const card = page.getByRole('region', { name: 'Answer' });
				await expect(card).toContainText(answerText());
				await expect(card.getByRole('heading', { name: 'What it rests on' })).toBeVisible();
				await expect(card).toContainText('Confidence');
			});

			test('Then the project manager sees one row per stage under the answer, each with its verdict', async ({
				page
			}) => {
				await expect(page.getByRole('region', { name: 'Answer' })).toContainText(answerText());
				// open under the answer, with nothing to click: one row for each stage, all of them passed
				const stages = page.getByRole('list', { name: 'How each stage went' });
				await expect(stages).toBeVisible();
				const rows = stages.getByRole('listitem');
				await expect(rows).toHaveCount(4);
				const names = ['The question', 'The lookup', 'The data', 'The answer'];
				for (const [index, name] of names.entries()) {
					await expect(rows.nth(index)).toContainText(name);
					await expect(rows.nth(index)).toContainText('pass');
				}
				await expect(rows.nth(3)).toContainText('sentences kept');
				// the whole trail is the tech lead's and the admin's
				await expect(page.getByRole('list', { name: TRAIL })).toHaveCount(0);
			});
		});
	});
});

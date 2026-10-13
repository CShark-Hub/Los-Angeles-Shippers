import { expect } from '@playwright/test';
import {
	ask,
	CONFIDENTIAL,
	recorded,
	REFUSAL_CODE,
	REFUSED_QUESTION,
	signedInTest as test
} from './support';

test.describe('A question the gateway refuses', () => {
	test.describe('Given a signed-in person on the Ask page', () => {
		test.beforeEach(async ({ page }) => {
			await page.goto('/ask');
			await expect(page.getByLabel('Project')).toBeVisible();
		});

		test.describe('When they ask for code', () => {
			test.beforeEach(async ({ page }) => {
				await ask(page, REFUSED_QUESTION);
			});

			test('Then the page refuses with a reason and a hint, and shows no answer', async ({
				page
			}) => {
				const refusal = page.getByRole('status').filter({ hasText: "Mercury can't answer that" });
				await expect(refusal).toBeVisible();
				await expect(refusal).toContainText(`Reason: ${REFUSAL_CODE}`);
				await expect(page.getByRole('region', { name: 'Answer' })).toBeHidden();
			});

			test('Then the short view shows the question blocked and the other stages not reached', async ({
				page
			}) => {
				await expect(page.getByRole('status').filter({ hasText: REFUSAL_CODE })).toBeVisible();
				const rows = page.getByRole('list', { name: 'How each stage went' }).getByRole('listitem');
				await expect(rows).toHaveCount(4);
				await expect(rows.nth(0)).toContainText('The question');
				await expect(rows.nth(0)).toContainText('blocked');
				for (const [index, name] of ['The lookup', 'The data', 'The answer'].entries()) {
					await expect(rows.nth(index + 1)).toContainText(name);
					await expect(rows.nth(index + 1)).toContainText('not reached');
				}
				// the whole trail is the tech lead's and the admin's
				await expect(
					page.getByRole('list', { name: 'What Mercury did with the question' })
				).toHaveCount(0);
			});
		});

		test.describe('When they ask about something confidential', () => {
			test.beforeEach(async ({ page }) => {
				await ask(page, recorded(CONFIDENTIAL).question);
			});

			test('Then the page refuses with what stays out of it and what to ask instead, and shows no answer', async ({
				page
			}) => {
				const refusal = page.getByRole('status').filter({ hasText: "Mercury can't answer that" });
				await expect(refusal).toBeVisible();
				await expect(refusal).toContainText("Mercury doesn't answer about confidential matters.");
				await expect(refusal).toContainText(
					/Contracts, prices, salaries, personal data, a client's internal information and security settings stay out of it: ask about the work in .+ instead\./
				);
				await expect(refusal).toContainText('Reason: confidential');
				await expect(page.getByRole('region', { name: 'Answer' })).toBeHidden();
			});

			test('Then the short view shows the question blocked before any lookup', async ({ page }) => {
				await expect(page.getByRole('status').filter({ hasText: 'confidential' })).toBeVisible();
				const rows = page.getByRole('list', { name: 'How each stage went' }).getByRole('listitem');
				await expect(rows).toHaveCount(4);
				await expect(rows.nth(0)).toContainText('The question');
				await expect(rows.nth(0)).toContainText('blocked');
				for (const [index, name] of ['The lookup', 'The data', 'The answer'].entries()) {
					await expect(rows.nth(index + 1)).toContainText(name);
					await expect(rows.nth(index + 1)).toContainText('not reached');
				}
			});
		});
	});
});

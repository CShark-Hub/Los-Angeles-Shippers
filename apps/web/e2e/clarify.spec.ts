import { expect, test } from '@playwright/test';
import { ask, clarifyQuestion, clarifyToken, signIn } from './support';

test.describe('A question that names no project, from a person with two', () => {
	test.describe('Given a signed-in manager of two projects on the Ask page, with no project picked', () => {
		test.beforeEach(async ({ page }) => {
			await signIn(page, clarifyToken());
			await page.goto('/ask');
			await expect(page.getByLabel('Project')).toHaveValue('');
		});

		test.describe('When they ask, and pick a project when the page asks which', () => {
			test('Then the answer comes from the project picked, and no answer shows before the pick', async ({
				page
			}) => {
				await ask(page, clarifyQuestion());
				const which = page.getByRole('status').filter({ hasText: 'Which project do you mean?' });
				await expect(which).toBeVisible();
				await expect(which.getByRole('button', { name: 'Delta — invoicing' })).toBeVisible();
				await expect(which.getByRole('button', { name: 'Ember — reports' })).toBeVisible();
				const answer = page.getByRole('region', { name: 'Answer' });
				await expect(answer).toBeHidden();

				await which.getByRole('button', { name: 'Delta — invoicing' }).click();
				await expect(answer).toBeVisible();
				await expect(page.locator('#conversation')).toContainText('About Delta — invoicing');
				await expect(page).toHaveURL(/\/ask\/[^/]+$/);
			});
		});
	});
});

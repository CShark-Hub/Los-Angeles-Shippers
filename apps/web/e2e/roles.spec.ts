import { expect } from '@playwright/test';
import { answerText, answerableQuestion, ask, leadTest as test } from './support';

const TRAIL = 'What Mercury did with the question';

test.describe('The trail under an answer, by role', () => {
	test.describe('Given a signed-in tech lead on the Ask page', () => {
		test.beforeEach(async ({ page }) => {
			await page.goto('/ask');
			await expect(page.getByLabel('Project')).toBeVisible();
		});

		test.describe('When they ask a question the project can answer', () => {
			test.beforeEach(async ({ page }) => {
				await ask(page, answerableQuestion());
			});

			test('Then the whole trail is folded under the answer, from the question to Done, and there is no short view', async ({
				page
			}) => {
				await expect(page.getByRole('region', { name: 'Answer' })).toContainText(answerText());
				const trail = page.getByRole('list', { name: TRAIL });
				// the answer comes first: the trail waits, closed, until it is asked for
				await expect(trail).toBeHidden();
				await page.getByText('How Mercury checked this').click();
				await expect(trail).toContainText('Question received');
				await expect(trail).toContainText('Kind of question');
				await expect(trail).toContainText('Answer checked');
				await expect(trail).toContainText('Answer ready');
				await expect(trail).toContainText('Done');
				await expect(page.getByRole('list', { name: 'How each stage went' })).toHaveCount(0);
			});
		});
	});
});

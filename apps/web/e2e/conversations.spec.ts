import { expect, type Page } from '@playwright/test';
import {
	REFUSAL_CODE,
	REFUSED_QUESTION,
	answerText,
	answerableQuestion,
	ask,
	signedInTest as test
} from './support';

/** The side panel's list of the conversations of this tab. */
const conversations = (page: Page) => page.getByRole('navigation', { name: 'Your conversations' });

/** Starts a new conversation from the side panel and asks the question the gateway refuses in it. */
async function askInANewConversation(page: Page): Promise<void> {
	await conversations(page).getByRole('button', { name: 'New conversation' }).click();
	await ask(page, REFUSED_QUESTION);
	await expect(page.getByRole('status').filter({ hasText: REFUSAL_CODE })).toBeVisible();
}

test.describe('Going back to a conversation from the side panel', () => {
	test.describe('Given a signed-in person who asked in one conversation, then in another', () => {
		test.beforeEach(async ({ page }) => {
			await page.goto('/ask');
			await expect(page.getByLabel('Project')).toBeVisible();
			await ask(page, answerableQuestion());
			await expect(page.getByRole('region', { name: 'Answer' })).toContainText(answerText());
			await askInANewConversation(page);
		});

		test.describe('When they open the first from the list, start a new one, ask in it and click the first again', () => {
			test('Then the first conversation opens each time, with its question and its answer', async ({
				page
			}) => {
				const first = conversations(page).getByRole('link', { name: answerableQuestion() });
				const answer = page.getByRole('region', { name: 'Answer' });
				const refusal = page.getByRole('status').filter({ hasText: REFUSAL_CODE });

				await first.click();
				await expect(answer).toContainText(answerText());
				await expect(refusal).toBeHidden();
				await expect(first).toHaveAttribute('aria-current', 'page');
				// the address names the question on screen, as a reload needs
				await expect(page).toHaveURL(/\/ask\/[^/]+$/);
				const address = page.url();

				await askInANewConversation(page);
				await expect(first).not.toHaveAttribute('aria-current', 'page');
				await expect(page).not.toHaveURL(address);

				// the router last went to this very address: the click still brings the conversation back
				await first.click();
				await expect(answer).toContainText(answerText());
				await expect(page.locator('#conversation')).toContainText(answerableQuestion());
				await expect(refusal).toBeHidden();
				await expect(first).toHaveAttribute('aria-current', 'page');
				await expect(page).toHaveURL(address);
			});
		});

		test.describe('When they load the first question by its address, then load /ask', () => {
			test('Then the address opens that question with its answer, and /ask opens a new conversation', async ({
				page
			}) => {
				const first = conversations(page).getByRole('link', { name: answerableQuestion() });
				const answer = page.getByRole('region', { name: 'Answer' });
				await first.click();
				// the first conversation on screen and its address in the bar: the second's was there before
				await expect(answer).toContainText(answerText());
				await expect(first).toHaveAttribute('aria-current', 'page');
				await expect(page).toHaveURL(/\/ask\/[^/]+$/);
				const address = page.url();

				// a load of the address itself, as a reload or a typed address does: the tab's memory starts
				// empty, so the question comes back by its address with its answer, not its words
				await page.goto(address);
				await expect(answer).toContainText(answerText());
				await expect(page).toHaveURL(address);

				await page.goto('/ask');
				await expect(page.getByLabel('Project')).toBeVisible();
				await expect(answer).toBeHidden();
				await expect(page).toHaveURL(/\/ask$/);
			});
		});
	});
});

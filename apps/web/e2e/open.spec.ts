import { expect } from '@playwright/test';
import {
	ask,
	CODE_NOT_READ,
	OPEN_UNREAD,
	recorded,
	signedInTest as test,
	withoutCitations
} from './support';

const TRAIL = 'What Mercury did with the question';

test.describe('An open question, about the project but none of the five kinds', () => {
	test.describe('Given a signed-in person on the Ask page of a project without a code reader', () => {
		test.beforeEach(async ({ page }) => {
			await page.goto('/ask');
			await expect(page.getByLabel('Project')).toBeVisible();
		});

		test.describe('When they ask an open question', () => {
			const open = recorded(OPEN_UNREAD);

			test.beforeEach(async ({ page }) => {
				await ask(page, open.question);
			});

			test("Then the answer card shows the answer from the project's data, with the note that the code wasn't read", async ({
				page
			}) => {
				const card = page.getByRole('region', { name: 'Answer' });
				await expect(card).toContainText(
					`${withoutCitations(open.composer ?? '')} ${CODE_NOT_READ}`
				);
				await expect(card.getByRole('heading', { name: 'What it rests on' })).toBeVisible();
				// no part of it came from the code reader, so it carries no mark saying so
				await expect(card.getByText('Read from the code')).toHaveCount(0);
			});

			test('Then the project manager sees one row per stage, the lookup naming the five kinds and no code reader', async ({
				page
			}) => {
				await expect(page.getByRole('region', { name: 'Answer' })).toContainText(CODE_NOT_READ);
				const rows = page.getByRole('list', { name: 'How each stage went' }).getByRole('listitem');
				await expect(rows).toHaveCount(4);
				const names = ['The question', 'The lookup', 'The data', 'The answer'];
				for (const [index, name] of names.entries()) {
					await expect(rows.nth(index)).toContainText(name);
					await expect(rows.nth(index)).toContainText('pass');
				}
				await expect(rows.nth(1)).toContainText(
					'progress · tests · blockers · build health · changes'
				);
				await expect(rows.nth(1)).not.toContainText('the code reader');
				await expect(rows.nth(3)).toContainText('sentences kept');
				// the whole trail is the tech lead's and the admin's
				await expect(page.getByRole('list', { name: TRAIL })).toHaveCount(0);
			});
		});
	});
});

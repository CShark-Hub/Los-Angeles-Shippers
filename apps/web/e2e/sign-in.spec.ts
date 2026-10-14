import { expect, test } from '@playwright/test';
import { signIn, token } from './support';

test.describe('Signing in to the web client', () => {
	test.describe('Given a visitor with no session', () => {
		test.describe('When they open the client', () => {
			test.beforeEach(async ({ page }) => {
				await page.goto('/');
			});

			test('Then they are taken to Sign in', async ({ page }) => {
				await expect(page).toHaveURL(/\/signin$/);
				await expect(page.getByRole('heading', { name: 'Sign in to Mercury' })).toBeVisible();
			});
		});

		test.describe('When they paste a token that is not valid', () => {
			test.beforeEach(async ({ page }) => {
				await signIn(page, 'mcy_this_token_does_not_exist');
			});

			test('Then the page says so in plain words, stays on Sign in and forgets the token', async ({
				page
			}) => {
				await expect(page.getByRole('alert')).toContainText("That token isn't valid");
				await expect(page).toHaveURL(/\/signin$/);
				await expect(page.getByLabel('Your token')).toHaveValue('');
			});
		});

		test.describe('When they paste their token', () => {
			test.beforeEach(async ({ page }) => {
				await signIn(page);
			});

			test('Then they land on Ask with their project to choose, and the token is nowhere in the page', async ({
				page
			}) => {
				await expect(page).toHaveURL(/\/ask$/);
				await expect(page.getByLabel('Project')).toBeVisible();
				await expect(page.getByLabel('Your question')).toBeVisible();
				expect((await page.content()).includes(token())).toBe(false);
				expect(page.url().includes(token())).toBe(false);
			});
		});
	});
});

import { expect, test, type Page } from '@playwright/test';
import { adminToken, signIn, token } from './support';

/** The admin panel's group in the side panel; on a phone the menu opens it first. */
async function openAdminLink(page: Page, name: string): Promise<void> {
	const menu = page.getByRole('button', { name: 'Menu' });
	if (await menu.isVisible()) await menu.click();
	await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name }).click();
}

test.describe('The admin panel', () => {
	test.describe('Given a project manager', () => {
		test.describe('When they look for the panel in the side panel, then open it by its address', () => {
			test('Then there is no Administration group, the page says only an admin sees it, and nothing is read', async ({
				page
			}) => {
				const reads: string[] = [];
				page.on('request', (request) => {
					if (new URL(request.url()).pathname.startsWith('/v1/admin/')) reads.push(request.url());
				});
				await signIn(page, token());
				const main = page.getByRole('navigation', { name: 'Main' });
				await expect(main.getByRole('link', { name: 'Ask' })).toBeVisible();
				await expect(main.getByRole('group', { name: 'Administration' })).toHaveCount(0);
				await expect(main.getByRole('link', { name: 'People & access' })).toHaveCount(0);

				for (const path of ['/admin/overview', '/admin/people', '/admin/repositories']) {
					await page.goto(path);
					await expect(
						page.getByRole('status').filter({ hasText: 'Only an admin sees the admin panel' })
					).toBeVisible();
				}
				expect(reads, "the manager's pages ask the core for nothing of the panel").toEqual([]);
			});
		});

		test.describe('When their browser asks the core for the panel itself', () => {
			test('Then the core refuses each read with a 403', async ({ page }) => {
				await signIn(page, token());
				for (const path of ['/v1/admin/overview', '/v1/admin/people', '/v1/admin/repositories']) {
					const status = await page.evaluate(
						async (url) => (await fetch(url, { headers: { 'X-Mercury-Client': 'web' } })).status,
						path
					);
					expect(status, path).toBe(403);
				}
			});
		});
	});

	test.describe('Given an admin', () => {
		test.beforeEach(async ({ page }) => {
			await signIn(page, adminToken());
		});

		test.describe('When they open the Overview', () => {
			test('Then the key figures, the chart and the alerts are shown, and another window reads again', async ({
				page
			}) => {
				await openAdminLink(page, 'Overview');
				await expect(page.getByRole('heading', { name: 'Overview', level: 1 })).toBeVisible();
				await expect(page.getByRole('region', { name: 'Key figures' })).toBeVisible();
				await expect(page.getByRole('heading', { name: 'Questions over time' })).toBeVisible();
				await expect(page.getByRole('heading', { name: 'Needs attention' })).toBeVisible();

				const read = page.waitForResponse(
					(response) =>
						response.url().includes('/v1/admin/overview?window=30d') && response.status() === 200
				);
				// the radio itself is visually hidden under its label, so click the label, as a person does
				await page.getByRole('radiogroup', { name: 'Window' }).getByText('30 days').click();
				await read;
				await expect(page.getByRole('radio', { name: '30 days' })).toBeChecked();
				await expect(page.getByText('What Mercury did in the last 30 days')).toBeVisible();
			});
		});

		test.describe('When they open People & access', () => {
			test('Then every person is listed with their projects and tokens, and no token secret is sent', async ({
				page
			}) => {
				const body = page.waitForResponse(
					(response) => response.url().endsWith('/v1/admin/people') && response.status() === 200
				);
				await openAdminLink(page, 'People & access');
				const text = await (await body).text();
				expect(text, 'the people body holds no token secret').not.toContain('mcy_');

				await expect(
					page.getByRole('heading', { name: 'People & access', level: 1 })
				).toBeVisible();
				const rows = page.getByRole('table').getByRole('row');
				await expect(rows.nth(2)).toBeVisible();
				await expect(page.getByRole('heading', { name: 'Tokens' })).toBeVisible();
				await expect(page.getByText(/^(mercury>)?\s*access list --email /).first()).toBeVisible();
			});
		});

		test.describe('When they open Repositories', () => {
			test("Then the stack's project is shown with its probe and the people who may ask", async ({
				page
			}) => {
				await openAdminLink(page, 'Repositories');
				await expect(page.getByRole('heading', { name: 'Repositories', level: 1 })).toBeVisible();
				const delta = page.getByRole('article').filter({ hasText: 'delta' }).first();
				await expect(delta).toBeVisible();
				await expect(delta.getByRole('region', { name: 'Who may ask' })).toBeVisible();
				await expect(delta).toContainText('access list --project delta');
			});
		});
	});
});

import { readFile } from 'node:fs/promises';

import { expect, test } from '@playwright/test';
import { adminToken, signIn, token } from './support';

test.describe('Reading the audit', () => {
	test.describe('Given a project manager', () => {
		test.describe('When they look for the audit in the side panel, then open it by its address', () => {
			test('Then there is no Audit link, the page says only an admin reads it, and no row is asked for', async ({
				page
			}) => {
				const audit: string[] = [];
				page.on('request', (request) => {
					if (new URL(request.url()).pathname.startsWith('/v1/audit')) audit.push(request.url());
				});
				await signIn(page, token());
				const main = page.getByRole('navigation', { name: 'Main' });
				await expect(main.getByRole('link', { name: 'Ask' })).toBeVisible();
				await expect(main.getByRole('link', { name: 'Audit' })).toHaveCount(0);

				await page.goto('/audit');
				await expect(page.getByRole('heading', { name: 'Audit', level: 1 })).toBeVisible();
				await expect(
					page.getByRole('status').filter({ hasText: 'Only an admin reads the audit' })
				).toBeVisible();
				await expect(page.getByRole('list', { name: 'Audit rows' })).toHaveCount(0);
				await expect(
					page.getByRole('link', { name: 'Export every matching row (JSON lines)' })
				).toHaveCount(0);
				await expect(
					page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Audit' })
				).toHaveCount(0);
				expect(audit, "the manager's page asks the core for no audit row").toEqual([]);
			});
		});
	});

	test.describe('Given an admin on the Audit screen', () => {
		test.beforeEach(async ({ page }) => {
			await signIn(page, adminToken());
			await page.getByRole('link', { name: 'Audit' }).click();
			await expect(page.getByRole('heading', { name: 'Audit', level: 1 })).toBeVisible();
		});

		test('Then the rows come with the status of their chain', async ({ page }) => {
			await expect(
				page.getByRole('status').filter({ hasText: 'The chain holds for these rows' })
			).toBeVisible();
			await expect(
				page.getByRole('list', { name: 'Audit rows' }).getByRole('listitem').first()
			).toBeVisible();
		});

		test.describe('When they narrow the rows to the refused questions', () => {
			test('Then only rows of refused questions are listed, and the chain still holds', async ({
				page
			}) => {
				await page.getByLabel('How it ended').selectOption('refused');
				await page.getByRole('button', { name: 'Show rows' }).click();
				const rows = page.getByRole('list', { name: 'Audit rows' }).getByRole('listitem');
				await expect(rows.first()).toBeVisible();
				await expect(
					page.getByRole('status').filter({ hasText: 'The chain holds for these rows' })
				).toBeVisible();
				await expect(page.getByRole('list', { name: 'Audit rows' })).toContainText('refused');
			});
		});

		test.describe('When they export the rows', () => {
			test('Then a file of JSON lines is saved, each row with the hashes that verify it', async ({
				page
			}) => {
				const [download] = await Promise.all([
					page.waitForEvent('download'),
					page.getByRole('link', { name: 'Export every matching row (JSON lines)' }).click()
				]);
				const path = await download.path();
				const lines = (await readFile(path, 'utf8')).trim().split('\n');
				expect(lines.length).toBeGreaterThan(0);
				for (const line of lines) {
					const row = JSON.parse(line) as { seq: number; hash: string; prev_hash: string };
					expect(row.hash).toMatch(/^[0-9a-f]{64}$/);
					expect(row.prev_hash).toMatch(/^[0-9a-f]{64}$/);
				}
			});
		});

		test.describe('When their session is gone by the time they export', () => {
			test('Then nothing is downloaded, and they are at Sign in with the reason', async ({
				page,
				context
			}) => {
				await expect(
					page.getByRole('status').filter({ hasText: 'The chain holds for these rows' })
				).toBeVisible();
				const downloads: string[] = [];
				page.on('download', (download) => downloads.push(download.suggestedFilename()));
				// the browser forgets the session cookie: the core sees no session on the next request
				await context.clearCookies();

				await page.getByRole('link', { name: 'Export every matching row (JSON lines)' }).click();
				await expect(page).toHaveURL(/\/signin$/);
				await expect(
					page.getByRole('status').filter({ hasText: 'Your session ended. Sign in again.' })
				).toBeVisible();
				expect(downloads, 'a refused export saves no file').toEqual([]);
			});
		});
	});
});

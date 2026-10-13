import { expect, test } from '@playwright/test';
import { SESSION_COOKIE, signedIn } from './support';

test.describe('Disconnecting', () => {
	test.describe('Given a signed-in person', () => {
		/** The session cookie as a request sends it back, taken before they disconnect. */
		let oldCookie = '';

		test.beforeEach(async ({ page, context }) => {
			await signedIn(page);
			const session = (await context.cookies()).find((c) => c.name === SESSION_COOKIE);
			expect(session).toBeDefined();
			oldCookie = `${session!.name}=${session!.value}`;
		});

		test.describe('When they press Disconnect', () => {
			test.beforeEach(async ({ page }) => {
				const ended = page.waitForResponse(
					(r) => r.url().endsWith('/v1/session') && r.request().method() === 'DELETE'
				);
				await page.getByRole('button', { name: 'Disconnect' }).click();
				expect((await ended).status()).toBe(204);
			});

			test('Then they are back at Sign in, and the cookie they had no longer works', async ({
				page,
				context,
				request
			}) => {
				await expect(page).toHaveURL(/\/signin$/);
				await expect(page.getByRole('heading', { name: 'Sign in to Mercury' })).toBeVisible();
				expect((await context.cookies()).filter((c) => c.name === SESSION_COOKIE)).toEqual([]);
				const me = await request.get('/v1/me', { headers: { cookie: oldCookie } });
				expect(me.status()).toBe(401);
			});
		});
	});
});

import { expect, test } from '@playwright/test';
import { approverToken, heldAnswer, heldQuestion, signIn, token } from './support';

/** What the page asked the browser to show: the browser's own Notification, with each one kept. */
declare global {
	interface Window {
		shown?: { title: string; body: string | undefined; tag: string | undefined }[];
	}
}

/** How long the Approvals list waits between two looks at the core, and some slack. */
const POLL = 30_000;

test.describe('The sign-off notification', () => {
	test.describe('Given an approver who allowed notifications, with the Approvals list open', () => {
		test.describe('When a project manager asks a question whose answer waits for them', () => {
			test('Then the browser shows one notification, which names no draft', async ({
				browser,
				browserName,
				playwright
			}) => {
				test.skip(browserName === 'webkit', 'WebKit has no web notifications to grant');
				test.setTimeout(3 * POLL + 30_000);

				const context = await browser.newContext({
					baseURL: process.env.MERCURY_E2E_URL,
					permissions: ['notifications']
				});
				// the browser's Notification as it is, with every notification it is asked for kept
				await context.addInitScript(() => {
					const Real = window.Notification;
					window.shown = [];
					class Kept extends Real {
						constructor(title: string, options?: NotificationOptions) {
							super(title, options);
							window.shown?.push({ title, body: options?.body, tag: options?.tag });
						}
					}
					Object.defineProperty(Kept, 'permission', { get: () => Real.permission });
					Object.defineProperty(Kept, 'requestPermission', {
						value: (...args: Parameters<typeof Real.requestPermission>) =>
							Real.requestPermission(...args)
					});
					window.Notification = Kept as typeof Notification;
				});
				const lead = await context.newPage();
				await signIn(lead, approverToken());
				await lead.goto('/approvals');
				await expect(lead.getByRole('heading', { name: 'Approvals', level: 1 })).toBeVisible();
				expect(await lead.evaluate(() => Notification.permission)).toBe('granted');
				// the first list is what waited already: nothing to tell
				await lead.waitForTimeout(1_000);
				expect(await lead.evaluate(() => window.shown?.length)).toBe(0);

				// the manager asks through the API, as their CLI would
				const api = await playwright.request.newContext({ baseURL: process.env.MERCURY_E2E_URL });
				const asked = await api.post('/v1/questions', {
					headers: {
						authorization: `Bearer ${token()}`,
						'idempotency-key': `notification-${browserName}-${Date.now()}`,
						'x-mercury-client': 'cli'
					},
					data: { text: heldQuestion(), project: 'delta' }
				});
				expect(asked.status()).toBe(202);
				const { id: question } = (await asked.json()) as { id: string };

				try {
					// the next look at the core finds it and tells once
					await expect
						.poll(() => lead.evaluate(() => window.shown?.length ?? 0), { timeout: POLL + 15_000 })
						.toBe(1);
					const [note] = await lead.evaluate(() => window.shown ?? []);
					expect(note).toMatchObject({
						title: 'Mercury',
						body: 'An answer waits for your sign-off.',
						tag: 'mercury-approvals'
					});
					expect(JSON.stringify(note)).not.toContain(heldAnswer());
					// and the list shows the draft, while the next look tells nothing new
					await expect(
						lead.getByRole('article').filter({ hasText: heldAnswer() }).first()
					).toBeVisible({
						timeout: POLL
					});
					await lead.waitForTimeout(POLL + 5_000);
					expect(await lead.evaluate(() => window.shown?.length)).toBe(1);
				} finally {
					await context.close();
					// the held answer rejected, so the next browser's tests start from an empty list
					const asApprover = {
						authorization: `Bearer ${approverToken()}`,
						'x-mercury-client': 'cli'
					};
					const listed = await api.get('/v1/approvals?status=pending', { headers: asApprover });
					const { approvals } = (await listed.json()) as {
						approvals: { id: string; question_id: string }[];
					};
					for (const held of approvals.filter((a) => a.question_id === question)) {
						const decided = await api.post(`/v1/approvals/${held.id}`, {
							headers: asApprover,
							data: { decision: 'reject', note: 'Cleared after the notification test.' }
						});
						expect(decided.ok()).toBe(true);
					}
					await api.dispose();
				}
			});
		});
	});
});

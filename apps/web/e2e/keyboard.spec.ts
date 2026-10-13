import { expect, test, type Browser, type Page } from '@playwright/test';
import {
	REFUSAL_CODE,
	REFUSED_QUESTION,
	answerText,
	answerableQuestion,
	approverToken,
	heldAnswer,
	heldQuestion,
	signIn,
	token
} from './support';

/** A page of its own for one person, signed in: its own cookie, so two people can act at once. */
async function personOn(browser: Browser, secret: string): Promise<Page> {
	const context = await browser.newContext({ baseURL: process.env.MERCURY_E2E_URL });
	const page = await context.newPage();
	await signIn(page, secret);
	return page;
}

/** What the focus looks like on the element that has it: the outline the keyboard follows. */
async function outlineOfFocus(page: Page): Promise<{ style: string; width: number }> {
	return page.evaluate(() => {
		const focused = getComputedStyle(document.activeElement as Element);
		return { style: focused.outlineStyle, width: parseFloat(focused.outlineWidth) };
	});
}

/** What the focus looks like on the composer whose box has it: the box itself draws none. */
async function outlineOfComposer(page: Page): Promise<{ style: string; width: number }> {
	return page.evaluate(() => {
		const form = (document.activeElement as Element).closest('form') as Element;
		const style = getComputedStyle(form);
		return { style: style.outlineStyle, width: parseFloat(style.outlineWidth) };
	});
}

/** Presses Shift+Tab until the focus is on `target`, at most `limit` times. */
async function shiftTabTo(
	page: Page,
	target: ReturnType<Page['locator']>,
	limit = 12
): Promise<void> {
	for (let presses = 0; presses < limit; presses += 1) {
		await page.keyboard.press('Shift+Tab');
		if (await target.evaluate((element) => element === document.activeElement)) return;
	}
	throw new Error(`the focus did not reach the control in ${limit} presses of Shift+Tab`);
}
/** Presses Tab until the focus is on `target`, at most `limit` times: a person's way to a control. */
async function tabTo(page: Page, target: ReturnType<Page['locator']>, limit = 12): Promise<void> {
	for (let presses = 0; presses < limit; presses += 1) {
		await page.keyboard.press('Tab');
		if (await target.evaluate((element) => element === document.activeElement)) return;
	}
	throw new Error(`the focus did not reach the control in ${limit} presses of Tab`);
}

test.describe('Working Mercury from the keyboard alone', () => {
	test.describe('Given a signed-in person on the Ask page', () => {
		test.describe('When they ask, read, ask again and start a new conversation without the mouse', () => {
			test('Then every step is a key, the focus is always seen and it follows the answer', async ({
				browser
			}) => {
				const page = await personOn(browser, token());
				await page.goto('/ask');
				await expect(page.getByLabel('Project')).toBeVisible();

				// the first stop is the way past the side panel, and it is seen
				await page.keyboard.press('Tab');
				const skip = page.getByRole('link', { name: 'Skip to the page' });
				await expect(skip).toBeFocused();
				expect((await outlineOfFocus(page)).style).toBe('solid');
				await page.keyboard.press('Enter');
				await expect(page.locator('#main')).toBeFocused();

				// to the box; the whole composer shows the focus; Enter asks
				const box = page.getByLabel('Your question');
				await tabTo(page, box);
				expect(await outlineOfComposer(page)).toEqual({ style: 'solid', width: 3 });
				await page.keyboard.type(answerableQuestion());
				await page.keyboard.press('Enter');

				// the box is off while it runs; the focus is not lost, it ends on the answer
				const answer = page.getByRole('heading', { name: 'Answer', exact: true });
				await expect(answer).toBeFocused();
				await expect(page.getByRole('region', { name: 'Answer' })).toContainText(answerText());

				// the short view under the answer holds nothing to operate: the next Tab is the box again
				await page.keyboard.press('Tab');
				await expect(box).toBeFocused();

				// New conversation is in the side panel, back past the box; Enter leaves the box ready
				const fresh = page.getByRole('button', { name: 'New conversation' });
				await shiftTabTo(page, fresh);
				expect((await outlineOfFocus(page)).style).toBe('solid');
				await page.keyboard.press('Enter');
				await expect(box).toBeFocused();
				await expect(box).toHaveValue('');
				// a refusal is where the focus goes too
				await page.keyboard.type(REFUSED_QUESTION);
				await page.keyboard.press('Enter');
				await expect(
					page.getByRole('heading', { name: "Mercury can't answer that", exact: true })
				).toBeFocused();
				await expect(page.getByRole('status').filter({ hasText: REFUSAL_CODE })).toBeVisible();
				await page.context().close();
			});
		});
	});

	test.describe('Given an approver and an answer that waits for them', () => {
		test.describe('When they open Approvals, approve one and reject another with the keyboard', () => {
			test('Then each decision is made with keys and the focus lands on what came of it', async ({
				browser
			}) => {
				test.setTimeout(180_000);
				const asker = await personOn(browser, token());
				await asker.goto('/ask');
				for (const question of [heldQuestion(), heldQuestion()]) {
					await asker.getByLabel('Your question').fill(question);
					await asker.getByRole('button', { name: 'Ask', exact: true }).click();
					await expect(
						asker.getByRole('status').filter({ hasText: 'Waiting for an approver' })
					).toBeVisible();
					await asker.getByRole('button', { name: 'New conversation' }).click();
				}

				const lead = await personOn(browser, approverToken());
				await lead.goto('/ask');
				await expect(lead.getByLabel('Project')).toBeVisible();
				// to the Approvals link by Tab, and open it with Enter
				const link = lead.getByRole('link', { name: /^Approvals/ });
				await tabTo(lead, link);
				expect((await outlineOfFocus(lead)).style).toBe('solid');
				await lead.keyboard.press('Enter');
				await expect(lead.getByRole('heading', { name: 'Approvals', exact: true })).toBeVisible();

				// the first held answer: Tab to Approve, Enter
				const held = lead.getByRole('article').filter({ hasText: heldAnswer() });
				await expect(held.first()).toBeVisible();
				const approve = held.first().getByRole('button', { name: 'Approve' });
				await tabTo(lead, approve, 20);
				await lead.keyboard.press('Enter');
				// its card and the button are gone with it: the focus is on the outcome
				await expect(lead.getByRole('heading', { name: 'Approved', exact: true })).toBeFocused();

				// the next one: the note by Tab, typed, and the button after it
				const next = lead.getByRole('article').filter({ hasText: heldAnswer() }).first();
				await expect(next).toBeVisible();
				const note = next.getByLabel('Or reject it, with a note for the person who asked');
				await tabTo(lead, note, 20);
				await lead.keyboard.type('Please say what is still open.');
				await lead.keyboard.press('Tab');
				await expect(next.getByRole('button', { name: 'Reject with this note' })).toBeFocused();
				await lead.keyboard.press('Space');
				await expect(lead.getByRole('heading', { name: 'Rejected', exact: true })).toBeFocused();
				await asker.context().close();
				await lead.context().close();
			});
		});
	});
});

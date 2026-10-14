import { describe, expect, it, vi } from 'vitest';
import { ApiError, type Approval, type Me, type Transport } from '$lib/api';
import { Approvals, held, isApprover, noteProblem, type Notifier } from './approvals.svelte';

/** A held answer as the core lists it: the person's own question comes without draft and flags. */
function item(id: string, mine = false): Approval {
	const base: Approval = {
		id,
		question_id: 'q_01JB2M3N4P5Q6R7S8T9V0W1X2D',
		project: 'delta',
		class: 'progress',
		asker_role: 'pm',
		reason: 'review_band',
		asked_by_me: mine,
		status: 'pending',
		created_at: '2026-10-06T09:00:00Z',
		expires_at: '2026-10-07T09:00:00Z'
	};
	return mine
		? base
		: { ...base, draft: { text: 'Export is done.', facts: [], confidence: 0.8 }, flags: [] };
}

/** A core whose list is `waiting`, and which records every decision. */
function fakeCore(initial: Approval[]) {
	const posts: { path: string; body: unknown }[] = [];
	let waiting = initial;
	let refuse: ApiError | null = null;
	const transport: Transport = {
		async get<T>() {
			return { approvals: waiting } as T;
		},
		async post<T>(path: string, body: unknown) {
			posts.push({ path, body });
			if (refuse) throw refuse;
			return undefined as T;
		},
		async delete<T>() {
			return undefined as T;
		},
		events: () => () => {}
	};
	return {
		transport,
		posts,
		wait(next: Approval[]) {
			waiting = next;
		},
		refuseWith(error: ApiError | null) {
			refuse = error;
		}
	};
}

function notifier(): Notifier & { shown: number[] } {
	const shown: number[] = [];
	return {
		shown,
		permission: () => 'granted',
		request: async () => 'granted',
		show: (count) => shown.push(count)
	};
}

function on(core: ReturnType<typeof fakeCore>, told = notifier()) {
	return new Approvals({ api: async () => core.transport, notifier: told, pollMs: 30_000 });
}

describe('the answers waiting for sign-off', () => {
	it("is an approver's screen only", () => {
		const me = (approver: boolean): Me => ({
			user: {
				id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X2Y',
				email: 'dana.reyes@example.com',
				display_name: 'Dana Reyes',
				role: 'pm'
			},
			projects: [{ project: 'delta', approver }],
			token: { public_id: 'mockTokn0001', expires_at: '2026-11-01T00:00:00Z' }
		});
		expect(isApprover(me(true))).toBe(true);
		expect(isApprover(me(false))).toBe(false);
		expect(isApprover(null)).toBe(false);
	});

	it("keeps the person's own question as the core sends it, without draft or flags: the asker reads nothing of it before the decision", async () => {
		const own = item('ap_2', true);
		expect(own).not.toHaveProperty('draft');
		expect(own).not.toHaveProperty('flags');
		const list = on(fakeCore([item('ap_1'), own]));
		await list.refresh();
		const [theirs, mine] = list.items;
		// someone else's question: the approver reads the draft they decide on
		expect(theirs.draft?.text).toBe('Export is done.');
		// their own: what it is and why it waits, nothing of a draft
		expect(mine).toMatchObject({ id: 'ap_2', asked_by_me: true, draft: null, flags: [] });
		expect(mine.reason).toBe('review_band');

		expect(held(item('ap_1'))).toEqual(item('ap_1'));
		expect(held(own)).toEqual({ ...own, draft: null, flags: [] });
	});

	it("drops a draft and flags that a list brings with the person's own question anyway", async () => {
		const own: Approval = {
			...item('ap_2', true),
			draft: { text: 'The SEPA export is still a draft.', facts: [], confidence: 0.6 },
			flags: [
				{
					sentence: 'The SEPA export is still a draft.',
					check: 'sentence_contains_code_or_identifier',
					p: 0.45
				}
			]
		};
		const list = on(fakeCore([item('ap_1'), own]));
		await list.refresh();
		expect(list.items[1]).toMatchObject({ id: 'ap_2', draft: null, flags: [] });
		const kept = JSON.stringify(list.items);
		expect(kept).not.toContain('SEPA');
		expect(kept).not.toContain('sentence_contains_code_or_identifier');
	});

	it("keeps someone else's item that came without its draft with none, so nothing is approved unread", () => {
		const bare = item('ap_1');
		delete bare.draft;
		delete bare.flags;
		expect(held(bare)).toEqual({ ...bare, draft: null, flags: [] });
	});

	it('counts only what the person can decide on: their own questions wait for someone else', async () => {
		const list = on(fakeCore([item('ap_1'), item('ap_2', true)]));
		await list.refresh();
		expect(list.items.map((a) => a.id)).toEqual(['ap_1', 'ap_2']);
		expect(list.decidable.map((a) => a.id)).toEqual(['ap_1']);
	});

	it('asks again every 30 seconds while it runs, and not once it stopped', async () => {
		vi.useFakeTimers();
		try {
			const core = fakeCore([]);
			const get = vi.spyOn(core.transport, 'get');
			const list = on(core);
			list.start();
			list.start();
			await vi.advanceTimersByTimeAsync(60_000);
			expect(get).toHaveBeenCalledTimes(3);
			expect(get).toHaveBeenLastCalledWith('/v1/approvals?status=pending');
			list.stop();
			await vi.advanceTimersByTimeAsync(60_000);
			expect(get).toHaveBeenCalledTimes(3);
		} finally {
			vi.useRealTimers();
		}
	});

	it("tells the browser once of answers that arrive after the first list, never of the person's own", async () => {
		const core = fakeCore([item('ap_1')]);
		const told = notifier();
		const list = on(core, told);
		await list.refresh();
		expect(told.shown).toEqual([]);
		core.wait([item('ap_1'), item('ap_2'), item('ap_3', true)]);
		await list.refresh();
		await list.refresh();
		expect(told.shown).toEqual([1]);
	});

	it('approves without a note and takes the answer off the list', async () => {
		const core = fakeCore([item('ap_1'), item('ap_2')]);
		const list = on(core);
		await list.refresh();
		await list.decide('ap_1', 'approve');
		expect(core.posts).toEqual([{ path: '/v1/approvals/ap_1', body: { decision: 'approve' } }]);
		expect(list.items.map((a) => a.id)).toEqual(['ap_2']);
		expect(list.outcome).toEqual({ id: 'ap_1', decision: 'approve' });
	});

	it('rejects only with a note on one line, sent trimmed', async () => {
		const core = fakeCore([item('ap_1')]);
		const list = on(core);
		await list.refresh();
		for (const note of ['', '   ', 'x'.repeat(501), 'two\nlines']) {
			await list.decide('ap_1', 'reject', note);
			expect(list.decisionFailure?.id).toBe('ap_1');
		}
		expect(core.posts).toEqual([]);
		await list.decide('ap_1', 'reject', '  Ask the tech lead first.  ');
		expect(core.posts).toEqual([
			{
				path: '/v1/approvals/ap_1',
				body: { decision: 'reject', note: 'Ask the tech lead first.' }
			}
		]);
		expect(list.decisionFailure).toBeNull();
	});

	it('says why a decision was refused and shows what is left', async () => {
		const core = fakeCore([item('ap_1')]);
		const list = on(core);
		await list.refresh();
		core.refuseWith(
			new ApiError(
				409,
				'invalid_request',
				'The answer has already been decided on.',
				'Refresh the pending list.'
			)
		);
		core.wait([]);
		await list.decide('ap_1', 'approve');
		expect(list.decisionFailure).toEqual({
			id: 'ap_1',
			failure: {
				message: 'The answer has already been decided on.',
				hint: 'Refresh the pending list.'
			}
		});
		expect(list.items).toEqual([]);
		expect(list.outcome).toBeNull();

		// the card is gone, the reason stays until the next list or decision
		await list.refresh();
		expect(list.decisionFailure).toBeNull();
	});

	it('refuses a note with any control character, as the core does, C1 ones included', async () => {
		expect(noteProblem('Ask the lead\u0085first.')).toBe('Write the note on one line.');
		expect(noteProblem('Ask the lead\u009ffirst.')).toBe('Write the note on one line.');
		expect(noteProblem('Ask the lead first.')).toBeNull();
	});

	it("words a note's problems for the person", () => {
		expect(noteProblem('Fine.')).toBeNull();
		expect(noteProblem('')).toMatch(/asker/);
	});
});

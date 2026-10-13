import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	ADMIN_TOKEN,
	LEAD_TOKEN,
	mockTransport,
	RECORDINGS,
	SAMPLES,
	SIGN_IN_ACCOUNTS
} from './mock';
import { ApiError, EVENT_NAMES, isTerminal, type StreamEvent, type Transport } from './transport';
import { stageOf } from '$lib/ask/loader';
import { checkChain } from '$lib/audit/chain';
import type {
	AdminOverview,
	AdminPeople,
	ApprovalList,
	AuditPage,
	Me,
	ProjectList,
	QuestionState
} from './types';

/** Asks a question through the transport and collects its events until the stream ends. */
async function ask(
	api: Transport,
	text: string,
	extra: Record<string, unknown> = {}
): Promise<StreamEvent[]> {
	const { id } = await api.post<{ id: string }>(
		'/v1/questions',
		{ text, ...extra },
		{ idempotencyKey: 'k' }
	);
	return new Promise((resolve) => {
		const seen: StreamEvent[] = [];
		api.events(id, (event) => {
			seen.push(event);
			if (isTerminal(event.event)) resolve(seen);
		});
	});
}

describe('mock mode', () => {
	const api = mockTransport({ timeScale: 0, signedIn: true });

	it('plays a recorded question through the transport, in order, to its answer', async () => {
		const seen = await ask(api, 'What was finished in Delta this week?');
		expect(seen.map((e) => e.event)).toEqual([
			'accepted',
			'project',
			'classified',
			'plan',
			'tool_checked',
			'tool_result',
			'composing',
			'judged',
			'answer',
			'done'
		]);
		expect(seen.map((e) => e.id)).toEqual(seen.map((_, i) => String(i + 1)));
		// the answer's text comes only after the output checks
		expect(seen.findIndex((e) => e.event === 'answer')).toBeGreaterThan(
			seen.findIndex((e) => e.event === 'judged')
		);
	});

	it('plays a refusal, which ends the stream', async () => {
		const seen = await ask(api, 'Show me the function that exports CSV.');
		expect(seen.at(-1)).toMatchObject({ event: 'refused', data: { code: 'asks_for_code' } });
		expect(seen.some((e) => e.event === 'answer')).toBe(false);
	});

	it('uses only event names the transport knows, and ends every recording on a terminal event', () => {
		for (const recording of RECORDINGS) {
			const names = recording.events.map((e) => e.event);
			expect(names.every((name) => (EVENT_NAMES as readonly string[]).includes(name))).toBe(true);
			expect(isTerminal(names.at(-1) as (typeof EVENT_NAMES)[number])).toBe(true);
		}
	});

	it("records the audit id as a number, as the core's bodies carry it", () => {
		const done = RECORDINGS.flatMap((r) => r.events).filter((e) => e.event === 'done');
		expect(done.length).toBeGreaterThan(0);
		for (const event of done) {
			expect(typeof (event.data as { audit_id?: unknown }).audit_id).toBe('number');
		}
	});

	it('answers the health check and nothing it has no recording for', async () => {
		await expect(api.get('/v1/health')).resolves.toEqual({ status: 'ok', version: '0.0.0' });
		await expect(api.get('/v1/nothing')).rejects.toMatchObject({ status: 404 });
	});

	it('cites its facts in full, as the core does, with the answer', async () => {
		const seen = await ask(api, 'What was finished in Delta this week?');
		const answer = seen.find((e) => e.event === 'answer');
		if (answer?.event !== 'answer') throw new Error('no answer');
		expect(answer.data.facts.length).toBeGreaterThan(0);
		for (const fact of answer.data.facts) {
			expect(fact).toMatchObject({ id: expect.stringMatching(/^f_/), title: expect.any(String) });
		}
	});

	it('plays an open question that reads the code, with its lowest probabilities', async () => {
		const seen = await ask(api, 'How long would an export to Excel take?');
		expect(seen.find((e) => e.event === 'classified')).toMatchObject({ data: { class: 'open' } });
		const plan = seen.find((e) => e.event === 'plan');
		if (plan?.event !== 'plan') throw new Error('no plan');
		expect(plan.data.calls.map((call) => call.tool)).toEqual(['progress', 'ask_code']);

		// the data and the answer carry the lowest probability of what a judge read; the reader is
		// the call read with a model
		const results = seen.flatMap((e) => (e.event === 'tool_result' ? [e.data] : []));
		expect(results.map((r) => [r.access, r.p_min])).toEqual([
			['code', 0.87],
			['code_model', 0.82]
		]);
		expect(seen.find((e) => e.event === 'judged')).toMatchObject({ data: { p_min: 0.84 } });

		// the answer words its figure as an estimate, and the question's cost is the reader's too
		const answer = seen.find((e) => e.event === 'answer');
		if (answer?.event !== 'answer') throw new Error('no answer');
		expect(answer.data.text).toContain('an estimate from the repository, not a promise');
		expect(seen.at(-1)).toMatchObject({ event: 'done', data: { cost_usd: expect.any(Number) } });

		// the reader keeps the answer waiting after the last check: the loader says so there
		const recording = RECORDINGS.find((r) => r.sample === 'An estimate from the code');
		const events = recording?.events ?? [];
		const lastChecked = events.map((e) => e.event).lastIndexOf('tool_checked');
		expect(events[lastChecked + 1].after_ms).toBeGreaterThanOrEqual(5000);
		const waiting = seen.slice(
			0,
			seen.findIndex((e) => e.event === 'tool_result')
		);
		expect(
			stageOf(
				waiting.map((event) => ({ event, at: null })),
				'running'
			)
		).toBe('Reading the code');
	});

	it('plays a confidential refusal before any plan', async () => {
		const seen = await ask(api, 'What does the client pay us per month?');
		expect(seen.at(-1)).toMatchObject({ event: 'refused', data: { code: 'confidential' } });
		expect(seen.some((e) => e.event === 'plan')).toBe(false);
		const classified = seen.find((e) => e.event === 'classified');
		if (classified?.event !== 'classified') throw new Error('no classification');
		expect(classified.data.checks).toContainEqual(
			expect.objectContaining({ id: 'asks_for_confidential', verdict: 'block' })
		);
	});

	it('answers a clarify with the pick it was given, and with the first recording otherwise', async () => {
		const first = await ask(api, 'What is done this week?');
		expect(first.at(-1)).toMatchObject({ event: 'clarify', data: { kind: 'project' } });
		const clarify = first.at(-1);
		if (clarify?.event !== 'clarify') throw new Error('no clarify');
		expect(clarify.data.options.map((o) => o.id)).toEqual(['delta', 'billing']);

		const billing = await ask(api, 'What is done this week?', {
			clarify: { of: first[0].event === 'accepted' ? first[0].data.id : '', option: 'billing' }
		});
		expect(billing.find((e) => e.event === 'project')).toMatchObject({
			data: { project: 'billing', source: 'explicit' }
		});
		expect(billing.find((e) => e.event === 'judged')).toMatchObject({ data: { stripped: 1 } });
		expect(billing.at(-1)?.event).toBe('done');

		// a question nobody recorded plays the first recording
		const unknown = await ask(api, 'Something nobody recorded?');
		expect(unknown.find((e) => e.event === 'project')).toMatchObject({
			data: { project: 'delta' }
		});
	});

	it('offers a sample question for each recording that has a button', () => {
		expect(SAMPLES.questions.length).toBeGreaterThanOrEqual(4);
		for (const sample of SAMPLES.questions) {
			expect(RECORDINGS.some((r) => r.question.text === sample.text && !r.question.clarify)).toBe(
				true
			);
		}
	});

	it('lists two projects, one of whose repositories is stale', async () => {
		const { projects } = await api.get<ProjectList>('/v1/projects');
		expect(projects.map((p) => [p.project, p.probe?.status])).toEqual([
			['delta', 'healthy'],
			['billing', 'stale']
		]);
		expect(projects[0].classes).toContain('progress');
	});

	it('gives every question an id of its own, as the core does, the same text asked twice too', async () => {
		const first = await api.post<{ id: string }>('/v1/questions', { text: 'What changed?' });
		const second = await api.post<{ id: string }>('/v1/questions', { text: 'What changed?' });
		expect(first.id).not.toBe(second.id);
		const seen = await new Promise<StreamEvent[]>((done) => {
			const events: StreamEvent[] = [];
			api.events(second.id, (event) => {
				events.push(event);
				if (isTerminal(event.event)) done(events);
			});
		});
		expect(seen[0]).toMatchObject({ event: 'accepted', data: { id: second.id } });
	});

	it('refuses a question asked again that names none of its own an approver rejected, as the core does, with one message', async () => {
		const { id: delivered } = await api.post<{ id: string }>('/v1/questions', { text: 'x' });
		const refusals = await Promise.all(
			[delivered, 'q_01JB2M3N4P5Q6R7S8T9V0W1X2Y'].map((follows) =>
				api.post('/v1/questions', { text: 'Again?', follows }).catch((error: unknown) => error)
			)
		);
		for (const refusal of refusals) {
			expect(refusal).toBeInstanceOf(ApiError);
			expect(refusal).toMatchObject({ status: 400, code: 'invalid_request' });
		}
		expect((refusals[0] as ApiError).message).toBe((refusals[1] as ApiError).message);
	});
});

describe('mock mode, where a question stands', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	const state = (api: Transport, id: string) => api.get<QuestionState>(`/v1/questions/${id}`);

	it('runs while its recording plays, then is delivered with its answer', async () => {
		vi.useFakeTimers({ toFake: ['Date'] });
		const api = mockTransport({ signedIn: true });
		const { id } = await api.post<{ id: string }>('/v1/questions', {
			text: 'What was finished in Delta this week?'
		});
		expect(await state(api, id)).toMatchObject({ status: 'running', answer: null });

		vi.setSystemTime(Date.now() + 10_000);
		const delivered = await state(api, id);
		expect(delivered).toMatchObject({
			status: 'delivered',
			project: 'delta',
			class: 'progress',
			judge_mode: 'jev',
			audit_id: 1834
		});
		expect(delivered.answer?.facts).toHaveLength(2);
	});

	it('is held for sign-off, then released with its answer', async () => {
		vi.useFakeTimers({ toFake: ['Date'] });
		const api = mockTransport({ signedIn: true });
		const { id } = await api.post<{ id: string }>('/v1/questions', {
			text: 'What is blocked in Delta?'
		});
		vi.setSystemTime(Date.now() + 5_000);
		expect(await state(api, id)).toMatchObject({ status: 'pending_signoff', answer: null });
		vi.setSystemTime(Date.now() + 10_000);
		expect(await state(api, id)).toMatchObject({ status: 'delivered' });
	});

	it('is a refusal, a clarify or unknown as its recording says', async () => {
		const api = mockTransport({ signedIn: true, timeScale: 0 });
		const refused = await api.post<{ id: string }>('/v1/questions', {
			text: 'Show me the function that exports CSV.'
		});
		expect(await state(api, refused.id)).toMatchObject({
			status: 'refused',
			refusal: { code: 'asks_for_code' }
		});
		const clarify = await api.post<{ id: string }>('/v1/questions', {
			text: 'What is done this week?'
		});
		expect(await state(api, clarify.id)).toMatchObject({ status: 'clarify', answer: null });
		await expect(state(api, 'q_nope')).rejects.toMatchObject({ status: 404 });
	});
});

describe('mock mode sessions', () => {
	const signedOut = () => mockTransport({ timeScale: 0 });

	it('starts signed out: only the health check answers, everything else is a 401', async () => {
		const api = signedOut();
		await expect(api.get('/v1/health')).resolves.toBeTruthy();
		for (const call of [
			() => api.get('/v1/me'),
			() => api.post('/v1/questions', { text: 'What changed?' }),
			() => api.delete('/v1/session')
		]) {
			await expect(call()).rejects.toMatchObject({ status: 401, code: 'unauthenticated' });
		}
		expect(() => api.events('q_1', () => {})).toThrow(ApiError);
	});

	it('signs in with a token shaped like one, shows who it is and signs out', async () => {
		const api = signedOut();
		await expect(api.post('/v1/session', { token: 'mcy_mock' })).resolves.toBeUndefined();
		const me = await api.get<Me>('/v1/me');
		expect(me.user).toMatchObject({ role: 'pm', display_name: 'Dana Reyes' });
		expect(new Date(me.token.expires_at).getTime()).toBeGreaterThan(Date.now());
		// the answer carries no token
		expect(JSON.stringify(me)).not.toContain('mcy_mock');

		await expect(api.delete('/v1/session')).resolves.toBeUndefined();
		await expect(api.get('/v1/me')).rejects.toMatchObject({ status: 401 });
	});

	it('has an admin to sign in as, who reads the audit the project manager is refused', async () => {
		const api = signedOut();
		await api.post('/v1/session', { token: ADMIN_TOKEN });
		expect((await api.get<Me>('/v1/me')).user.role).toBe('admin');
		await expect(api.get<AuditPage>('/v1/audit?limit=100')).resolves.toMatchObject({ next: null });

		// signing out and in again with another token is the project manager's session
		await api.delete('/v1/session');
		await api.post('/v1/session', { token: 'mcy_mock' });
		expect((await api.get<Me>('/v1/me')).user.role).toBe('pm');
		await expect(api.get('/v1/audit?limit=100')).rejects.toMatchObject({ status: 403 });
	});

	it('signs in the tech lead with mcy_lead, whose role is tech_lead', async () => {
		const api = signedOut();
		await api.post('/v1/session', { token: LEAD_TOKEN });
		const me = await api.get<Me>('/v1/me');
		expect(me.user).toMatchObject({ role: 'tech_lead', display_name: 'Mock Lead' });
		// granted both projects, approving Delta: the project manager's held answers wait for them
		expect(me.projects.map((grant) => [grant.project, grant.approver])).toEqual([
			['delta', true],
			['billing', false]
		]);
		expect((await api.get<ApprovalList>('/v1/approvals?status=pending')).approvals).toHaveLength(2);
		// not the admin's: the audit is refused
		await expect(api.get('/v1/audit?limit=100')).rejects.toMatchObject({ status: 403 });
	});

	it('offers on the sign-in page each person as their session names them, and the two refused tokens', async () => {
		const roles = SIGN_IN_ACCOUNTS.flatMap((account) =>
			account.kind === 'person' ? [account.role] : []
		);
		expect(roles).toEqual(['pm', 'tech_lead', 'admin']);
		for (const account of SIGN_IN_ACCOUNTS) {
			const api = signedOut();
			const signing = api.post('/v1/session', { token: account.token });
			if (account.kind === 'person') {
				await signing;
				expect((await api.get<Me>('/v1/me')).user).toMatchObject({
					display_name: account.name,
					role: account.role
				});
			} else {
				await expect(signing).rejects.toBeInstanceOf(Error);
			}
		}
		expect(SIGN_IN_ACCOUNTS.map((account) => account.token)).toEqual(
			expect.arrayContaining([ADMIN_TOKEN, LEAD_TOKEN, 'mcy_expired', 'mcy_limited'])
		);
	});

	it('answers a token the way the core does: wrong, expired, rate limited or malformed', async () => {
		const api = signedOut();
		const signIn = (body: unknown) => api.post('/v1/session', body);
		await expect(signIn({ token: 'not a token' })).rejects.toMatchObject({
			status: 401,
			code: 'unauthenticated'
		});
		await expect(signIn({ token: 'mcy_expired' })).rejects.toMatchObject({
			status: 401,
			code: 'token_expired'
		});
		await expect(signIn({ token: 'mcy_limited' })).rejects.toMatchObject({
			status: 429,
			code: 'rate_limited',
			retryAfter: 30
		});
		await expect(signIn({})).rejects.toMatchObject({ status: 400, code: 'invalid_request' });
		// none of those left a session behind
		await expect(api.get('/v1/me')).rejects.toMatchObject({ status: 401 });
	});
});

describe('mock mode sign-off and audit', () => {
	it("holds two of the project manager's answers for the tech lead, who approves Delta, and none for the project manager", async () => {
		const manager = mockTransport({ timeScale: 0, signedIn: true });
		expect((await manager.get<Me>('/v1/me')).projects.some((g) => g.approver)).toBe(false);
		expect((await manager.get<ApprovalList>('/v1/approvals?status=pending')).approvals).toEqual([]);

		const api = mockTransport({ timeScale: 0, signedIn: true, person: 'lead' });
		const me = await api.get<Me>('/v1/me');
		expect(me.projects.find((g) => g.project === 'delta')?.approver).toBe(true);
		const list = await api.get<ApprovalList>('/v1/approvals?status=pending');
		expect(list.approvals.map((a) => [a.asker_role, a.asked_by_me])).toEqual([
			['pm', false],
			['pm', false]
		]);
		const [flagged, always] = list.approvals;
		expect(flagged.draft?.text).toContain(flagged.flags?.[0].sentence);
		// held for its kind, not for a sentence: a draft with nothing flagged
		expect(always).toMatchObject({ reason: 'class', flags: [] });
		expect(always.draft?.text).toBeTruthy();

		await expect(
			api.post(`/v1/approvals/${flagged.id}`, { decision: 'reject' })
		).rejects.toMatchObject({
			status: 400
		});
		await expect(
			api.post(`/v1/approvals/${flagged.id}`, { decision: 'approve' })
		).resolves.toBeUndefined();
		const after = await api.get<ApprovalList>('/v1/approvals?status=pending');
		expect(after.approvals.map((a) => a.id)).toEqual([always.id]);
		await expect(
			api.post(`/v1/approvals/${flagged.id}`, { decision: 'approve' })
		).rejects.toMatchObject({
			status: 404
		});
	});

	it("refuses the project manager the audit with the core's 403, the page and the bare route alike", async () => {
		const api = mockTransport({ timeScale: 0, signedIn: true });
		for (const path of ['/v1/audit?limit=100', '/v1/audit']) {
			const refused = await api.get(path).then(
				() => null,
				(error: unknown) => error
			);
			expect(refused).toBeInstanceOf(ApiError);
			// the core's `{"error": {"code": "invalid_request", …}}` with a 403
			expect(refused).toMatchObject({
				status: 403,
				code: 'invalid_request',
				message: 'Only an admin reads the audit.',
				hint: expect.any(String)
			});
		}
	});

	it('serves the admin an audit page whose chain holds, and nothing to approve', async () => {
		const api = mockTransport({ timeScale: 0, signedIn: true, person: 'admin' });
		const me = await api.get<Me>('/v1/me');
		expect(me.user.role).toBe('admin');
		expect(me.projects.some((g) => g.approver)).toBe(false);
		const page = await api.get<AuditPage>('/v1/audit?limit=100');
		expect(page.next).toBeNull();
		expect(await checkChain(page.items)).toEqual({ state: 'holds', rows: 6, linked: 5 });
		const list = await api.get<ApprovalList>('/v1/approvals?status=pending');
		expect(list.approvals).toEqual([]);
		await expect(
			api.post('/v1/approvals/ap_01JB2M6F9G0H1J2K3L4M5N6P7Q', { decision: 'approve' })
		).rejects.toMatchObject({ status: 404 });
	});
});

describe('mock mode admin panel', () => {
	it("refuses the project manager the panel's reads with the core's 403, before a read", async () => {
		const api = mockTransport({ timeScale: 0, signedIn: true });
		for (const path of ['/v1/admin/overview', '/v1/admin/people', '/v1/admin/repositories']) {
			const refusal = await api.get(path).catch((error: unknown) => error);
			expect(refusal).toBeInstanceOf(ApiError);
			expect((refusal as ApiError).status).toBe(403);
		}
	});

	it('serves the admin the overview of the window asked for, and refuses another window', async () => {
		const api = mockTransport({ timeScale: 0, signedIn: true, person: 'admin' });
		const day = await api.get<AdminOverview>('/v1/admin/overview?window=24h');
		expect(day.window).toBe('24h');
		expect(day.series).toHaveLength(24);
		const week = await api.get<AdminOverview>('/v1/admin/overview');
		expect(week.window).toBe('7d');
		const refusal = await api.get('/v1/admin/overview?window=1y').catch((error: unknown) => error);
		expect((refusal as ApiError).status).toBe(400);
		const people = await api.get<AdminPeople>('/v1/admin/people');
		expect(people.people.some((p) => p.role === 'admin')).toBe(true);
	});
});

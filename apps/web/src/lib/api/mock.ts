/**
 * Mock mode: the same `Transport`, answered from recorded questions instead of a core, so
 * screens can be built and tried without one. Loaded only when `VITE_MERCURY_MOCK=true`, so a
 * production build doesn't contain it.
 *
 * Like the core, it starts signed out (`VITE_MERCURY_MOCK_SIGNED_IN=true` starts it signed in as the
 * project manager, for building a screen without signing in at every reload): `POST /v1/session`
 * with `mcy_` and four or more characters after it (`mcy_demo`, say) signs in as the project
 * manager, `mcy_lead` as a tech lead (who reads the whole trail under an answer, where the others
 * read its short view), `mcy_admin` as an admin, `DELETE /v1/session` signs out, and everything
 * but the health check answers `401` in between. Two words stand in for the core's other sign-in
 * answers: `mcy_expired` and `mcy_limited`. The sign-in page offers each of them as a tile
 * (`SIGN_IN_ACCOUNTS`), so nobody types a token in mock mode.
 *
 * A question is answered by the recording of the same text (or, for a clarify, of the same pick),
 * else by the first recording. The state of a question follows its recording's clock, so a reload
 * finds it running or finished; one held for sign-off is released a few seconds after it was asked.
 *
 * The tech lead approves Delta and the project manager asks: two of the project manager's answers
 * wait for the tech lead, and approving or rejecting takes one off the list. No recording
 * ends in an approver's rejection, so a question asked again (`follows`) gets the core's refusal. The audit is an admin's alone, as on the core: the project
 * manager is refused it with the core's `403`, and the admin reads a short chain of rows, hashed as
 * the core hashes them, so the Audit screen finds it whole. The admin panel's three reads are the
 * admin's too: a small team, its repositories and a month of questions (`mock-admin.ts`).
 */

import { rowHash } from '$lib/audit/chain';
import { RECORDINGS, type Recording } from './recordings';
import { ApiError, isTerminal, type StreamEvent, type Transport } from './transport';
import type {
	Accepted,
	Approval,
	ApprovalDecision,
	AuditItem,
	CoreEvent,
	Done,
	Me,
	NewQuestion,
	ProjectList,
	QuestionState,
	Role
} from './types';

export { RECORDINGS, type Recording };

/** The token that signs in as the mock admin, the one person who reads the audit. */
export const ADMIN_TOKEN = 'mcy_admin';

/** The token that signs in as the mock tech lead, who reads the whole trail under an answer. */
export const LEAD_TOKEN = 'mcy_lead';

/** The token the sign-in page offers for the project manager; any `mcy_` with four or more characters after it is one too. */
export const MANAGER_TOKEN = 'mcy_demo';

/** The questions the Ask page offers in mock mode: one button each, from the recordings. */
export const SAMPLES: {
	intro: string;
	questions: readonly { label: string; text: string; project: string | null }[];
} = {
	intro: 'Mock mode, try one:',
	questions: RECORDINGS.filter((r) => r.sample).map((r) => ({
		label: r.sample ?? r.question.text,
		text: r.question.text,
		project: r.question.project ?? null
	}))
};

/** Who a mock session belongs to: the project manager, the tech lead `mcy_lead` or the admin `mcy_admin` signs in as. */
export type MockPerson = 'manager' | 'lead' | 'admin';

/** The token a mock sign-in is for, one per person. */
const TOKEN_PUBLIC_ID = 'mockTokn0001';
const ADMIN_TOKEN_PUBLIC_ID = 'mockTokn0002';
const LEAD_TOKEN_PUBLIC_ID = 'mockTokn0003';
const TOKEN_VALID_FOR_MS = 30 * 24 * 60 * 60 * 1000;

/** Whose the mock session is, with a token that expires 30 days after the mock started. */
function person(who: MockPerson, now: Date): Me {
	const expires_at = new Date(now.getTime() + TOKEN_VALID_FOR_MS).toISOString();
	if (who === 'admin') {
		// granted both projects, approving neither: the audit is theirs, the sign-off isn't
		return {
			user: {
				id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X3Z',
				email: 'admin@example.com',
				display_name: 'Mock Admin',
				role: 'admin'
			},
			projects: [
				{ project: 'delta', approver: false },
				{ project: 'billing', approver: false }
			],
			token: { public_id: ADMIN_TOKEN_PUBLIC_ID, expires_at }
		};
	}
	if (who === 'lead') {
		// granted both projects, approving Delta: the project manager's held answers wait for them
		return {
			user: {
				id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X4A',
				email: 'lead@example.com',
				display_name: 'Mock Lead',
				role: 'tech_lead'
			},
			projects: [
				{ project: 'delta', approver: true },
				{ project: 'billing', approver: false }
			],
			token: { public_id: LEAD_TOKEN_PUBLIC_ID, expires_at }
		};
	}
	return {
		user: {
			id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X2Y',
			email: 'dana.reyes@example.com',
			display_name: 'Dana Reyes',
			role: 'pm'
		},
		// the one who asks: their held answers wait for the tech lead, who approves Delta
		projects: [
			{ project: 'delta', approver: false },
			{ project: 'billing', approver: false }
		],
		token: { public_id: TOKEN_PUBLIC_ID, expires_at }
	};
}

/** One tile on the sign-in page in mock mode: a person to sign in as, or a token the core refuses. */
export type SignInAccount =
	| { kind: 'person'; token: string; name: string; role: Role }
	| { kind: 'refused'; token: string; name: string };

/**
 * What the sign-in page offers in mock mode instead of a field to type in: the three people, named
 * as their session names them, then the two tokens the core refuses. No token here is real.
 */
export const SIGN_IN_ACCOUNTS: readonly SignInAccount[] = [
	...(
		[
			[MANAGER_TOKEN, 'manager'],
			[LEAD_TOKEN, 'lead'],
			[ADMIN_TOKEN, 'admin']
		] as const
	).map(([token, who]): SignInAccount => {
		const { display_name, role } = person(who, new Date(0)).user;
		return { kind: 'person', token, name: display_name, role };
	}),
	{ kind: 'refused', token: 'mcy_expired', name: 'Expired token' },
	{ kind: 'refused', token: 'mcy_limited', name: 'Too many tries' }
];

/** The two projects of the mock people: Delta's repository is fresh, Billing's is not. */
function projects(now: Date): ProjectList {
	const minutesAgo = (minutes: number) => new Date(now.getTime() - minutes * 60_000).toISOString();
	return {
		projects: [
			{
				project: 'delta',
				display_name: 'Delta — invoicing',
				modules: ['Invoicing', 'Export', 'Auth'],
				classes: ['progress', 'tested', 'blocked', 'health', 'changes', 'open'],
				probe: { status: 'healthy', as_of: minutesAgo(1) }
			},
			{
				project: 'billing',
				display_name: 'Billing',
				modules: ['Payments', 'Plans'],
				classes: ['progress', 'health'],
				probe: { status: 'stale', as_of: minutesAgo(25) }
			}
		]
	};
}

/**
 * Two of the project manager's answers held for sign-off in Delta, for the tech lead to decide on:
 * one a check was unsure about, with the sentence it flagged, and one of a kind always read first.
 */
function held(now: Date): Approval[] {
	const at = (hours: number) => new Date(now.getTime() + hours * 3_600_000).toISOString();
	const draft = (text: string) => ({ text, facts: [], confidence: 0.81 });
	return [
		{
			id: 'ap_01JB2M6F9G0H1J2K3L4M5N6P7Q',
			question_id: 'q_01JB2M6F9G0H1J2K3L4M5N6P8R',
			project: 'delta',
			class: 'progress',
			asker_role: 'pm',
			reason: 'review_band',
			draft: draft(
				'The logging package bump was merged on 29 September. The SEPA export for the client is still a draft.'
			),
			flags: [
				{
					sentence: 'The SEPA export for the client is still a draft.',
					check: 'sentence_contains_code_or_identifier',
					p: 0.45
				}
			],
			asked_by_me: false,
			status: 'pending',
			created_at: at(-1),
			expires_at: at(23)
		},
		{
			id: 'ap_01JB2M6F9G0H1J2K3L4M5N6P9S',
			question_id: 'q_01JB2M6F9G0H1J2K3L4M5N6P9S',
			project: 'delta',
			class: 'tested',
			asker_role: 'pm',
			reason: 'class',
			draft: {
				...draft(
					'The invoice list with filters and the fix for rounding in totals both pass their tests, and CI passed on the last merge.'
				),
				confidence: 0.91
			},
			flags: [],
			asked_by_me: false,
			status: 'pending',
			created_at: at(-2),
			expires_at: at(22)
		}
	];
}

/** A short chain of audit rows of one question, hashed as the core hashes them. */
async function auditRows(): Promise<AuditItem[]> {
	const types = ['accepted', 'project', 'classified', 'judged', 'answer', 'done'];
	const rows: AuditItem[] = [];
	let prev = '0'.repeat(64);
	for (const [i, type] of types.entries()) {
		const row: AuditItem = {
			seq: i + 1,
			question_id: 'q_01JB2M6F9G0H1J2K3L4M5N6P8R',
			type,
			at: new Date(Date.UTC(2026, 9, 6, 9, 0, i)).toISOString(),
			user: 'dana.reyes@example.com',
			token: TOKEN_PUBLIC_ID,
			project: 'delta',
			// the first row's payload as retention leaves it
			payload: i === 0 ? null : {},
			payload_sha256: 'c'.repeat(64),
			prev_hash: prev,
			hash: ''
		};
		row.hash = await rowHash(row);
		prev = row.hash;
		rows.push(row);
	}
	return rows;
}

export interface MockOptions {
	recordings?: readonly Recording[];
	/** Multiplies every recorded delay: 1 plays in real time, 0 as fast as possible. */
	timeScale?: number;
	/** Starts with a session, as a browser that signed in before. The default is signed out. */
	signedIn?: boolean;
	/** Whose session `signedIn` starts with; the project manager's unless it says otherwise. */
	person?: MockPerson;
}

function notRecorded(path: string): ApiError {
	return new ApiError(404, 'invalid_request', `Mock mode has no recording for ${path}.`);
}

function unauthenticated(): ApiError {
	return new ApiError(
		401,
		'unauthenticated',
		'The request has no valid token.',
		'Sign in again with your token, or ask the admin for a new one.'
	);
}

function invalidBody(): ApiError {
	return new ApiError(400, 'invalid_request', "The request body isn't the JSON this route takes.");
}

/**
 * The core's answer to a `follows` that doesn't name the person's own question an approver
 * rejected: one message whatever the reason.
 */
function notRejected(): ApiError {
	return new ApiError(
		400,
		'invalid_request',
		"The question to ask again isn't one of yours that an approver rejected.",
		"Name your own question that ended with an approver's rejection, or ask without it."
	);
}

/** The core's answer to anyone but an admin who asks for the audit, before a row is read. */
function onlyAnAdmin(): ApiError {
	return new ApiError(
		403,
		'invalid_request',
		'Only an admin reads the audit.',
		'What happened to your own question is shown where you asked it; ask an admin for anything else.'
	);
}

/** The core's answer to anyone but an admin who asks for the admin panel's data. */
function onlyAnAdminPanel(): ApiError {
	return new ApiError(
		403,
		'invalid_request',
		'Only an admin reads the admin panel.',
		'Ask an admin about people, access or repositories.'
	);
}

function badWindow(): ApiError {
	return new ApiError(
		400,
		'invalid_request',
		"The parameter `window` isn't 24h, 7d or 30d.",
		"See /v1/openapi.json for the admin panel's parameters."
	);
}

/** The core's answers to a token it is asked to open a session for, and whose session it opens. */
function checkToken(token: unknown): MockPerson {
	if (typeof token !== 'string') throw invalidBody();
	if (token === 'mcy_expired') {
		throw new ApiError(
			401,
			'token_expired',
			'The token has expired.',
			'Ask the admin for a new token.'
		);
	}
	if (token === 'mcy_limited') {
		throw new ApiError(
			429,
			'rate_limited',
			'Too many requests with this token.',
			'Wait 30 seconds, then try again.',
			30
		);
	}
	if (!/^mcy_\S{4,}$/.test(token)) throw unauthenticated();
	if (token === ADMIN_TOKEN) return 'admin';
	return token === LEAD_TOKEN ? 'lead' : 'manager';
}

/** How long a recording takes to play, with its delays scaled. */
function lengthOf(recording: Recording, timeScale: number): number {
	return recording.events.reduce((sum, e) => sum + e.after_ms * timeScale, 0);
}

/** The data of a recording's event of that name, if it has one. */
function dataOf<N extends CoreEvent['event']>(recording: Recording, name: N) {
	return recording.events.find((e) => e.event === name)?.data as
		Extract<CoreEvent, { event: N }>['data'] | undefined;
}

/** The recording a question is answered by. */
function recordingFor(recordings: readonly Recording[], body: NewQuestion): Recording {
	const { text, clarify } = body;
	const answered = clarify
		? recordings.find(
				(r) =>
					r.question.clarify?.of === clarify.of &&
					r.question.clarify.option === clarify.option &&
					r.question.text === text
			)
		: recordings.find((r) => !r.question.clarify && r.question.text === text);
	return answered ?? recordings.find((r) => !r.question.clarify) ?? recordings[0];
}

export function mockTransport({
	recordings = RECORDINGS,
	timeScale = 1,
	signedIn = false,
	person: startsAs = 'manager'
}: MockOptions = {}): Transport {
	/** Whose session the browser has, or `null` while it has none. */
	let who: MockPerson | null = signedIn ? startsAs : null;
	const started = new Date();
	let waiting = held(started);
	/** The held answers the person decides on: the tech lead approves Delta, where the project manager asks. */
	const theirs = () => (who === 'lead' ? waiting : []);
	/**
	 * Every question asked here, as on the core: an id of its own (the recording's, and a number),
	 * the recording it plays and when it was asked. A recording's own id, or a question this mock no
	 * longer remembers, plays a question asked long ago.
	 */
	const asked = new Map<string, { recording: Recording; at: number }>();
	let count = 0;
	/** Whose session the request comes with; a request without one is the core's `401`. */
	const needSession = (): MockPerson => {
		if (who === null) throw unauthenticated();
		return who;
	};

	function find(id: string): { id: string; recording: Recording; at: number } {
		const known = asked.get(id);
		if (known) return { id, ...known };
		const recording = recordings.find((r) => r.id === id.replace(/-\d+$/, ''));
		if (!recording) {
			throw new ApiError(404, 'invalid_request', 'There is no such question.');
		}
		return { id, recording, at: -Infinity };
	}

	/** `GET /v1/questions/{id}`, from the recording and how far it has played. */
	function stateOf({ id, recording, at }: ReturnType<typeof find>): QuestionState {
		const sinceAsked = Date.now() - at;
		const last = recording.events[recording.events.length - 1].event;
		const state: QuestionState = {
			id,
			status: 'running',
			project: dataOf(recording, 'project')?.project ?? null,
			class: dataOf(recording, 'classified')?.class ?? null,
			answer: null,
			refusal: null,
			judge_mode: null,
			cost_usd: 0,
			duration_ms: null,
			audit_id: null
		};
		if (sinceAsked < lengthOf(recording, timeScale)) return state;

		const settle = (done: Done) => ({
			judge_mode: done.judge_mode,
			cost_usd: done.cost_usd,
			duration_ms: done.duration_ms,
			audit_id: done.audit_id
		});
		switch (last) {
			case 'done': {
				const done = dataOf(recording, 'done');
				return {
					...state,
					status: 'delivered',
					answer: dataOf(recording, 'answer') ?? null,
					...(done ? settle(done) : {})
				};
			}
			case 'refused':
				return { ...state, status: 'refused', refusal: dataOf(recording, 'refused') ?? null };
			case 'clarify':
				return { ...state, status: 'clarify' };
			case 'pending_signoff': {
				const { approved } = recording;
				return approved && sinceAsked >= approved.after_ms * timeScale
					? {
							...state,
							status: 'delivered',
							answer: approved.answer,
							...settle(approved.done)
						}
					: { ...state, status: 'pending_signoff' };
			}
			default:
				return { ...state, status: 'failed' };
		}
	}

	return {
		async get<T>(path: string) {
			if (path === '/v1/health') return { status: 'ok', version: '0.0.0' } as T;
			const caller = needSession();
			if (path === '/v1/me') return person(caller, started) as T;
			if (path === '/v1/projects') return projects(new Date()) as T;
			if (path.startsWith('/v1/approvals')) return { approvals: theirs() } as T;
			if (path === '/v1/audit' || path.startsWith('/v1/audit?')) {
				// an admin's alone: anyone else is refused before a row is read
				if (caller !== 'admin') throw onlyAnAdmin();
				return { items: await auditRows(), next: null } as T;
			}
			if (path.startsWith('/v1/admin/')) {
				// the panel is an admin's alone, as on the core: anyone else is refused before a read
				if (caller !== 'admin') throw onlyAnAdminPanel();
				const { mockOverview, mockPeople, mockRepositories } = await import('./mock-admin');
				const url = new URL(path, 'http://mock');
				if (url.pathname === '/v1/admin/overview') {
					const window = url.searchParams.get('window') ?? '7d';
					if (window !== '24h' && window !== '7d' && window !== '30d') throw badWindow();
					return mockOverview(window, new Date()) as T;
				}
				if (path === '/v1/admin/people') return mockPeople(new Date()) as T;
				if (path === '/v1/admin/repositories') return mockRepositories(new Date()) as T;
			}
			const question = /^\/v1\/questions\/([^/]+)$/.exec(path);
			if (question) return stateOf(find(decodeURIComponent(question[1]))) as T;
			throw notRecorded(path);
		},

		async post<T>(path: string, body: unknown) {
			if (path === '/v1/session') {
				who = checkToken((body as { token?: unknown } | undefined)?.token);
				return undefined as T;
			}
			needSession();
			if (path === '/v1/questions') {
				const question = body as Partial<NewQuestion> | undefined;
				if (typeof question?.text !== 'string' || question.text.trim() === '') throw invalidBody();
				// a question asked again names one of this mock's that an approver rejected, as on the core
				if (question.follows) {
					const known = asked.get(question.follows);
					const ended = known ? stateOf({ id: question.follows, ...known }) : null;
					if (ended?.refusal?.code !== 'rejected_by_approver') throw notRejected();
				}
				// a clarify names the question it answers, which plays a recording
				const clarify = question.clarify
					? { ...question.clarify, of: find(question.clarify.of).recording.id }
					: undefined;
				const recording = recordingFor(recordings, { ...(question as NewQuestion), clarify });
				count += 1;
				const id = `${recording.id}-${count}`;
				asked.set(id, { recording, at: Date.now() });
				return { id } as Accepted as T;
			}
			const decided = /^\/v1\/approvals\/([^/]+)$/.exec(path);
			if (decided) {
				const id = decodeURIComponent(decided[1]);
				const item = theirs().find((a) => a.id === id);
				if (!item) throw new ApiError(404, 'invalid_request', 'There is no such approval.');
				const decision = body as Partial<ApprovalDecision> | undefined;
				if (decision?.decision === 'reject' && !decision.note?.trim()) {
					throw new ApiError(400, 'invalid_request', 'A rejection needs a note.');
				}
				waiting = waiting.filter((a) => a.id !== id);
				return undefined as T;
			}
			throw notRecorded(path);
		},

		async delete<T>(path: string) {
			if (path !== '/v1/session') throw notRecorded(path);
			needSession();
			who = null;
			return undefined as T;
		},

		events(questionId, onEvent) {
			needSession();
			const { recording, at: askedAt } = find(questionId);
			const timers: ReturnType<typeof setTimeout>[] = [];
			// like the core, a stream opened late replays what already happened at once
			const elapsed = Date.now() - askedAt;
			let at = 0;
			let stopped = false;
			recording.events.forEach((recorded, index) => {
				at += recorded.after_ms * timeScale;
				const delay = Math.max(0, at - elapsed);
				timers.push(
					setTimeout(() => {
						if (stopped) return;
						// a recording is checked against the generated types where it is written
						const event = {
							event: recorded.event,
							id: String(index + 1),
							// the question's own id, not the recording's
							data:
								recorded.event === 'accepted'
									? { ...(recorded.data as object), id: questionId }
									: recorded.data
						} as StreamEvent;
						onEvent(event);
						if (isTerminal(event.event)) stopped = true;
					}, delay)
				);
			});
			return () => {
				stopped = true;
				timers.forEach(clearTimeout);
			};
		}
	};
}

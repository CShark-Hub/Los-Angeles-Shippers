import { afterEach, describe, expect, it } from 'vitest';
import {
	ApiError,
	onSessionEnded,
	watchingTheSession,
	type AuditItem,
	type AuditPage,
	type Me,
	type Role,
	type Transport
} from '$lib/api';
import { mockTransport } from '$lib/api/mock';
import { Session } from '$lib/session.svelte';
import { AuditView } from './audit.svelte';
import {
	CLASSES,
	exportCheckPath,
	exportPath,
	filterQuery,
	noFilters,
	OUTCOMES,
	pagePath
} from './query';
import { rowHash } from './chain';

/** A chain of `n` rows from `seq` 1, hashed as the core hashes them. */
async function rows(n: number): Promise<AuditItem[]> {
	const out: AuditItem[] = [];
	let prev = '0'.repeat(64);
	for (let seq = 1; seq <= n; seq += 1) {
		const row: AuditItem = {
			seq,
			question_id: null,
			type: 'probe_judge',
			at: `2026-10-06T08:46:${String(seq).padStart(2, '0')}.000Z`,
			user: null,
			token: null,
			project: 'delta',
			payload: null,
			payload_sha256: 'c'.repeat(64),
			prev_hash: prev,
			hash: ''
		};
		row.hash = await rowHash(row);
		prev = row.hash;
		out.push(row);
	}
	return out;
}

/** A core that serves `all` two rows a page, and records what it was asked. */
function fakeCore(all: AuditItem[], fail?: ApiError) {
	const asked: string[] = [];
	const transport: Transport = {
		async get<T>(path: string) {
			asked.push(path);
			if (fail) throw fail;
			const after = Number(new URL(path, 'http://localhost').searchParams.get('cursor') ?? 0);
			const items = all.filter((r) => r.seq > after).slice(0, 2);
			const last = items.at(-1);
			const page: AuditPage = {
				items,
				next: last && last.seq < all.length ? String(last.seq) : null
			};
			return page as T;
		},
		post: async <T>() => undefined as T,
		delete: async <T>() => undefined as T,
		events: () => () => {}
	};
	return { transport, asked };
}

describe('the audit screen', () => {
	it('sends only the filters that are set, trimmed', () => {
		const filters = { ...noFilters(), project: ' delta ', outcome: 'refused', from: '2026-10-01' };
		expect(filterQuery(filters).toString()).toBe('project=delta&outcome=refused&from=2026-10-01');
		expect(pagePath(filters, '41')).toBe(
			'/v1/audit?project=delta&outcome=refused&from=2026-10-01&limit=100&cursor=41'
		);
		expect(exportPath(filters)).toBe(
			'/v1/audit/export?project=delta&outcome=refused&from=2026-10-01&format=jsonl'
		);
		expect(pagePath(noFilters())).toBe('/v1/audit?limit=100');
	});

	it('offers every class of question and every outcome, and sends the one picked', () => {
		expect(CLASSES).toEqual([
			'progress',
			'tested',
			'blocked',
			'health',
			'changes',
			'open',
			'portfolio',
			'forbidden',
			'other'
		]);
		expect(OUTCOMES).toHaveLength(6);
		expect(pagePath({ ...noFilters(), class: 'forbidden' })).toBe(
			'/v1/audit?class=forbidden&limit=100'
		);
	});

	it('loads page after page by the cursor, and checks the chain over every row loaded', async () => {
		const core = fakeCore(await rows(3));
		const view = new AuditView(async () => core.transport);
		view.filters.project = 'delta';
		await view.search();
		expect(view.items.map((r) => r.seq)).toEqual([1, 2]);
		expect(view.next).toBe('2');
		expect(view.chain).toEqual({ state: 'holds', rows: 2, linked: 1 });
		await view.more();
		expect(view.items.map((r) => r.seq)).toEqual([1, 2, 3]);
		expect(view.next).toBeNull();
		expect(view.chain).toEqual({ state: 'holds', rows: 3, linked: 2 });
		expect(core.asked).toEqual([
			'/v1/audit?project=delta&limit=100',
			'/v1/audit?project=delta&limit=100&cursor=2'
		]);
	});

	it('names the row that breaks the chain', async () => {
		const all = await rows(2);
		all[1].type = 'answer';
		const view = new AuditView(async () => fakeCore(all).transport);
		await view.search();
		expect(view.chain).toEqual({ state: 'broken', seq: 2, what: 'hash' });
	});

	it('keeps the filters it loaded with apart from the form, for the export', async () => {
		const all = await rows(1);
		const view = new AuditView(async () => fakeCore(all).transport);
		view.filters.outcome = 'refused';
		await view.search();
		view.filters.outcome = 'delivered';
		expect(exportPath(view.applied)).toContain('outcome=refused');
		await view.clear();
		expect(view.applied).toEqual(noFilters());
	});

	it('says in words why the rows could not be read', async () => {
		const view = new AuditView(
			async () => fakeCore([], new ApiError(0, 'internal', 'unreachable')).transport
		);
		await view.search();
		expect(view.failure?.message).toBe("Mercury can't be reached.");
		expect(view.loaded).toBe(false);
	});
});

/** `GET /v1/me` for someone with this role. */
function me(role: Role): Me {
	return {
		user: {
			id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X3Z',
			email: 'admin@example.com',
			display_name: 'Mock Admin',
			role
		},
		projects: [{ project: 'delta', approver: false }],
		token: { public_id: 'mockTokn0002', expires_at: '2026-11-01T00:00:00Z' }
	};
}

const ONLY_AN_ADMIN = new ApiError(
	403,
	'invalid_request',
	'Only an admin reads the audit.',
	'What happened to your own question is shown where you asked it; ask an admin for anything else.'
);

/**
 * A core whose admin can be demoted: `/v1/me` says the role, the audit refuses anyone but an admin
 * with the core's `403`, and `failWith` makes the audit routes fail some other way.
 */
function demotable(all: AuditItem[]) {
	let role: Role = 'admin';
	let failure: ApiError | null = null;
	const asked: string[] = [];
	const transport: Transport = {
		async get<T>(path: string) {
			asked.push(path);
			if (path === '/v1/me') return me(role) as T;
			if (failure) throw failure;
			if (role !== 'admin') throw ONLY_AN_ADMIN;
			return { items: all, next: null } as T;
		},
		post: async <T>() => undefined as T,
		delete: async <T>() => undefined as T,
		events: () => () => {}
	};
	return {
		transport,
		asked,
		demote() {
			role = 'pm';
		},
		failWith(error: ApiError | null) {
			failure = error;
		}
	};
}

/** A signed-in session and an Audit screen over the same core, wired as the app wires them. */
async function screenOver(core: ReturnType<typeof demotable>) {
	// the app's transport: a 401 on any route but the session's own ends the session
	const api = watchingTheSession(core.transport);
	const session = new Session(async () => api);
	onSessionEnded(() => session.sessionEnded());
	await session.restore();
	const view = new AuditView(
		async () => api,
		() => session.refresh()
	);
	return { session, view };
}

const admin = (session: Session) => session.me?.user.role === 'admin';

describe('the audit, refused', () => {
	afterEach(() => {
		onSessionEnded(() => {});
	});

	it("reads the person's role again after a 403, so a demoted admin loses the screen and the rows", async () => {
		const core = demotable(await rows(2));
		const { session, view } = await screenOver(core);
		await view.search();
		expect(view.items).toHaveLength(2);
		expect(admin(session)).toBe(true);

		core.demote();
		await view.search();
		expect(view.failure).toEqual({
			message: 'Only an admin reads the audit.',
			hint: 'What happened to your own question is shown where you asked it; ask an admin for anything else.'
		});
		expect(view.items).toEqual([]);
		expect(view.loaded).toBe(false);
		// whose the session is was asked again: the screen and its link follow the new role
		expect(core.asked.filter((path) => path === '/v1/me')).toHaveLength(2);
		expect(admin(session)).toBe(false);
		expect(session.status).toBe('signed-in');
	});

	it("is the core's 403 in mock mode too, for the project manager the mock signs in", async () => {
		const mock = mockTransport({ timeScale: 0, signedIn: true });
		const view = new AuditView(
			async () => mock,
			async () => {}
		);
		await view.search();
		expect(view.failure?.message).toBe('Only an admin reads the audit.');
		expect(view.loaded).toBe(false);

		const asAdmin = mockTransport({ timeScale: 0, signedIn: true, person: 'admin' });
		const adminView = new AuditView(async () => asAdmin);
		await adminView.search();
		expect(adminView.failure).toBeNull();
		expect(adminView.chain).toMatchObject({ state: 'holds' });
	});
});

describe('the export, before the browser downloads it', () => {
	afterEach(() => {
		onSessionEnded(() => {});
	});

	it('tries the filters on one row of the page route, which writes no audit row, then lets the download start', async () => {
		const core = demotable(await rows(1));
		const { view } = await screenOver(core);
		view.filters.outcome = 'refused';
		await view.search();
		view.filters.outcome = 'delivered';
		expect(await view.checkExport()).toBe(true);
		expect(core.asked.at(-1)).toBe('/v1/audit?outcome=refused&limit=1');
		expect(exportCheckPath(view.applied)).toBe('/v1/audit?outcome=refused&limit=1');
		expect(view.exportFailure).toBeNull();
		expect(view.exporting).toBe(false);
	});

	it("says a server's failure in words, and starts nothing", async () => {
		const core = demotable(await rows(1));
		const { session, view } = await screenOver(core);
		await view.search();
		for (const status of [500, 502, 503]) {
			core.failWith(new ApiError(status, 'internal', `The core answered ${status}.`));
			expect(await view.checkExport()).toBe(false);
			expect(view.exportFailure).toEqual({
				message: 'Something went wrong on the server.',
				hint: 'Try again. If it keeps failing, tell your admin.'
			});
		}
		// the rows read before stay: the server failed, the person may still read them
		expect(view.items).toHaveLength(1);
		expect(session.status).toBe('signed-in');

		core.failWith(null);
		expect(await view.checkExport()).toBe(true);
		expect(view.exportFailure).toBeNull();
	});

	it('signs the person out on a 401, with the words of an ended session', async () => {
		const core = demotable(await rows(1));
		const { session, view } = await screenOver(core);
		await view.search();
		core.failWith(new ApiError(401, 'unauthenticated', 'The request has no valid token.'));
		expect(await view.checkExport()).toBe(false);
		expect(view.exportFailure).toEqual({
			message: 'Your session has ended.',
			hint: 'Sign in again.'
		});
		expect(session.status).toBe('signed-out');
		expect(session.notice).toBe('Your session ended. Sign in again.');
	});

	it("says why on a 403, and reads the person's role again", async () => {
		const core = demotable(await rows(1));
		const { session, view } = await screenOver(core);
		await view.search();
		core.demote();
		expect(await view.checkExport()).toBe(false);
		expect(view.exportFailure?.message).toBe('Only an admin reads the audit.');
		expect(view.items).toEqual([]);
		expect(admin(session)).toBe(false);
	});

	it('checks one export at a time', async () => {
		const core = demotable(await rows(1));
		const { view } = await screenOver(core);
		await view.search();
		const first = view.checkExport();
		expect(view.exporting).toBe(true);
		expect(await view.checkExport()).toBe(false);
		expect(await first).toBe(true);
	});
});

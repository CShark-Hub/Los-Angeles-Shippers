import { afterEach, describe, expect, it, vi } from 'vitest';

import { ApiError, onSessionEnded, watchingTheSession, type Me, type Transport } from '$lib/api';
import { mockTransport } from '$lib/api/mock';
import { SessionNotKept } from './errors';
import { Session } from './session.svelte';

const TOKEN = 'mcy_AbCdEf123456_0123456789abcdefghijklmnopqrstuvwxyzABCDEFG';

/** A session over a mock core that starts signed out (or in), and the mock itself. */
function sessionOver(signedIn = false) {
	const core = mockTransport({ timeScale: 0, signedIn });
	return { session: new Session(async () => core), core };
}

afterEach(() => {
	vi.unstubAllGlobals();
	onSessionEnded(() => {});
});

describe('Session', () => {
	it('finds out whose browser it is once, and keeps the answer', async () => {
		const { session, core } = sessionOver(true);
		const get = vi.spyOn(core, 'get');
		await session.restore();
		await session.restore();
		expect(session.status).toBe('signed-in');
		expect(session.me?.user.display_name).toBe('Dana Reyes');
		expect(get).toHaveBeenCalledTimes(1);
	});

	it('is signed out when the core says there is no session', async () => {
		const { session } = sessionOver(false);
		await session.restore();
		expect(session.status).toBe('signed-out');
		expect(session.me).toBeNull();
		expect(session.notice).toBeNull();
	});

	it('is unreachable, not signed out, when the core cannot be reached, and tries again when asked', async () => {
		let reachable = false;
		const real = mockTransport({ signedIn: true });
		const flaky: Transport = {
			...real,
			get: async (path) => {
				if (!reachable) throw new ApiError(0, 'internal', "The core can't be reached.");
				return real.get(path);
			}
		};
		const session = new Session(async () => flaky);
		await session.restore();
		expect(session.status).toBe('unreachable');
		expect(session.failure).toMatchObject({ message: "Mercury can't be reached." });

		reachable = true;
		await session.restore();
		expect(session.status).toBe('signed-in');
		expect(session.failure).toBeNull();
	});

	it('says so when the core opened a session the browser did not keep', async () => {
		const real = mockTransport({ timeScale: 0, signedIn: false });
		// 204 for the sign-in, then a 401 for whose it is: a Secure cookie dropped on plain http
		const dropping: Transport = {
			...real,
			post: async () => undefined as never,
			get: async () => {
				throw new ApiError(401, 'unauthenticated', 'The request has no valid token.');
			}
		};
		const session = new Session(async () => dropping);
		await expect(session.signIn(TOKEN)).rejects.toBeInstanceOf(SessionNotKept);
		expect(session.status).not.toBe('signed-in');
	});

	it('signs in with a token and shows who the session belongs to', async () => {
		const { session } = sessionOver();
		await session.signIn(TOKEN);
		expect(session.status).toBe('signed-in');
		expect(session.me?.user.role).toBe('pm');
		expect(session.notice).toBeNull();
	});

	it('throws what the core refuses a sign-in with, and stays signed out', async () => {
		const { session } = sessionOver();
		for (const [token, code] of [
			['not a token', 'unauthenticated'],
			['mcy_expired', 'token_expired'],
			['mcy_limited', 'rate_limited']
		]) {
			await expect(session.signIn(token)).rejects.toMatchObject({ code });
		}
		expect(session.status).not.toBe('signed-in');
		expect(session.me).toBeNull();
	});

	it('keeps the token nowhere: not in its state, not in storage', async () => {
		const touched: string[] = [];
		const refuse = (name: string) => () => {
			touched.push(name);
			throw new Error('storage is not for this');
		};
		vi.stubGlobal('localStorage', { setItem: refuse('local'), getItem: refuse('local') });
		vi.stubGlobal('sessionStorage', { setItem: refuse('session'), getItem: refuse('session') });
		const { session } = sessionOver();
		await session.signIn(TOKEN);
		await session.signOut();
		await session.signIn(TOKEN).catch(() => {});

		expect(touched).toEqual([]);
		const state = JSON.stringify({
			status: session.status,
			me: session.me,
			notice: session.notice,
			failure: session.failure
		});
		expect(state).not.toContain(TOKEN);
		expect(state).not.toContain(TOKEN.slice(17));
	});

	it('signs out, and tells whoever is listening', async () => {
		const { session } = sessionOver(true);
		await session.restore();
		const heard = vi.fn();
		const stop = session.onSignedOut(heard);
		await session.signOut();
		expect(session.status).toBe('signed-out');
		expect(session.me).toBeNull();
		expect(session.notice).toBeNull();
		expect(heard).toHaveBeenCalledTimes(1);

		stop();
		await session.signIn(TOKEN);
		await session.signOut();
		expect(heard).toHaveBeenCalledTimes(1);
	});

	it('counts a session that was gone already as signed out, and any other failure as one to show', async () => {
		const { session } = sessionOver(false);
		await expect(session.signOut()).resolves.toBeUndefined();
		expect(session.status).toBe('signed-out');

		const refusing: Transport = {
			...mockTransport(),
			delete: async () => {
				throw new ApiError(0, 'internal', "The core can't be reached.");
			}
		};
		const unreachable = new Session(async () => refusing);
		await unreachable.signIn(TOKEN).catch(() => {});
		unreachable.status = 'signed-in';
		await expect(unreachable.signOut()).rejects.toMatchObject({ status: 0 });
		expect(unreachable.status, 'a failed sign-out leaves the session as it was').toBe('signed-in');
	});

	it('goes back to signed out, with a notice, when a request finds the session gone', async () => {
		const { session } = sessionOver(true);
		await session.restore();
		const heard = vi.fn();
		session.onSignedOut(heard);

		session.sessionEnded();
		expect(session.status).toBe('signed-out');
		expect(session.me).toBeNull();
		expect(session.notice).toBe('Your session ended. Sign in again.');
		expect(heard).toHaveBeenCalledTimes(1);

		// nobody was signed in: nothing to tell
		session.sessionEnded();
		expect(heard).toHaveBeenCalledTimes(1);
	});

	it('reads whose session it is again when asked, and follows a role the admin changed', async () => {
		let role: Me['user']['role'] = 'admin';
		const real = mockTransport({ timeScale: 0, signedIn: true, person: 'admin' });
		const changing: Transport = {
			...real,
			get: async <T>(path: string): Promise<T> => {
				const answer = await real.get<T>(path);
				if (path !== '/v1/me') return answer;
				const me = answer as unknown as Me;
				return { ...me, user: { ...me.user, role } } as unknown as T;
			}
		};
		const get = vi.spyOn(changing, 'get');
		const session = new Session(async () => changing);
		await session.restore();
		expect(session.me?.user.role).toBe('admin');

		role = 'pm';
		// restore keeps what it knows; refresh asks
		await session.restore();
		expect(session.me?.user.role).toBe('admin');
		await session.refresh();
		expect(session.me?.user.role).toBe('pm');
		expect(get).toHaveBeenCalledTimes(2);
		expect(session.status).toBe('signed-in');
	});

	it('asks nothing while signed out, ends the session on a 401 and keeps what it knew on any other failure', async () => {
		const { session, core } = sessionOver(false);
		const get = vi.spyOn(core, 'get');
		await session.refresh();
		expect(get).not.toHaveBeenCalled();

		const real = mockTransport({ timeScale: 0, signedIn: true });
		let refusal: ApiError | null = null;
		const failing: Transport = {
			...real,
			get: async <T>(path: string) => {
				if (refusal) throw refusal;
				return real.get<T>(path);
			}
		};
		const known = new Session(async () => failing);
		await known.restore();
		refusal = new ApiError(0, 'internal', "The core can't be reached.");
		await known.refresh();
		expect(known.status).toBe('signed-in');
		expect(known.me?.user.display_name).toBe('Dana Reyes');

		refusal = new ApiError(401, 'unauthenticated', 'The request has no valid token.');
		await known.refresh();
		expect(known.status).toBe('signed-out');
		expect(known.me).toBeNull();
		expect(known.notice).toBe('Your session ended. Sign in again.');
	});

	it('clears the notice when the next sign-in succeeds', async () => {
		const { session } = sessionOver(true);
		await session.restore();
		session.sessionEnded();
		await session.signIn(TOKEN);
		expect(session.notice).toBeNull();
	});
});

describe('a 401 anywhere', () => {
	function over(answer: () => Promise<never>): Transport {
		const base: Transport = {
			get: answer,
			post: answer,
			delete: answer,
			events: () => () => {}
		};
		return watchingTheSession(base);
	}
	const unauthenticated = () =>
		Promise.reject(new ApiError(401, 'unauthenticated', 'The request has no valid token.'));

	it('is reported for every route but the session’s own', async () => {
		const ended = vi.fn();
		onSessionEnded(ended);
		const api = over(unauthenticated);
		await expect(api.get('/v1/me')).rejects.toMatchObject({ status: 401 });
		await expect(api.post('/v1/questions', { text: 'x' })).rejects.toMatchObject({ status: 401 });
		await expect(api.delete('/v1/questions/q_1')).rejects.toMatchObject({ status: 401 });
		expect(ended).toHaveBeenCalledTimes(3);

		// a wrong token, or a sign-out that finds nothing to sign out, are answers
		await expect(api.post('/v1/session', { token: 'x' })).rejects.toMatchObject({ status: 401 });
		await expect(api.delete('/v1/session')).rejects.toMatchObject({ status: 401 });
		expect(ended).toHaveBeenCalledTimes(3);
	});

	it('is not reported for other failures', async () => {
		const ended = vi.fn();
		onSessionEnded(ended);
		const forbidden = () => Promise.reject(new ApiError(403, 'forbidden_project', 'No.'));
		await expect(over(forbidden).get('/v1/me')).rejects.toMatchObject({ status: 403 });
		const down = () => Promise.reject(new ApiError(0, 'internal', 'Down.'));
		await expect(over(down).get('/v1/me')).rejects.toMatchObject({ status: 0 });
		expect(ended).not.toHaveBeenCalled();
	});
});

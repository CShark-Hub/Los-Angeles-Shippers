/**
 * Who is signed in. The token is typed once, sent once to `POST /v1/session` and never kept: not
 * here, not in a store, not in storage. What stays is the session cookie, which this code cannot
 * read, and `GET /v1/me`'s answer about whose session it is.
 */

import { SvelteSet } from 'svelte/reactivity';

import { ApiError, getTransport, onSessionEnded, type Me, type Transport } from '$lib/api';
import { SessionNotKept, describe, type Described } from '$lib/errors';

export type SessionStatus =
	/** Not asked yet. */
	| 'unknown'
	| 'signed-in'
	| 'signed-out'
	/** The core could not be reached, so nothing is known about the session. */
	| 'unreachable';

export class Session {
	status = $state<SessionStatus>('unknown');
	/** Whose session this is, while it is one. */
	me = $state<Me | null>(null);
	/** Why the person is signed out when they didn't choose it. */
	notice = $state<string | null>(null);
	/** What went wrong while the core couldn't be reached. */
	failure = $state<Described | null>(null);

	#api: () => Promise<Transport>;
	#signedOut = new SvelteSet<() => void>();

	constructor(api: () => Promise<Transport> = getTransport) {
		this.#api = api;
	}

	/**
	 * Asks the core whose session this browser has, once; a later call does nothing unless the core
	 * was unreachable. [`Session.refresh`] asks again.
	 */
	async restore(): Promise<void> {
		if (this.status === 'signed-in' || this.status === 'signed-out') return;
		try {
			const api = await this.#api();
			this.me = await api.get<Me>('/v1/me');
			this.status = 'signed-in';
			this.failure = null;
		} catch (error) {
			if (error instanceof ApiError && error.status === 401) {
				this.status = 'signed-out';
				this.me = null;
			} else {
				this.status = 'unreachable';
				this.failure = describe(error);
			}
		}
	}

	/**
	 * Asks the core again whose session this is, while it is one: a refusal the screens were not
	 * expecting (the audit's `403` for someone the app took for an admin) can mean the admin changed the
	 * person's role or grants since. What the screens show follows the new answer; a `401` ends the
	 * session, and any other failure leaves what was known.
	 */
	async refresh(): Promise<void> {
		if (this.status !== 'signed-in') return;
		try {
			const api = await this.#api();
			const me = await api.get<Me>('/v1/me');
			// signed out while the answer was on its way: it is nobody's any more
			if (this.status === 'signed-in') this.me = me;
		} catch (error) {
			if (error instanceof ApiError && error.status === 401) this.sessionEnded();
		}
	}

	/**
	 * Trades the token for a session, then asks whose it is. The token goes into the one request and
	 * is not kept; a refusal is thrown as the core's `ApiError` for the form to put in words.
	 */
	async signIn(token: string): Promise<void> {
		const api = await this.#api();
		await api.post('/v1/session', { token });
		try {
			this.me = await api.get<Me>('/v1/me');
		} catch (error) {
			// the session was opened, so a 401 now is a cookie the browser didn't keep
			if (error instanceof ApiError && error.status === 401) throw new SessionNotKept();
			throw error;
		}
		this.status = 'signed-in';
		this.notice = null;
		this.failure = null;
	}

	/** Ends the session. One that was gone already is as good as one that was ended. */
	async signOut(): Promise<void> {
		const api = await this.#api();
		try {
			await api.delete('/v1/session');
		} catch (error) {
			if (!(error instanceof ApiError && error.status === 401)) throw error;
		}
		this.#end(null);
	}

	/** A request was refused as unauthenticated: if someone was signed in, they are not any more. */
	sessionEnded(): void {
		if (this.status === 'signed-in') this.#end('Your session ended. Sign in again.');
	}

	/** Calls `listener` whenever the person ends up signed out, however it came about. */
	onSignedOut(listener: () => void): () => void {
		this.#signedOut.add(listener);
		return () => this.#signedOut.delete(listener);
	}

	#end(notice: string | null): void {
		this.status = 'signed-out';
		this.me = null;
		this.notice = notice;
		for (const listener of this.#signedOut) listener();
	}
}

/** The app's one session. */
export const session = new Session();
onSessionEnded(() => session.sessionEnded());

/**
 * What went wrong, in words for someone who doesn't read code: a sentence that says what happened
 * and, when there is one, what to do about it. The core's own error messages are written the same
 * way, so a refusal it explains is shown as it sent it.
 */

import type { ErrorCode } from '$lib/api';
import { ApiError } from '$lib/api/transport';

export interface Described {
	message: string;
	hint?: string;
}

const TRY_AGAIN = 'Try again. If it keeps failing, tell your admin.';

function wait(seconds: number | undefined): string {
	return seconds === undefined
		? 'Wait a minute'
		: `Wait ${seconds} second${seconds === 1 ? '' : 's'}`;
}

/** Anything the app asked of the core and didn't get. */
export function describe(error: unknown): Described {
	if (!(error instanceof ApiError)) {
		return { message: 'Something went wrong in the app.', hint: TRY_AGAIN };
	}
	if (error.status === 0) {
		return { message: "Mercury can't be reached.", hint: 'Check your connection, then try again.' };
	}
	if (error.status === 401) {
		return { message: 'Your session has ended.', hint: 'Sign in again.' };
	}
	if (error.code === 'rate_limited') {
		return { message: 'Too many requests.', hint: `${wait(error.retryAfter)}, then try again.` };
	}
	if (error.status >= 500 || error.code === 'internal') {
		return { message: 'Something went wrong on the server.', hint: TRY_AGAIN };
	}
	return { message: error.message, hint: error.hint };
}

/**
 * The core accepted the token and opened a session, but the browser didn't keep its cookie: the
 * cookie is `Secure`, and a browser drops it on plain http anywhere but `localhost`.
 */
export class SessionNotKept extends Error {
	constructor() {
		super("The browser didn't keep the session.");
		this.name = 'SessionNotKept';
	}
}

/** A sign-in the core refused: the token is the only thing the person can fix. */
export function describeSignIn(error: unknown): Described {
	if (error instanceof SessionNotKept) {
		return {
			message: "Mercury signed you in, but this browser didn't keep the session.",
			hint: 'Open Mercury at its https:// address (on a phone, the tailnet address your admin gave you), then sign in again.'
		};
	}
	if (!(error instanceof ApiError)) return describe(error);
	switch (error.code) {
		case 'unauthenticated':
			return {
				message: "That token isn't valid.",
				hint: 'Check that you pasted all of it, with nothing before or after. It may also have been revoked: if it still fails, ask your admin for a new one.'
			};
		case 'token_expired':
			return { message: 'That token has expired.', hint: 'Ask your admin for a new one.' };
		case 'rate_limited':
			return {
				message: 'Too many sign-in attempts.',
				hint: `${wait(error.retryAfter)}, then try again.`
			};
		case 'invalid_request':
			// 403: the server only opens a session for the page it serves
			return error.status === 403
				? {
						message: 'Mercury only signs you in from its own web page.',
						hint: error.hint ?? 'Open Mercury from the address your admin gave you, then try again.'
					}
				: { message: "The sign-in couldn't be sent.", hint: error.hint };
		default:
			return describe(error);
	}
}

/**
 * A question that started and then failed: the `error` event's code, in words. The event's own
 * message is for the audit; what to do about it is the same whatever it said.
 */
export function describeFailure(code: ErrorCode): Described {
	switch (code) {
		case 'judge_unavailable':
			return {
				message: "Mercury's safety checks are unavailable, so it can't answer right now.",
				hint: 'Try again in a few minutes.'
			};
		case 'probe_unavailable':
			return {
				message: "Mercury can't read this project's repository right now.",
				hint: TRY_AGAIN
			};
		case 'budget_exceeded':
			// in the stream it is the question's own cap; the day's budget refuses the ask itself (429)
			return {
				message: 'This question reached its spending limit, so Mercury stopped it.',
				hint: 'Ask something narrower, about one project or a shorter period.'
			};
		case 'rate_limited':
			return { message: 'Too many requests.', hint: 'Wait a minute, then try again.' };
		default:
			return { message: 'Something went wrong on the server.', hint: TRY_AGAIN };
	}
}

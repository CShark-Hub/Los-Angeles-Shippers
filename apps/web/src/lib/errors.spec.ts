import { describe as suite, expect, it } from 'vitest';

import { ApiError } from '$lib/api/transport';
import { SessionNotKept, describe, describeFailure, describeSignIn } from './errors';

const refused = (
	status: number,
	code: ConstructorParameters<typeof ApiError>[1],
	hint?: string,
	wait?: number
) => new ApiError(status, code, 'The core said so.', hint, wait);

suite('describeSignIn', () => {
	it('tells a session the browser did not keep from a wrong token, and how to get one kept', () => {
		const said = describeSignIn(new SessionNotKept());
		expect(said.message).toMatch(/didn't keep the session/);
		expect(said.hint).toMatch(/https:\/\//);
		expect(said.hint).toMatch(/tailnet/);
		expect(said.message).not.toMatch(/token isn't valid/);
	});

	it('says the token is wrong, and what to check, in plain words', () => {
		const said = describeSignIn(refused(401, 'unauthenticated'));
		expect(said.message).toBe("That token isn't valid.");
		expect(said.hint).toMatch(/pasted all of it/);
		expect(said.hint).toMatch(/revoked/);
	});

	it('says an expired token has expired', () => {
		expect(describeSignIn(refused(401, 'token_expired'))).toEqual({
			message: 'That token has expired.',
			hint: 'Ask your admin for a new one.'
		});
	});

	it('says how long to wait after too many attempts', () => {
		expect(describeSignIn(refused(429, 'rate_limited', undefined, 6)).hint).toBe(
			'Wait 6 seconds, then try again.'
		);
		expect(describeSignIn(refused(429, 'rate_limited', undefined, 1)).hint).toBe(
			'Wait 1 second, then try again.'
		);
		expect(describeSignIn(refused(429, 'rate_limited')).hint).toBe(
			'Wait a minute, then try again.'
		);
	});

	it("says Mercury signs in only from its own page when the core refuses the page's origin", () => {
		const said = describeSignIn(refused(403, 'invalid_request', 'Use Mercury at its own address.'));
		expect(said.message).toBe('Mercury only signs you in from its own web page.');
		expect(said.hint).toBe('Use Mercury at its own address.');
		expect(describeSignIn(refused(403, 'invalid_request')).hint).toMatch(/address your admin gave/);
	});

	it('says when Mercury can not be reached, and when the server failed', () => {
		expect(describeSignIn(refused(0, 'internal')).message).toBe("Mercury can't be reached.");
		expect(describeSignIn(refused(500, 'internal')).message).toBe(
			'Something went wrong on the server.'
		);
		expect(describeSignIn(refused(502, 'internal')).hint).toMatch(/tell your admin/);
	});

	it('never shows what is not an API error, which may be technical', () => {
		const said = describeSignIn(new TypeError('Cannot read properties of undefined'));
		expect(JSON.stringify(said)).not.toMatch(/properties/);
	});
});

suite('describe', () => {
	it("shows the core's own words for a refusal it explains", () => {
		const said = describe(refused(403, 'forbidden_project', 'Ask about a project you were given.'));
		expect(said).toEqual({
			message: 'The core said so.',
			hint: 'Ask about a project you were given.'
		});
	});

	it('sends a lost session back to sign in', () => {
		expect(describe(refused(401, 'unauthenticated'))).toEqual({
			message: 'Your session has ended.',
			hint: 'Sign in again.'
		});
	});
});

suite('describeFailure', () => {
	it('says what failed in words, never the code', () => {
		for (const code of [
			'judge_unavailable',
			'probe_unavailable',
			'budget_exceeded',
			'internal'
		] as const) {
			const said = describeFailure(code);
			expect(said.message).not.toContain('_');
			expect(said.hint).toBeTruthy();
		}
		// in the stream it is the question's own cap; the day's budget is POST /v1/questions' 429
		expect(describeFailure('budget_exceeded').message).toMatch(
			/question reached its spending limit/
		);
		expect(describeFailure('budget_exceeded').hint).toMatch(/narrower/);
		expect(JSON.stringify(describeFailure('budget_exceeded'))).not.toMatch(/today|tomorrow/i);
		expect(describeFailure('probe_unavailable').message).toMatch(/repository/);
	});

	it('does not guess at a code it does not know', () => {
		expect(describeFailure('invalid_request').message).toBe('Something went wrong on the server.');
	});
});

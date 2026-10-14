import { describe, expect, it } from 'vitest';
import { headerPolicy, parsePolicy, snippet } from './csp-caddy.js';

const page = (policy) =>
	`<!doctype html><head><meta http-equiv="content-security-policy" content="${policy}"></head>`;
const POLICY =
	"default-src 'self'; script-src 'self' 'sha256-abc='; style-src 'self' 'unsafe-inline'; img-src 'self' data:";

describe('the header made from a built page', () => {
	it('is the page’s own policy with the script hash, and frame-ancestors added', () => {
		const header = headerPolicy(page(POLICY));
		expect(parsePolicy(header)).toEqual({
			...parsePolicy(POLICY),
			'frame-ancestors': ["'none'"]
		});
	});

	it('refuses a page that carries no policy', () => {
		expect(() => headerPolicy('<!doctype html><head></head>')).toThrow(
			/no Content Security Policy/
		);
	});

	it('refuses a policy with no script hash, which would stop the app’s own bootstrap script', () => {
		expect(() => headerPolicy(page("default-src 'self'; script-src 'self'"))).toThrow(
			/no script hash/
		);
	});
});

describe('the Caddy snippet', () => {
	it('is one header line holding the policy', () => {
		expect(snippet("default-src 'self'")).toBe(
			'header Content-Security-Policy "default-src \'self\'"\n'
		);
	});

	it('refuses a policy Caddy would read as syntax, instead of writing a broken file', () => {
		expect(() => snippet('default-src "self"')).toThrow(/syntax/);
		expect(() => snippet('default-src {env.X}')).toThrow(/syntax/);
	});
});

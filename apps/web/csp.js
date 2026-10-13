// The page's Content Security Policy, in one place: `vite.config.ts` hands the directives to SvelteKit,
// which adds the hash of each inline script it writes (in `hash` mode) and puts the policy in the
// pages' <meta> tag; `scripts/csp-caddy.js` then writes the same policy, whole, as the header
// Caddy sends. Nothing outside the app's own origin may load or be connected to.

/**
 * What SvelteKit puts in the meta tag, and adds the hashes to.
 * @type {NonNullable<NonNullable<import('@sveltejs/kit').KitConfig['csp']>['directives']>}
 */
export const directives = {
	'default-src': ['self'],
	'script-src': ['self'],
	// Svelte sets inline styles on elements, which only `unsafe-inline` allows; no script runs from one
	'style-src': ['self', 'unsafe-inline'],
	'img-src': ['self', 'data:'],
	'font-src': ['self'],
	'connect-src': ['self'],
	'object-src': ['none'],
	'base-uri': ['self'],
	'form-action': ['self']
};

/** What only a header can say: a <meta> tag's policy ignores it. */
export const headerOnly = { 'frame-ancestors': ['none'] };

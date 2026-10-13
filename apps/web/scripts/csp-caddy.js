// Run by `pnpm build` after the build: writes `csp.caddy`, the Caddy snippet that holds the page's
// whole Content Security Policy as a header. SvelteKit's hash mode puts the hash of each inline script
// it wrote into a <meta> tag of the page; the hashes change with every build, so the header is made
// from that tag, plus what only a header can say (`frame-ancestors`). The file sits beside `build/`,
// never inside it, so it is not served. The `mercury-web` image copies it to /etc/caddy/csp.caddy and
// `just demo-local` mounts it; the Caddyfile imports it.

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { headerOnly } from '../csp.js';

const META = /<meta\s+http-equiv="content-security-policy"\s+content="([^"]*)"\s*\/?>/i;

/** The directive's tokens in the order given: `{ 'script-src': ["'self'", "'sha256-…'"], … }`. */
export function parsePolicy(policy) {
	const parsed = {};
	for (const part of policy.split(';')) {
		const [name, ...sources] = part.trim().split(/\s+/);
		if (name) parsed[name] = sources;
	}
	return parsed;
}

/**
 * The header's policy for a built page: the page's own policy and the directives only a header can
 * have. Throws when the page has no policy, or no script hash (an inline script would then not run),
 * so a build never ships a policy that silently lets nothing, or everything, through.
 */
export function headerPolicy(html) {
	const found = META.exec(html);
	if (!found) throw new Error('the built page has no Content Security Policy <meta> tag');
	const policy = parsePolicy(found[1].replaceAll('&#39;', "'"));
	if (!policy['script-src']?.some((source) => source.startsWith("'sha256-"))) {
		throw new Error('the page’s policy has no script hash: kit.csp must be in hash mode');
	}
	for (const [name, sources] of Object.entries(headerOnly)) {
		policy[name] = sources.map((source) => (source.startsWith('data:') ? source : `'${source}'`));
	}
	return Object.entries(policy)
		.map(([name, sources]) => [name, ...sources].join(' '))
		.join('; ');
}

/** The Caddy snippet for a policy. */
export function snippet(policy) {
	if (/["{}\\]/.test(policy))
		throw new Error('the policy holds a character Caddy would read as syntax');
	return `header Content-Security-Policy "${policy}"\n`;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
	const html = readFileSync(new URL('../build/index.html', import.meta.url), 'utf8');
	writeFileSync(new URL('../csp.caddy', import.meta.url), snippet(headerPolicy(html)));
}

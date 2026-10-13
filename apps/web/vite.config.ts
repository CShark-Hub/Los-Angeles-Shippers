import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vitest/config';
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { directives } from './csp.js';

export default defineConfig({
	plugins: [
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			// a single-page app: every unknown path gets index.html, and the router takes over
			adapter: adapter({ fallback: 'index.html' }),
			// hash mode: the page is static, so there is no server to hand out a nonce; `pnpm build` turns the
			// policy the page carries into csp.caddy, the header Caddy sends (scripts/csp-caddy.js)
			csp: { mode: 'hash', directives }
		})
	],
	server: {
		// Development against a local core (`just web-dev proxy`): /v1 goes to it, so the page and the
		// API share one origin, as they do behind Caddy. The core has to know the page's origin:
		// MERCURY_PUBLIC_ORIGIN=http://localhost:5173. Mock mode never makes the request.
		proxy: {
			'/v1': { target: process.env.MERCURY_CORE_URL ?? 'http://localhost:8080', xfwd: true }
		}
	},
	test: {
		expect: { requireAssertions: true },
		projects: [
			{
				extends: './vite.config.ts',
				test: {
					name: 'server',
					environment: 'node',
					include: ['src/**/*.{test,spec}.{js,ts}', 'scripts/**/*.spec.js'],
					exclude: ['src/**/*.svelte.{test,spec}.{js,ts}']
				}
			}
		]
	}
});

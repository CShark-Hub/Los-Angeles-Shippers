import { redirect } from '@sveltejs/kit';

import { applyTheme, loadTheme } from '$lib/preferences';
import { session } from '$lib/session.svelte';
import type { LayoutLoad } from './$types';

// A single-page app: the browser renders everything, and the core is only ever called from it.
export const ssr = false;

/**
 * Before any screen: the look the person picked, then whose browser this is. Whoever is signed out
 * is sent to Sign in, and whoever is signed in has no business there.
 */
export const load: LayoutLoad = async ({ url }) => {
	applyTheme(loadTheme());
	await session.restore();
	if (session.status === 'signed-out' && url.pathname !== '/signin') redirect(307, '/signin');
	if (session.status === 'signed-in' && url.pathname === '/signin') redirect(307, '/');
};

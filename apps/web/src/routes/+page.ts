import { redirect } from '@sveltejs/kit';

import type { PageLoad } from './$types';

// There is nothing at the root: the first screen is Ask. (Sign in comes first for whoever is
// signed out, from the layout.)
export const load: PageLoad = () => redirect(307, '/ask');

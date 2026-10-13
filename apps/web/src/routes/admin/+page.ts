import { redirect } from '@sveltejs/kit';

import type { PageLoad } from './$types';

// The admin panel opens on its overview.
export const load: PageLoad = () => redirect(307, '/admin/overview');

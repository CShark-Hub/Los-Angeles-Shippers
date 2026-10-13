/**
 * What a page adds to the app's side panel, under the main links: the Ask page puts its list of
 * questions there. A page sets it when it opens and takes it back when it closes.
 */

import type { Snippet } from 'svelte';

class Shell {
	panel = $state.raw<Snippet | null>(null);
}

export const shell = new Shell();

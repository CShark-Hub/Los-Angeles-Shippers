import { describe, expect, it } from 'vitest';

import { ApiError, type Transport } from '$lib/api';
import { Projects } from './projects.svelte';

const delta = {
	project: 'delta',
	display_name: 'Delta — invoicing',
	modules: ['Invoicing'],
	classes: ['progress' as const],
	probe: null
};

function projectsOn(get: Transport['get']) {
	return new Projects(async () => ({ get }) as Transport);
}

describe('Projects', () => {
	it('lists what the core says, and finds a project by its id', async () => {
		const projects = projectsOn((async () => ({ projects: [delta] })) as Transport['get']);
		expect(projects.list).toBeNull();
		await projects.load();
		expect(projects.list).toEqual([delta]);
		expect(projects.find('delta')).toEqual(delta);
		expect(projects.find('nope')).toBeUndefined();
		expect(projects.find(null)).toBeUndefined();
	});

	it('says in words when the list could not be fetched, and recovers on the next try', async () => {
		let up = false;
		const projects = projectsOn((async () => {
			if (!up) throw new ApiError(0, 'internal', "The core can't be reached.");
			return { projects: [delta] };
		}) as Transport['get']);

		await projects.load();
		expect(projects.list).toBeNull();
		expect(projects.failure?.message).toBe("Mercury can't be reached.");

		up = true;
		await projects.load();
		expect(projects.failure).toBeNull();
		expect(projects.list).toHaveLength(1);
	});
});

/** The projects the person may ask about, and what they may ask: `GET /v1/projects`. */

import { getTransport, type ProjectList, type ProjectSummary, type Transport } from '$lib/api';
import { describe, type Described } from '$lib/errors';

export class Projects {
	/** Null until the list has been fetched. */
	list = $state<ProjectSummary[] | null>(null);
	failure = $state<Described | null>(null);

	#api: () => Promise<Transport>;

	constructor(api: () => Promise<Transport> = getTransport) {
		this.#api = api;
	}

	async load(): Promise<void> {
		try {
			const api = await this.#api();
			this.list = (await api.get<ProjectList>('/v1/projects')).projects;
			this.failure = null;
		} catch (error) {
			this.failure = describe(error);
		}
	}

	find(id: string | null | undefined): ProjectSummary | undefined {
		return id ? this.list?.find((project) => project.project === id) : undefined;
	}
}

/** The app's one list of projects. */
export const projects = new Projects();

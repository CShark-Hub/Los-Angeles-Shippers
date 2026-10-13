/**
 * The admin panel's state: one read of `GET /v1/admin/…` per screen, kept until the admin asks
 * again. The panel is an admin's alone: the core refuses anyone else with a `403` before it reads
 * anything, and the screens ask for nothing unless the person is an admin. A `403` all the same
 * means the person's role changed since the app last asked, so what was on screen goes and the
 * session reads whose it is again; the side panel follows its answer.
 *
 * Every read is an audit row on the core, so a screen reads once when it opens and again only when
 * the admin presses Refresh or picks another window: nothing polls.
 */

import {
	ApiError,
	getTransport,
	type AdminOverview,
	type AdminPeople,
	type AdminRepositories,
	type OverviewWindow,
	type Transport
} from '$lib/api';
import { describe, type Described } from '$lib/errors';
import { nowAsText } from '$lib/format';
import { session } from '$lib/session.svelte';

export const WINDOWS: readonly { value: OverviewWindow; label: string; long: string }[] = [
	{ value: '24h', label: '24 h', long: 'the last 24 hours' },
	{ value: '7d', label: '7 days', long: 'the last 7 days' },
	{ value: '30d', label: '30 days', long: 'the last 30 days' }
];

/** The window an address names, or the core's default. */
export function windowOf(raw: string | null): OverviewWindow {
	return WINDOWS.find((w) => w.value === raw)?.value ?? '7d';
}

/** One read of an admin route: what came, when, and what went wrong. */
export class AdminRead<T> {
	data = $state.raw<T | null>(null);
	loading = $state(false);
	failure = $state<Described | null>(null);
	/** When the data on screen was read, as an RFC 3339 time. */
	readAt = $state<string | null>(null);

	#api: () => Promise<Transport>;
	#roleChanged: () => Promise<void>;
	/** Bumped by each read, so a late answer to an earlier one is dropped. */
	#generation = 0;

	constructor(
		api: () => Promise<Transport> = getTransport,
		roleChanged: () => Promise<void> = () => session.refresh()
	) {
		this.#api = api;
		this.#roleChanged = roleChanged;
	}

	async load(path: string): Promise<void> {
		const generation = ++this.#generation;
		this.loading = true;
		try {
			const api = await this.#api();
			const data = await api.get<T>(path);
			if (generation !== this.#generation) return;
			this.data = data;
			this.readAt = nowAsText();
			this.failure = null;
		} catch (error) {
			if (generation !== this.#generation) return;
			this.failure = describe(error);
			if (error instanceof ApiError && error.status === 403) {
				this.data = null;
				await this.#roleChanged();
			}
		} finally {
			if (generation === this.#generation) this.loading = false;
		}
	}
}

export function overviewPath(window: OverviewWindow): string {
	return `/v1/admin/overview?window=${encodeURIComponent(window)}`;
}

export const PEOPLE_PATH = '/v1/admin/people';
export const REPOSITORIES_PATH = '/v1/admin/repositories';

export type OverviewRead = AdminRead<AdminOverview>;
export type PeopleRead = AdminRead<AdminPeople>;
export type RepositoriesRead = AdminRead<AdminRepositories>;

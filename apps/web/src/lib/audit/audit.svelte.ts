/**
 * The Audit screen's state: `GET /v1/audit` with the admin's filters, page after page by its
 * cursor, and the chain's status over the rows loaded. The audit is an admin's alone: the core
 * refuses anyone else with a `403` before a row is read, and the screen asks for nothing unless the
 * person is an admin. A `403` all the same means the person's role changed since the app last asked
 * (an admin who is one no more), so the rows on screen go and the session reads whose it is again;
 * the screen and the side panel follow its answer.
 *
 * The export is a link the browser downloads, so the file streams to disk without passing through
 * the page. A download can't tell the page why it failed, so the export's filters are first tried on
 * one row through the app's transport: a refusal is put in words on the screen (a `401` ends the
 * session as on any route, a `403` reads the role again, a server's failure says to try again), and
 * only a check that passed lets the browser download.
 */

import { ApiError, getTransport, type AuditItem, type AuditPage, type Transport } from '$lib/api';
import { describe, type Described } from '$lib/errors';
import { session } from '$lib/session.svelte';
import { checkChain, type ChainStatus } from './chain';
import { exportCheckPath, noFilters, pagePath, type AuditFilters } from './query';

export class AuditView {
	filters = $state<AuditFilters>(noFilters());
	/** The filters the rows on screen were loaded with: the export follows them, not the form. */
	applied = $state<AuditFilters>(noFilters());
	items = $state<AuditItem[]>([]);
	/** Null until the first page has come. */
	loaded = $state(false);
	loading = $state(false);
	next = $state<string | null>(null);
	chain = $state<ChainStatus>({ state: 'empty' });
	failure = $state<Described | null>(null);
	/** While the export's filters are being tried, before the browser downloads. */
	exporting = $state(false);
	/** Why the last export didn't start. */
	exportFailure = $state<Described | null>(null);

	#api: () => Promise<Transport>;
	#roleChanged: () => Promise<void>;
	/** Bumped by each new search, so a late page of an earlier one is dropped. */
	#generation = 0;

	/**
	 * `roleChanged` hears of an audit `403`: by default the app's session reads whose it is again,
	 * so someone who is no admin any more loses the screen and its link.
	 */
	constructor(
		api: () => Promise<Transport> = getTransport,
		roleChanged: () => Promise<void> = () => session.refresh()
	) {
		this.#api = api;
		this.#roleChanged = roleChanged;
	}

	/** The first page with the filters as they are in the form. */
	async search(): Promise<void> {
		this.#generation += 1;
		this.applied = { ...this.filters };
		this.items = [];
		this.next = null;
		this.loaded = false;
		this.exportFailure = null;
		await this.#load(null, this.#generation);
	}

	/** The page after the rows on screen. */
	async more(): Promise<void> {
		if (this.next === null || this.loading) return;
		await this.#load(this.next, this.#generation);
	}

	/** The form back to no filter, and every row the person may see. */
	async clear(): Promise<void> {
		this.filters = noFilters();
		await this.search();
	}

	/**
	 * Tries the export's filters on one row, before the browser downloads every matching row: true
	 * when the download may start, false when it was refused, with the reason in `exportFailure`.
	 */
	async checkExport(): Promise<boolean> {
		if (this.exporting) return false;
		this.exporting = true;
		this.exportFailure = null;
		try {
			const api = await this.#api();
			await api.get<AuditPage>(exportCheckPath(this.applied));
			return true;
		} catch (error) {
			this.exportFailure = describe(error);
			await this.#refused(error);
			return false;
		} finally {
			this.exporting = false;
		}
	}

	async #load(cursor: string | null, generation: number): Promise<void> {
		this.loading = true;
		try {
			const api = await this.#api();
			const page = await api.get<AuditPage>(pagePath(this.applied, cursor));
			if (generation !== this.#generation) return;
			const items = [...this.items, ...page.items];
			const chain = await checkChain(items);
			if (generation !== this.#generation) return;
			this.items = items;
			this.next = page.next;
			this.chain = chain;
			this.loaded = true;
			this.failure = null;
		} catch (error) {
			if (generation !== this.#generation) return;
			this.failure = describe(error);
			await this.#refused(error);
		} finally {
			if (generation === this.#generation) this.loading = false;
		}
	}

	/** An audit `403`: the person is no admin (any more), so no row stays and the role is read again. */
	async #refused(error: unknown): Promise<void> {
		if (!(error instanceof ApiError && error.status === 403)) return;
		this.items = [];
		this.next = null;
		this.loaded = false;
		this.chain = { state: 'empty' };
		await this.#roleChanged();
	}
}

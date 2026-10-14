/**
 * The answers waiting for the person's sign-off: `GET /v1/approvals?status=pending`, asked again
 * every 30 seconds while a tab is open, a badge with how many the person can decide on, and one
 * browser notification when new ones arrive, once the person has allowed notifications. The
 * notification says only that something waits, never what: a draft stays on the page. Nothing
 * here is kept in browser storage.
 *
 * A held draft is shown to an approver, never to the asker before the decision. The list also
 * holds the person's own questions when they approve the project too, and the core sends those
 * without their drafts and flags; each is kept here without them whatever a list brings, so
 * nothing on screen can show them.
 */

import { SvelteSet } from 'svelte/reactivity';

import {
	getTransport,
	type Answer,
	type Approval,
	type ApprovalDecision,
	type ApprovalList,
	type Decision,
	type Flag,
	type Me,
	type Transport
} from '$lib/api';
import { describe, type Described } from '$lib/errors';

/** How often the list is asked again while a tab is open. */
export const POLL_MS = 30_000;
/** The longest note, as the core takes it. */
export const MAX_NOTE = 500;

/**
 * A held answer as the Approvals screen keeps it: one for the person to decide on, with its draft
 * and flags; or their own question, with neither (`draft` is `null`), since it waits for another
 * approver and its draft is never the asker's to read before the decision. Someone else's item
 * that came without a draft keeps `null` too, and offers no decision: nobody approves unread.
 */
export type Held = Omit<Approval, 'draft' | 'flags'> & {
	draft: Answer | null;
	flags: Flag[];
};

/** A held answer as the core lists it, made what the screen may keep: see [`Held`]. */
export function held(item: Approval): Held {
	const { draft, flags, ...rest } = item;
	// the core leaves both out of the person's own question; were they sent, they are dropped here
	if (item.asked_by_me) return { ...rest, draft: null, flags: [] };
	return { ...rest, draft: draft ?? null, flags: flags ?? [] };
}

/** Whether the person approves any project, so the Approvals screen is theirs. */
export function isApprover(me: Me | null | undefined): boolean {
	return me?.projects.some((grant) => grant.approver) ?? false;
}

/** A note as the core takes it: trimmed, 1 to 500 characters, on one line; or why not. */
export function noteProblem(note: string): string | null {
	const trimmed = note.trim();
	if (trimmed === '') return 'Write what the asker should do instead; they see the note.';
	if ([...trimmed].length > MAX_NOTE) return `Keep the note to ${MAX_NOTE} characters.`;
	// every control character, C1 ones included, as the core refuses them (`char::is_control`)
	// eslint-disable-next-line no-control-regex
	if (/[\u0000-\u001f\u007f-\u009f]/.test(trimmed)) return 'Write the note on one line.';
	return null;
}

export type NotificationPermissionState = 'unsupported' | NotificationPermission;

export interface Notifier {
	permission(): NotificationPermissionState;
	request(): Promise<NotificationPermissionState>;
	/** Says that `count` new answers wait; nothing of what they say. */
	show(count: number): void;
}

/** The browser's Notification API, when it has one. */
export const browserNotifier: Notifier = {
	permission: () => ('Notification' in globalThis ? Notification.permission : 'unsupported'),
	async request() {
		if (!('Notification' in globalThis)) return 'unsupported';
		return Notification.requestPermission();
	},
	show(count) {
		if (!('Notification' in globalThis) || Notification.permission !== 'granted') return;
		new Notification('Mercury', {
			body:
				count === 1
					? 'An answer waits for your sign-off.'
					: `${count} answers wait for your sign-off.`,
			tag: 'mercury-approvals'
		});
	}
};

export interface ApprovalsOptions {
	api?: () => Promise<Transport>;
	pollMs?: number;
	notifier?: Notifier;
}

export class Approvals {
	items = $state<Held[]>([]);
	/** Whether the list has come once. */
	loaded = $state(false);
	failure = $state<Described | null>(null);
	/** The id being decided on, while its request is on its way. */
	deciding = $state<string | null>(null);
	/** What the last decision did, or why it didn't. */
	outcome = $state<{ id: string; decision: Decision } | null>(null);
	decisionFailure = $state<{ id: string; failure: Described } | null>(null);
	permission = $state<NotificationPermissionState>('unsupported');

	/** The answers the person can decide on: their own questions wait for someone else. */
	decidable = $derived(this.items.filter((item) => !item.asked_by_me));

	#api: () => Promise<Transport>;
	#pollMs: number;
	#notifier: Notifier;
	#timer: ReturnType<typeof setInterval> | null = null;
	/** Every id the person has been told of; new ones are the ones not in it. */
	#seen = new SvelteSet<string>();

	constructor({
		api = getTransport,
		pollMs = POLL_MS,
		notifier = browserNotifier
	}: ApprovalsOptions = {}) {
		this.#api = api;
		this.#pollMs = pollMs;
		this.#notifier = notifier;
		this.permission = notifier.permission();
	}

	/** Asks now, then every 30 seconds until [`Approvals.stop`]. Starting twice changes nothing. */
	start(): void {
		if (this.#timer !== null) return;
		void this.refresh();
		this.#timer = setInterval(() => void this.refresh(), this.#pollMs);
	}

	/** Stops asking and forgets the list: the session ended, or the person approves nothing. */
	stop(): void {
		if (this.#timer !== null) clearInterval(this.#timer);
		this.#timer = null;
		this.items = [];
		this.loaded = false;
		this.failure = null;
		this.outcome = null;
		this.decisionFailure = null;
		this.#seen.clear();
	}

	async refresh(): Promise<void> {
		// a new list is a new look: the reason of a decision that failed has been read by then
		this.decisionFailure = null;
		try {
			const api = await this.#api();
			const list = await api.get<ApprovalList>('/v1/approvals?status=pending');
			// the first list is what was waiting already; only later arrivals are news
			const fresh = list.approvals.filter((item) => !item.asked_by_me && !this.#seen.has(item.id));
			if (this.loaded && fresh.length > 0) this.#notifier.show(fresh.length);
			for (const item of list.approvals) this.#seen.add(item.id);
			// the person's own questions without their drafts, before anything renders the list
			this.items = list.approvals.map(held);
			this.loaded = true;
			this.failure = null;
		} catch (error) {
			this.failure = describe(error);
		}
	}

	/** Approves the draft unchanged, or rejects it with a note the asker reads. */
	async decide(id: string, decision: Decision, note?: string): Promise<void> {
		if (this.deciding !== null) return;
		const body: ApprovalDecision = { decision };
		if (decision === 'reject') {
			const problem = noteProblem(note ?? '');
			if (problem) {
				this.decisionFailure = { id, failure: { message: problem } };
				return;
			}
			body.note = (note ?? '').trim();
		}
		this.deciding = id;
		this.decisionFailure = null;
		this.outcome = null;
		try {
			const api = await this.#api();
			await api.post(`/v1/approvals/${encodeURIComponent(id)}`, body);
			this.items = this.items.filter((item) => item.id !== id);
			this.outcome = { id, decision };
		} catch (error) {
			const failure = describe(error);
			// decided by someone else, or expired: the list says what is left, and the reason stays
			// on screen above it, since the item's card may be gone with it
			await this.refresh();
			this.decisionFailure = { id, failure };
		} finally {
			this.deciding = null;
		}
	}

	/** Asks the browser to allow notifications; the browser keeps the answer, not the app. */
	async allowNotifications(): Promise<void> {
		this.permission = await this.#notifier.request();
	}
}

/** The app's one list of answers waiting for sign-off. */
export const approvals = new Approvals();

/**
 * The Ask screen's state: one question at a time, from the person pressing Ask to the card they
 * read. It sends the question, follows its stream, keeps what the events say and nothing else, and
 * finds its way back after a reload or a dropped connection.
 *
 * Two rules it keeps for the rest of the app:
 * - Text is shown only from the `answer` event (or the core's state of a delivered question, which
 *   is that same event). Nothing else the stream carries is model text, and `entries` is what the
 *   timeline is built from.
 * - Nothing is stored in the browser. The question's id is all that has to survive a reload, and
 *   the page keeps it in its address. The words of a question live in the tab's memory only (the
 *   side panel's list, `history.svelte.ts`), so a question found again after a reload has none.
 */

import {
	ApiError,
	getTransport,
	isTerminal,
	type Accepted,
	type Answer,
	type Clarify,
	type ClarifyAnswer,
	type Ending,
	type NewQuestion,
	type PendingSignoff,
	type ProjectPicked,
	type QuestionState,
	type Refusal,
	type StreamEvent,
	type Transport
} from '$lib/api';
import { describe, describeFailure, type Described } from '$lib/errors';
import { session } from '$lib/session.svelte';

import { callsTheCodeReader } from './timeline';

export type Phase =
	/** Nothing asked. */
	| 'idle'
	/** The question is on its way to the core. */
	| 'sending'
	/** A question from an earlier visit is being fetched. */
	| 'loading'
	/** Accepted, and its stream is open. */
	| 'running'
	/** The stream broke while the question was still running. */
	| 'disconnected'
	/** The question ended with a choice for the person to make. */
	| 'clarify'
	/** The answer waits for an approver. */
	| 'pending_signoff'
	| 'delivered'
	| 'refused'
	/** The question could not be sent, or started and failed. */
	| 'failed'
	/** The id of a question that is not the person's, or does not exist. */
	| 'unavailable'
	/** The question ended, but what came of it is no longer kept: the retention emptied it. */
	| 'expired';

/** An event and when it arrived, in milliseconds after the question was sent (not for a replay). */
export interface Entry {
	event: StreamEvent;
	at: number | null;
}

export interface ConversationOptions {
	api?: () => Promise<Transport>;
	/** How often a question held for sign-off is asked about, in milliseconds. */
	pollMs?: number;
	now?: () => number;
	/** A fresh `Idempotency-Key`. */
	newKey?: () => string;
}

/** What `GET /v1/questions/{id}` says about how a question ended, in the shape of `done`. */
function doneOf(state: QuestionState): Ending | null {
	if (state.duration_ms === null || state.audit_id === null || state.judge_mode === null) {
		return null;
	}
	return {
		duration_ms: state.duration_ms,
		cost_usd: state.cost_usd,
		judge_mode: state.judge_mode,
		audit_id: state.audit_id
	};
}

/**
 * The message of the core's `400` for a `follows` it won't link (another person's question, one an
 * approver didn't reject, an unknown id): fixed and the same for each, so it tells this refusal
 * from any other `400`.
 */
export const FOLLOWS_REFUSED =
	"The question to ask again isn't one of yours that an approver rejected.";

/** Whether a send failed because the core won't link the question to the one it asks again. */
function refusesFollows(error: unknown): boolean {
	return (
		error instanceof ApiError &&
		error.status === 400 &&
		error.code === 'invalid_request' &&
		error.message === FOLLOWS_REFUSED
	);
}

/** How a question ended, as its state says it. */
export type Settled =
	| { phase: 'delivered'; answer: Answer; done: Ending | null }
	| { phase: 'refused'; refusal: Refusal }
	| { phase: 'failed' }
	| { phase: 'expired' };

/**
 * How a question ended, from `GET /v1/questions/{id}`; none while it is still going or waits for
 * a choice or an approver. A question delivered or refused whose answer or refusal the state no
 * longer holds was emptied by the retention (the core writes its status with its last event, so an
 * ended question always had them): what came of it is no longer kept.
 */
export function settledOf(state: QuestionState): Settled | null {
	switch (state.status) {
		case 'delivered':
			return state.answer
				? { phase: 'delivered', answer: state.answer, done: doneOf(state) }
				: { phase: 'expired' };
		case 'refused':
			return state.refusal ? { phase: 'refused', refusal: state.refusal } : { phase: 'expired' };
		case 'failed':
			return { phase: 'failed' };
		default:
			return null;
	}
}

export class Conversation {
	phase = $state<Phase>('idle');
	questionId = $state<string | null>(null);
	/**
	 * Whether the core knows the question on screen: it accepted it, or said where it stands. A
	 * question fetched by its id is not known until the core answers, and one it never had never is.
	 */
	known = $state(false);
	/** What was asked. Empty for a question that came back by its address alone: the browser doesn't keep its words. */
	text = $state('');
	entries = $state<Entry[]>([]);
	project = $state<ProjectPicked | null>(null);
	answer = $state<Answer | null>(null);
	refusal = $state<Refusal | null>(null);
	clarify = $state<Clarify | null>(null);
	signoff = $state<PendingSignoff | null>(null);
	done = $state<Ending | null>(null);
	failure = $state<Described | null>(null);
	/**
	 * The question an approver rejected that the next one asks again, once the person chose to: it
	 * goes with that question as `follows`, so the audit links the two.
	 */
	following = $state<string | null>(null);

	/** Text values the data checks withheld, over every call of the question. */
	withheld = $derived(
		this.entries.reduce(
			(sum, { event }) =>
				sum + (event.event === 'tool_result' ? event.data.withheld_fields.length : 0),
			0
		)
	);
	/** Sentences the output checks removed. */
	stripped = $derived(
		this.entries.reduce(
			(sum, { event }) => sum + (event.event === 'judged' ? event.data.stripped : 0),
			0
		)
	);
	held = $derived(
		this.entries.reduce(
			(sum, { event }) => sum + (event.event === 'judged' ? event.data.held : 0),
			0
		)
	);
	/** Whether part of the answer comes from the project's code reader: the plan called it. */
	fromCode = $derived(callsTheCodeReader(this.entries.map(({ event }) => event)));

	#api: () => Promise<Transport>;
	#pollMs: number;
	#now: () => number;
	#newKey: () => string;

	/** Bumped whenever the screen moves on, so a late answer to an old question is dropped. */
	#generation = 0;
	#stop: (() => void) | null = null;
	#timer: ReturnType<typeof setTimeout> | null = null;
	#started = 0;
	#timed = true;
	#lastSeq = 0;
	#project: string | null = null;
	/** The key of the last send and what it was a send of: a retry keeps it, a new question doesn't. */
	#key = '';
	#keyFor = '';
	/** Whether a drop's question to the core is on its way, so the browser's retries ask once at a time. */
	#asking = false;

	constructor({
		api = getTransport,
		pollMs = 5000,
		now = () => performance.now(),
		newKey = () => crypto.randomUUID()
	}: ConversationOptions = {}) {
		this.#api = api;
		this.#pollMs = pollMs;
		this.#now = now;
		this.#newKey = newKey;
	}

	/** Whether a question is on its way, being answered or waiting for an approver. */
	get busy(): boolean {
		return ['sending', 'loading', 'running', 'disconnected', 'pending_signoff'].includes(
			this.phase
		);
	}

	/** Whether the options of a clarify can be answered: it needs the text of the question. */
	get canChoose(): boolean {
		return this.phase === 'clarify' && this.text !== '' && this.questionId !== null;
	}

	/** Whether the question on screen is one an approver rejected, which its asker may ask again. */
	get canAskAgain(): boolean {
		return (
			this.phase === 'refused' &&
			this.refusal?.code === 'rejected_by_approver' &&
			this.questionId !== null
		);
	}

	/**
	 * Sends a question and follows its events. `project` is the person's pick, or none. After
	 * [`Conversation.askAgain`], it names the rejected question as `follows`.
	 */
	async ask(text: string, project: string | null): Promise<void> {
		await this.#send(text, project, undefined, this.following ?? undefined);
	}

	/**
	 * The person asks again the question an approver rejected: the next question they send names it.
	 * Gives the rejected question's words, to put back in the box for them to send as they are or
	 * change; empty when this tab no longer has them.
	 */
	askAgain(): string {
		if (!this.canAskAgain) return '';
		this.following = this.questionId;
		return this.text;
	}

	/** Answers a clarify: the same question again, with the option picked. */
	async choose(option: string): Promise<void> {
		if (!this.canChoose || this.questionId === null) return;
		await this.#send(this.text, this.#project, { of: this.questionId, option });
	}

	async #send(
		text: string,
		project: string | null,
		clarify?: ClarifyAnswer,
		follows?: string
	): Promise<void> {
		const trimmed = text.trim();
		if (this.busy || trimmed === '') return;
		// a pick whose send fails gives the person the same choice back, to pick again
		const choice =
			clarify && this.clarify
				? { questionId: this.questionId, clarify: this.clarify, entries: this.entries }
				: null;
		this.#clear();
		const generation = this.#generation;
		this.phase = 'sending';
		this.text = trimmed;
		this.#project = project;

		const body: NewQuestion = { text: trimmed };
		if (project) body.project = project;
		if (clarify) body.clarify = clarify;
		if (follows) body.follows = follows;
		// a send that failed is retried with its key, so the core can't start the question twice
		const signature = JSON.stringify(body);
		if (signature !== this.#keyFor) {
			this.#keyFor = signature;
			this.#key = this.#newKey();
		}

		try {
			const api = await this.#api();
			const accepted = await api.post<Accepted>('/v1/questions', body, {
				idempotencyKey: this.#key
			});
			if (generation !== this.#generation) return;
			// asking the same thing again later is a new question, with a key of its own
			this.#keyFor = '';
			this.questionId = accepted.id;
			this.known = true;
			this.#started = this.#now();
			this.#timed = true;
			this.phase = 'running';
			this.#follow(accepted.id, generation);
		} catch (error) {
			if (generation !== this.#generation) return;
			this.failure = describe(error);
			if (choice) {
				this.questionId = choice.questionId;
				this.known = true;
				this.clarify = choice.clarify;
				this.entries = choice.entries;
				this.phase = 'clarify';
				return;
			}
			// the question is still in the box, and sending it again still asks the rejected one again;
			// unless the core refused the link itself: then the next send goes without it. Any other
			// refusal of the request (words too long, say) is about the question, so the corrected one
			// still names the question it asks again
			this.following = refusesFollows(error) ? null : (follows ?? null);
			this.phase = 'failed';
		}
	}

	/**
	 * Brings back a question: where it stands, then its events from the start. `text` is what was
	 * asked when this tab still has the words (a conversation opened again from the side panel), so
	 * a clarify can be answered; a question that came back by its address alone has none. A question
	 * already on screen stays as it is, unless fetching it failed: then it is fetched again.
	 */
	async restore(id: string, text = ''): Promise<void> {
		if (this.questionId === id && (this.known || this.phase === 'loading')) return;
		this.#clear();
		const generation = this.#generation;
		this.phase = 'loading';
		this.questionId = id;
		this.text = text.trim();
		this.#timed = false;
		try {
			const api = await this.#api();
			const state = await api.get<QuestionState>(`/v1/questions/${encodeURIComponent(id)}`);
			if (generation !== this.#generation) return;
			this.known = true;
			if (state.status === 'pending_signoff') {
				this.phase = 'pending_signoff';
				this.#poll(generation);
			} else if (!this.#settle(state)) {
				this.phase = state.status === 'running' ? 'running' : 'loading';
			}
			// the stream replays every event from the first, so the timeline is whole again
			this.#follow(id, generation);
		} catch (error) {
			if (generation !== this.#generation) return;
			if (error instanceof ApiError && error.status === 404) {
				this.questionId = null;
				this.phase = 'unavailable';
			} else {
				this.failure = describe(error);
				this.phase = 'failed';
			}
		}
	}

	/** Opens the stream again after it broke; what was shown already is not shown twice. */
	reconnect(): void {
		if (this.questionId === null || this.phase !== 'disconnected') return;
		this.phase = 'running';
		this.#follow(this.questionId, this.#generation);
	}

	/** A new conversation, or the end of the session: the stream closes and the screen is empty. */
	reset(): void {
		this.#clear();
	}

	#clear(): void {
		this.#generation += 1;
		this.#stop?.();
		this.#stop = null;
		if (this.#timer !== null) clearTimeout(this.#timer);
		this.#timer = null;
		this.phase = 'idle';
		this.questionId = null;
		this.known = false;
		this.text = '';
		this.entries = [];
		this.project = null;
		this.answer = null;
		this.refusal = null;
		this.clarify = null;
		this.signoff = null;
		this.done = null;
		this.failure = null;
		this.following = null;
		this.#lastSeq = 0;
	}

	#follow(id: string, generation: number): void {
		this.#stop?.();
		this.#stop = null;
		this.#api()
			.then((api) => {
				if (generation !== this.#generation) return;
				this.#stop = api.events(
					id,
					(event) => this.#receive(event, generation),
					() => void this.#dropped(generation)
				);
			})
			.catch(() => void this.#lost(generation));
	}

	#receive(event: StreamEvent, generation: number): void {
		if (generation !== this.#generation) return;
		// the transport reports a stream it can no longer hear, or an event it can't read, as an
		// `error` with no id: not a row, since its cost and judge are no record; the state says how
		// the question ended
		if (event.event === 'error' && event.id === '') {
			void this.#lost(generation);
			return;
		}
		// an event is shown once, whichever way the browser came by it again
		const seq = Number(event.id);
		if (Number.isFinite(seq)) {
			if (seq <= this.#lastSeq) return;
			this.#lastSeq = seq;
		}
		// a row the retention emptied has no data to read; the question's state says how it ended
		if (event.data === null) {
			if (isTerminal(event.event)) void this.#lost(generation, true);
			return;
		}
		this.entries.push({ event, at: this.#timed ? this.#now() - this.#started : null });
		if (this.phase === 'loading') this.phase = 'running';

		switch (event.event) {
			case 'project':
				this.project = event.data;
				break;
			case 'refused':
				this.refusal = event.data;
				this.phase = 'refused';
				this.#stopPolling();
				break;
			case 'clarify':
				this.clarify = event.data;
				this.phase = 'clarify';
				break;
			case 'pending_signoff':
				this.signoff = event.data;
				// a replay after the decision comes past the hold: a question already settled stays so
				if (this.#settled()) break;
				this.phase = 'pending_signoff';
				this.#poll(generation);
				break;
			case 'answer':
				// the one place an answer's text comes in
				this.answer = event.data;
				this.phase = 'delivered';
				this.#stopPolling();
				break;
			case 'done':
				this.done = event.data;
				break;
			case 'error':
				this.failure = describeFailure(event.data.code);
				this.phase = 'failed';
				break;
		}
	}

	/**
	 * The connection dropped and the browser is trying again. A question that ended without its last
	 * event never sends it, so ask the core: an ended question is shown as it ended and its stream
	 * closed; one still running keeps its stream, which replays from where it was.
	 */
	async #dropped(generation: number): Promise<void> {
		const id = this.questionId;
		if (id === null || generation !== this.#generation || this.#asking) return;
		if (this.phase !== 'running' && this.phase !== 'loading') return;
		this.#asking = true;
		try {
			const api = await this.#api();
			const state = await api.get<QuestionState>(`/v1/questions/${encodeURIComponent(id)}`);
			if (generation !== this.#generation) return;
			if (state.status === 'pending_signoff') {
				this.#stop?.();
				this.#stop = null;
				this.phase = 'pending_signoff';
				this.#poll(generation);
			} else if (this.#settle(state)) {
				this.#stop?.();
				this.#stop = null;
			}
		} catch {
			// the core can't be reached either: the browser's retries go on
		} finally {
			this.#asking = false;
		}
	}

	/**
	 * The stream is gone. The core knows whether the question ended; ask it. `emptied` when the
	 * stream ended on a row the retention emptied: a question that ended so (with a choice to make,
	 * say, whose options are gone) is no longer kept, not lost.
	 */
	async #lost(generation: number, emptied = false): Promise<void> {
		const id = this.questionId;
		if (id === null || generation !== this.#generation) return;
		try {
			const api = await this.#api();
			const state = await api.get<QuestionState>(`/v1/questions/${encodeURIComponent(id)}`);
			if (generation !== this.#generation) return;
			if (state.status === 'pending_signoff') {
				this.phase = 'pending_signoff';
				this.#poll(generation);
				return;
			}
			if (this.#settle(state)) return;
			if (emptied && state.status !== 'running') {
				this.phase = 'expired';
				return;
			}
		} catch {
			// the core can't be reached either: the person can try again
		}
		if (generation === this.#generation) this.phase = 'disconnected';
	}

	/** Takes how a question ended from its state. False when the state doesn't say. */
	#settle(state: QuestionState): boolean {
		const settled = settledOf(state);
		if (settled === null) return false;
		switch (settled.phase) {
			case 'delivered':
				this.answer = settled.answer;
				this.done = settled.done ?? this.done;
				break;
			case 'refused':
				this.refusal = settled.refusal;
				break;
			case 'failed':
				this.failure = describeFailure('internal');
				break;
		}
		this.phase = settled.phase;
		return true;
	}

	/** Whether the question has ended as it will stay: answered, refused, failed or no longer kept. */
	#settled(): boolean {
		return ['delivered', 'refused', 'failed', 'expired'].includes(this.phase);
	}

	/** The decision came on the stream: the poll has nothing left to ask. */
	#stopPolling(): void {
		if (this.#timer !== null) clearTimeout(this.#timer);
		this.#timer = null;
	}

	/** Asks about a question held for sign-off until it is decided. */
	#poll(generation: number): void {
		if (this.#timer !== null) clearTimeout(this.#timer);
		this.#timer = setTimeout(async () => {
			this.#timer = null;
			const id = this.questionId;
			if (id === null || generation !== this.#generation) return;
			try {
				const api = await this.#api();
				const state = await api.get<QuestionState>(`/v1/questions/${encodeURIComponent(id)}`);
				if (generation !== this.#generation) return;
				if (state.status !== 'pending_signoff' && this.#settle(state)) return;
			} catch {
				// the next round tries again; a session that ended is caught by the transport
			}
			if (generation === this.#generation) this.#poll(generation);
		}, this.#pollMs);
	}
}

/** The app's one conversation: it survives moving to Settings and back, and ends with the session. */
export const conversation = new Conversation();
session.onSignedOut(() => conversation.reset());

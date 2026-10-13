/**
 * The conversations of this tab, newest first, for the Ask screen's side list: each one the
 * questions asked in it, one after the other, with what came of each. Kept in memory only, like the
 * rest of the Ask screen: a reload or a sign-out starts it empty, and nothing of it reaches browser
 * storage. A question from an earlier visit comes back by its address alone, as a conversation of
 * its own.
 *
 * A turn keeps what the question's events said, the answer only from its `answer` event (or the
 * state of a delivered question, which is that same event), so an earlier answer in a conversation
 * is the one the core delivered.
 *
 * The list says how each question stands, never more than the core said: a question is put in it
 * once the core knows it (never one whose address named nothing), and a question left while it was
 * still being answered, or waiting for an approver, is asked about until it ends, so the list
 * doesn't show it answering after it ended.
 */

import {
	ApiError,
	getTransport,
	type Answer,
	type Clarify,
	type Ending,
	type QuestionState,
	type Refusal,
	type Transport
} from '$lib/api';
import { session } from '$lib/session.svelte';

import { answerBlocks } from './answer-text';
import { settledOf, type Conversation, type Phase } from './conversation.svelte';

/** One question of a conversation and what came of it. */
export interface Turn {
	id: string;
	/** What was asked; empty for a question that came back by its address. */
	text: string;
	/** The project it was about, by id, once Mercury knows it. */
	project: string | null;
	phase: Phase;
	answer: Answer | null;
	refusal: Refusal | null;
	clarify: Clarify | null;
	done: Ending | null;
	/** What the person should know about the data the answer rests on, as the page said it. */
	notes: string[];
	/** Text values the data checks withheld. */
	withheld: number;
	/** Sentences the output checks left out. */
	stripped: number;
	/** Whether part of the answer comes from the project's code reader. */
	fromCode: boolean;
	/** One line of how it ended, for the side list: the answer's start, the refusal, the question back. */
	preview: string;
}

export interface Thread {
	key: string;
	turns: Turn[];
	/** When its first and its last question were asked, in milliseconds since the epoch. */
	at: number;
	updated: number;
}

/** One line of how a question stands, for the side list. */
export function previewOf(turn: Pick<Turn, 'phase' | 'answer' | 'refusal' | 'clarify'>): string {
	switch (turn.phase) {
		case 'delivered':
			return turn.answer ? firstLine(turn.answer.text) : '';
		case 'refused':
			return turn.refusal?.message ?? '';
		case 'clarify':
			return turn.clarify?.question ?? '';
		case 'pending_signoff':
			return 'Waiting for an approver';
		case 'expired':
			return 'The answer is no longer kept';
		default:
			return '';
	}
}

/** The answer's first words, as plain text. */
function firstLine(text: string): string {
	const first = answerBlocks(text).find((block) => block.kind !== 'note');
	if (!first) return text;
	const parts =
		first.kind === 'paragraph' ? first.parts : first.kind === 'list' ? first.items[0] : [];
	return parts.map((part) => part.text).join('');
}

/**
 * The question on screen as a turn of its conversation, or none while the core doesn't know it
 * yet: a question being fetched by its id, or one whose address named nothing.
 */
export function turnOf(conversation: Conversation, notes: string[]): Turn | null {
	const id = conversation.questionId;
	if (id === null || !conversation.known || conversation.phase === 'loading') return null;
	const turn = {
		id,
		text: conversation.text,
		project: conversation.project?.project ?? null,
		phase: conversation.phase,
		answer: $state.snapshot(conversation.answer),
		refusal: $state.snapshot(conversation.refusal),
		clarify: $state.snapshot(conversation.clarify),
		done: $state.snapshot(conversation.done),
		notes: [...notes],
		withheld: conversation.withheld,
		stripped: conversation.stripped,
		fromCode: conversation.fromCode
	};
	return { ...turn, preview: previewOf(turn) };
}

export interface ThreadsOptions {
	api?: () => Promise<Transport>;
	/** How often a question left before it ended is asked about, in milliseconds. */
	pollMs?: number;
}

export class Threads {
	items = $state<Thread[]>([]);
	/** The conversation on screen; none before its first question. */
	active = $state<string | null>(null);
	#next = 0;

	#api: () => Promise<Transport>;
	#pollMs: number;
	/** The questions left before they ended, and the next time each is asked about. */
	// eslint-disable-next-line svelte/prefer-svelte-reactivity -- timers, which no screen reads
	#watched = new Map<string, ReturnType<typeof setTimeout>>();
	/** Bumped when the list is cleared, so an answer about a question of the old list is dropped. */
	#generation = 0;

	constructor({ api = getTransport, pollMs = 5000 }: ThreadsOptions = {}) {
		this.#api = api;
		this.#pollMs = pollMs;
	}

	get current(): Thread | undefined {
		return this.items.find((thread) => thread.key === this.active);
	}

	/** The conversation a question was asked in. */
	find(questionId: string | null): Thread | undefined {
		if (questionId === null) return undefined;
		return this.items.find((thread) => thread.turns.some((turn) => turn.id === questionId));
	}

	/** A question of this tab, as the list has it. */
	turn(questionId: string): Turn | undefined {
		return this.find(questionId)?.turns.find((turn) => turn.id === questionId);
	}

	/**
	 * Adds a question to the conversation on screen, starting one if there is none, or brings a
	 * question already there up to date. A choice answered after a clarify takes the clarify's place.
	 */
	record(turn: Turn, now: number = Date.now()): void {
		if (!this.current) {
			this.#next += 1;
			const key = `c${this.#next}`;
			this.items = [{ key, turns: [], at: now, updated: now }, ...this.items];
			this.active = key;
		}
		const thread = this.current!;
		const known = thread.turns.find((item) => item.id === turn.id);
		if (known) {
			Object.assign(known, { ...turn, text: turn.text || known.text });
		} else {
			const last = thread.turns.at(-1);
			if (last && last.phase === 'clarify' && last.text === turn.text) {
				thread.turns[thread.turns.length - 1] = turn;
			} else {
				thread.turns.push(turn);
				thread.updated = now;
			}
		}
		// the conversation spoken in last goes to the top
		if (this.items[0] !== thread) {
			this.items = [thread, ...this.items.filter((item) => item !== thread)];
		}
	}

	/**
	 * Puts on screen the conversation a question was asked in, or none for a question this tab
	 * doesn't have, and gives the question as the list has it. The question on screen is followed
	 * by the conversation, so the list stops asking about it.
	 */
	open(questionId: string): Turn | undefined {
		this.unwatch(questionId);
		this.active = this.find(questionId)?.key ?? null;
		return this.turn(questionId);
	}

	/**
	 * A question left while it was still being answered, or waiting for an approver: the core is
	 * asked about it every few seconds until it ends, and the list says how it ended.
	 */
	watch(questionId: string): void {
		if (this.#watched.has(questionId) || !this.turn(questionId)) return;
		this.#schedule(questionId, this.#generation);
	}

	unwatch(questionId: string): void {
		clearTimeout(this.#watched.get(questionId));
		this.#watched.delete(questionId);
	}

	clear(): void {
		this.#generation += 1;
		for (const timer of this.#watched.values()) clearTimeout(timer);
		this.#watched.clear();
		this.items = [];
		this.active = null;
	}

	#schedule(id: string, generation: number): void {
		this.#watched.set(
			id,
			setTimeout(() => void this.#check(id, generation), this.#pollMs)
		);
	}

	async #check(id: string, generation: number): Promise<void> {
		let state: QuestionState | null = null;
		let gone = false;
		try {
			const api = await this.#api();
			state = await api.get<QuestionState>(`/v1/questions/${encodeURIComponent(id)}`);
		} catch (error) {
			// a question the core no longer has ends here; anything else is asked again
			gone = error instanceof ApiError && error.status === 404;
		}
		if (generation !== this.#generation || !this.#watched.has(id)) return;
		const turn = this.turn(id);
		if (!turn) {
			this.#watched.delete(id);
			return;
		}
		if (gone) {
			this.#end(turn, 'unavailable');
			return;
		}
		if (state) {
			const settled = settledOf(state);
			if (settled) {
				if (settled.phase === 'delivered') {
					turn.answer = settled.answer;
					turn.done = settled.done ?? turn.done;
				}
				if (settled.phase === 'refused') turn.refusal = settled.refusal;
				this.#end(turn, settled.phase);
				return;
			}
			// it ended with a choice for the person to make, which opening it again offers
			if (state.status === 'clarify') {
				this.#end(turn, 'clarify');
				return;
			}
			if (state.status === 'pending_signoff' && turn.phase !== 'pending_signoff') {
				turn.phase = 'pending_signoff';
				turn.preview = previewOf(turn);
			}
		}
		this.#schedule(id, generation);
	}

	#end(turn: Turn, phase: Phase): void {
		this.#watched.delete(turn.id);
		turn.phase = phase;
		turn.preview = previewOf(turn);
	}
}

export const threads = new Threads();

session.onSignedOut(() => threads.clear());

/**
 * Puts a question on screen, in the conversation of this tab it was asked in (none for a question
 * that came back by its address alone), with its words when the tab has them, so a clarify of it
 * can be answered. Whatever named the question (a click in the side panel, an address, a reload),
 * this is what opens it. A question it leaves while still being answered is watched from the list.
 */
export function openQuestion(
	conversation: Conversation,
	questionId: string,
	list: Threads = threads
): Promise<void> {
	if (conversation.questionId !== questionId) leave(conversation, list);
	const turn = list.open(questionId);
	return conversation.restore(questionId, turn?.text ?? '');
}

/** A new conversation: the screen is empty and the next question starts a conversation of its own. */
export function newConversation(conversation: Conversation, list: Threads = threads): void {
	leave(conversation, list);
	conversation.reset();
	list.active = null;
}

/** The question on screen goes; if it is still being answered, the list asks about it until it ends. */
function leave(conversation: Conversation, list: Threads): void {
	const id = conversation.questionId;
	if (id !== null && conversation.known && conversation.busy) list.watch(id);
}

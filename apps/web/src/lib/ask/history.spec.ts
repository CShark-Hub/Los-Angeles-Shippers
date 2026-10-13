import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	ApiError,
	type Answer,
	type Clarify,
	type QuestionState,
	type Refusal,
	type StreamEvent,
	type Transport
} from '$lib/api';
import { Conversation } from './conversation.svelte';
import {
	newConversation,
	openQuestion,
	previewOf,
	Threads,
	threads,
	turnOf,
	type Turn
} from './history.svelte';

function turn(id: string, text: string, phase: Turn['phase'] = 'delivered'): Turn {
	return {
		id,
		text,
		project: 'delta',
		phase,
		answer: null,
		refusal: null,
		clarify: null,
		done: null,
		notes: [],
		withheld: 0,
		stripped: 0,
		fromCode: false,
		preview: ''
	};
}

describe('the conversations of this tab', () => {
	beforeEach(() => threads.clear());

	it('keeps every question of a conversation, one after the other, as a chat does', () => {
		threads.record(turn('q_1', 'What was finished this week?'));
		threads.record(turn('q_2', 'What is blocked right now?'));
		threads.record(turn('q_3', 'And what is it waiting on?'));
		expect(threads.items).toHaveLength(1);
		expect(threads.current?.turns.map((t) => t.text)).toEqual([
			'What was finished this week?',
			'What is blocked right now?',
			'And what is it waiting on?'
		]);
	});

	it('brings a question up to date where it stands, keeping its words', () => {
		threads.record(turn('q_1', 'What was finished this week?', 'running'));
		threads.record(turn('q_1', '', 'delivered'));
		expect(threads.current?.turns).toHaveLength(1);
		expect(threads.current?.turns[0]).toMatchObject({
			text: 'What was finished this week?',
			phase: 'delivered'
		});
	});

	it('puts the choice answered after a clarify in the place of the clarify', () => {
		threads.record(turn('q_1', 'What is done this week?', 'clarify'));
		threads.record(turn('q_2', 'What is done this week?'));
		expect(threads.current?.turns.map((t) => t.id)).toEqual(['q_2']);
	});

	it('starts a new conversation after New conversation, newest first, and finds a question in its own', () => {
		threads.record(turn('q_1', 'What was finished this week?'));
		threads.active = null;
		threads.record(turn('q_2', 'Show me the export code', 'refused'));
		expect(threads.items.map((t) => t.turns[0].id)).toEqual(['q_2', 'q_1']);
		expect(threads.find('q_1')?.turns[0].text).toBe('What was finished this week?');
	});

	it('says in one line how a question stands, for the side list', () => {
		expect(previewOf({ ...turn('q_1', ''), answer: ANSWER })).toBe(ANSWER.text);
		expect(previewOf({ ...turn('q_1', '', 'refused'), refusal: REJECTED })).toBe(REJECTED.message);
		expect(previewOf(turn('q_1', '', 'pending_signoff'))).toBe('Waiting for an approver');
		expect(previewOf(turn('q_1', '', 'expired'))).toBe('The answer is no longer kept');
		expect(previewOf(turn('q_1', '', 'running'))).toBe('');
	});
});

const ANSWER: Answer = {
	text: 'Two items were finished in Delta this week.',
	facts: [],
	confidence: 0.91
};

const REJECTED: Refusal = {
	code: 'rejected_by_approver',
	message: 'An approver did not release this answer.',
	hint: 'Ask the tech lead about the export first.'
};

const CLARIFY: Clarify = {
	kind: 'project',
	question: 'Which project do you mean?',
	options: [
		{ id: 'delta', label: 'Delta' },
		{ id: 'billing', label: 'Billing' }
	],
	cost_usd: 0,
	judge_mode: null
};

function stateOf(id: string, change: Partial<QuestionState> = {}): QuestionState {
	return {
		id,
		status: 'running',
		project: 'delta',
		class: null,
		answer: null,
		refusal: null,
		judge_mode: null,
		cost_usd: 0,
		duration_ms: null,
		audit_id: null,
		...change
	};
}

type Stream = { id: string; onEvent: (event: StreamEvent) => void; closed: boolean };

/** A core of several questions, `q_1`, `q_2`… in the order they are asked; one it never had is a `404`. */
function fakeCore() {
	const states = new Map<string, QuestionState | ApiError>();
	const streams: Stream[] = [];
	const posts: unknown[] = [];
	const gets: string[] = [];
	let asked = 0;

	const transport: Transport = {
		async get<T>(path: string) {
			gets.push(path);
			const id = decodeURIComponent(path.split('/').at(-1) ?? '');
			const state = states.get(id) ?? new ApiError(404, 'invalid_request', 'No such question.');
			if (state instanceof ApiError) throw state;
			return state as T;
		},
		async post<T>(_path: string, body: unknown) {
			posts.push(body);
			asked += 1;
			const id = `q_${asked}`;
			states.set(id, stateOf(id));
			return { id } as T;
		},
		async delete<T>() {
			return undefined as T;
		},
		events(id, onEvent) {
			const stream: Stream = { id, onEvent, closed: false };
			streams.push(stream);
			return () => {
				stream.closed = true;
			};
		}
	};

	return {
		transport,
		posts,
		gets,
		/** Where a question stands, as `GET /v1/questions/{id}` says it from now on. */
		set(id: string, change: Partial<QuestionState>) {
			const now = states.get(id);
			states.set(id, stateOf(id, { ...(now instanceof ApiError ? {} : now), ...change }));
		},
		/** An event on the stream the screen has open for a question. */
		emit(id: string, seq: number, event: string, data: unknown) {
			const stream = streams.filter((s) => s.id === id && !s.closed).at(-1);
			stream?.onEvent({ event, id: String(seq), data } as StreamEvent);
		}
	};
}

const flush = () => vi.advanceTimersByTimeAsync(0);

/**
 * The Ask page as far as its side list goes: the question on screen, the conversations of the
 * tab, and the turn the page records whenever the question on screen changes (its effect). Opening
 * a question is what the page does for every navigation that names one, a click in the side panel
 * among them.
 */
function askPage(core: ReturnType<typeof fakeCore>) {
	let keys = 0;
	const list = new Threads({ api: async () => core.transport, pollMs: 1000 });
	const ask = new Conversation({
		api: async () => core.transport,
		pollMs: 1000,
		now: () => Date.now(),
		newKey: () => `key-${++keys}`
	});
	const record = () => {
		const now = turnOf(ask, []);
		if (now) list.record(now);
	};
	return {
		list,
		ask,
		record,
		async question(text: string) {
			const sent = ask.ask(text, null);
			record();
			await sent;
			await flush();
			record();
		},
		async open(id: string) {
			const opened = openQuestion(ask, id, list);
			// while it is fetched, the core hasn't said anything about it yet
			record();
			await opened;
			await flush();
			record();
		},
		fresh() {
			newConversation(ask, list);
			record();
		}
	};
}

describe('the side panel', () => {
	beforeEach(() => {
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('opens the conversation clicked, after a new one was started and asked in, every time', async () => {
		const core = fakeCore();
		const page = askPage(core);
		await page.question('What was finished this week?');
		core.emit('q_1', 1, 'answer', ANSWER);
		core.set('q_1', { status: 'delivered', answer: ANSWER });
		page.record();
		const first = page.list.active;

		page.fresh();
		await page.question('Show me the export code');
		core.emit('q_2', 1, 'refused', REJECTED);
		core.set('q_2', { status: 'refused', refusal: REJECTED });
		page.record();

		// opened from the list
		await page.open('q_1');
		expect(page.list.active).toBe(first);
		expect(page.ask.questionId).toBe('q_1');

		// a new conversation, and a question asked in it
		page.fresh();
		await page.question('Is the build green?');
		expect(page.ask.questionId).toBe('q_3');
		expect(page.list.active).not.toBe(first);

		// the first one, clicked again: it is on screen, with its words and its answer
		await page.open('q_1');
		expect(page.list.active).toBe(first);
		expect(page.ask.questionId).toBe('q_1');
		expect(page.ask.text).toBe('What was finished this week?');
		expect(page.ask.phase).toBe('delivered');
		expect(page.ask.answer).toEqual(ANSWER);
		// three conversations, each with its own question, and the core asked about the first each time
		expect(page.list.items.map((thread) => thread.turns.map((t) => t.id)).sort()).toEqual([
			['q_1'],
			['q_2'],
			['q_3']
		]);
		expect(core.gets.filter((path) => path === '/v1/questions/q_1')).toHaveLength(2);
	});

	it('lets a clarify be answered after switching back to its conversation', async () => {
		const core = fakeCore();
		const page = askPage(core);
		await page.question("What's done this week?");
		core.emit('q_1', 1, 'clarify', CLARIFY);
		core.set('q_1', { status: 'clarify', project: null });
		page.record();

		page.fresh();
		await page.question('Is the build green?');

		await page.open('q_1');
		// its stream replays from the first event: the question back, with its options
		core.emit('q_1', 1, 'clarify', CLARIFY);
		page.record();
		expect(page.ask.phase).toBe('clarify');
		expect(page.ask.canChoose).toBe(true);

		await page.ask.choose('billing');
		await flush();
		page.record();
		expect(core.posts.at(-1)).toEqual({
			text: "What's done this week?",
			clarify: { of: 'q_1', option: 'billing' }
		});
		// the choice takes the place of the clarify in its own conversation
		expect(page.list.current?.turns.map((t) => t.id)).toEqual(['q_3']);
	});

	it('keeps no conversation for an address the core never had, and the next question starts its own', async () => {
		const core = fakeCore();
		const page = askPage(core);
		await page.open('q_nope');

		expect(page.ask.phase).toBe('unavailable');
		expect(page.list.items).toEqual([]);
		expect(page.list.active).toBeNull();

		await page.question('What was finished this week?');
		expect(page.list.items).toHaveLength(1);
		expect(page.list.current?.turns.map((t) => t.id)).toEqual(['q_1']);
	});

	it('shows how a question left while it ran ended, not that it is still answering', async () => {
		const core = fakeCore();
		const page = askPage(core);
		await page.question('What was finished this week?');
		core.emit('q_1', 1, 'accepted', { id: 'q_1' });
		page.record();

		page.fresh();
		expect(page.list.turn('q_1')?.phase).toBe('running');

		await vi.advanceTimersByTimeAsync(1000);
		expect(page.list.turn('q_1')?.phase).toBe('running');

		core.set('q_1', {
			status: 'delivered',
			answer: ANSWER,
			judge_mode: 'jev',
			duration_ms: 1900,
			audit_id: 9
		});
		await vi.advanceTimersByTimeAsync(1000);
		expect(page.list.turn('q_1')).toMatchObject({
			phase: 'delivered',
			answer: ANSWER,
			done: { duration_ms: 1900, audit_id: 9 },
			preview: ANSWER.text
		});

		// it ended: nobody asks about it any more
		const asked = core.gets.length;
		await vi.advanceTimersByTimeAsync(10_000);
		expect(core.gets).toHaveLength(asked);
	});

	it('shows the rejection and its note for a question left while it waited for an approver', async () => {
		const core = fakeCore();
		const page = askPage(core);
		await page.question('Is the export ready for the client?');
		core.emit('q_1', 1, 'pending_signoff', { approval_id: 'ap_1', reason: 'class' });
		core.set('q_1', { status: 'pending_signoff' });
		page.record();

		// a new conversation while it waits; back to it, and away again by the list
		page.fresh();
		await page.question('What is blocked right now?');
		await page.open('q_1');
		expect(page.ask.phase).toBe('pending_signoff');
		await page.open('q_2');
		expect(page.list.turn('q_1')?.phase).toBe('pending_signoff');

		core.set('q_1', { status: 'refused', refusal: REJECTED });
		await vi.advanceTimersByTimeAsync(1000);
		expect(page.list.turn('q_1')).toMatchObject({
			phase: 'refused',
			refusal: REJECTED,
			preview: REJECTED.message
		});
	});

	it('says the answer is no longer kept for a question left while it ran, once the retention emptied it', async () => {
		const core = fakeCore();
		const page = askPage(core);
		await page.question('What was finished this week?');
		page.fresh();
		core.set('q_1', { status: 'delivered', answer: null });
		await vi.advanceTimersByTimeAsync(1000);
		expect(page.list.turn('q_1')).toMatchObject({
			phase: 'expired',
			preview: 'The answer is no longer kept'
		});
	});

	it('stops asking about a question once it is on screen again, or the list is cleared', async () => {
		const core = fakeCore();
		const page = askPage(core);
		await page.question('What was finished this week?');
		page.fresh();
		await page.open('q_1');
		const asked = core.gets.length;
		await vi.advanceTimersByTimeAsync(5000);
		// the conversation follows its stream; the list doesn't ask as well
		expect(core.gets).toHaveLength(asked);

		page.fresh();
		page.list.clear();
		await vi.advanceTimersByTimeAsync(5000);
		expect(core.gets).toHaveLength(asked);
	});
});

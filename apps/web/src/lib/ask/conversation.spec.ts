import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	ApiError,
	type Answer,
	type Done,
	type QuestionState,
	type StreamEvent,
	type Transport
} from '$lib/api';
import { session } from '$lib/session.svelte';
import { Conversation, conversation, FOLLOWS_REFUSED } from './conversation.svelte';
import { describeEvent } from './timeline';

const ANSWER: Answer = {
	text: 'Two items were finished in Delta this week.',
	facts: [],
	confidence: 0.91
};

const DONE: Done = {
	duration_ms: 64000,
	cost_usd: 0.004,
	judge_mode: 'jev',
	composer: 'model',
	audit_id: 4
};

type Stream = {
	id: string;
	onEvent: (event: StreamEvent) => void;
	onDropped?: () => void;
	closed: boolean;
};

/** A core that answers from a script: what `GET` returns is `state`, and every stream is kept. */
function fakeCore() {
	const streams: Stream[] = [];
	const posts: { path: string; body: unknown; key: string | undefined }[] = [];
	const gets: string[] = [];
	let state: QuestionState | ApiError = {
		id: 'q_1',
		status: 'running',
		project: 'delta',
		class: null,
		answer: null,
		refusal: null,
		judge_mode: null,
		cost_usd: 0,
		duration_ms: null,
		audit_id: null
	};
	let accepted: string[] = ['q_1'];
	let postFails: ApiError | null = null;

	const transport: Transport = {
		async get<T>(path: string) {
			gets.push(path);
			if (state instanceof ApiError) throw state;
			return state as T;
		},
		async post<T>(path: string, body: unknown, options?: { idempotencyKey?: string }) {
			posts.push({ path, body, key: options?.idempotencyKey });
			if (postFails) throw postFails;
			if (path === '/v1/questions') return { id: accepted.shift() ?? 'q_x' } as T;
			return undefined as T;
		},
		async delete<T>() {
			return undefined as T;
		},
		events(id, onEvent, onDropped) {
			const stream: Stream = { id, onEvent, onDropped, closed: false };
			streams.push(stream);
			return () => {
				stream.closed = true;
			};
		}
	};

	return {
		transport,
		streams,
		posts,
		gets,
		get stream() {
			return streams[streams.length - 1];
		},
		/** Delivers an event to the open stream the way the transport does. */
		emit(id: number | '', event: string, data: unknown, stream = streams[streams.length - 1]) {
			stream.onEvent({ event, id: String(id), data } as StreamEvent);
		},
		/** The connection drops and the browser tries again, as `EventSource` does. */
		drop(stream = streams[streams.length - 1]) {
			stream.onDropped?.();
		},
		setState(next: Partial<QuestionState> | ApiError) {
			state = next instanceof ApiError ? next : { ...(state as QuestionState), ...next };
		},
		queue(...ids: string[]) {
			accepted = ids;
		},
		failNextPost(error: ApiError | null) {
			postFails = error;
		}
	};
}

function conversationOn(core: ReturnType<typeof fakeCore>, pollMs = 5000) {
	let keys = 0;
	return new Conversation({
		api: async () => core.transport,
		pollMs,
		now: () => Date.now(),
		newKey: () => `key-${++keys}`
	});
}

const flush = () => vi.advanceTimersByTimeAsync(0);

/** The events of an answered question, up to but not including the answer. */
function runUpToTheAnswer(core: ReturnType<typeof fakeCore>) {
	core.emit(1, 'accepted', { id: 'q_1' });
	core.emit(2, 'project', { project: 'delta', p: 1, source: 'explicit' });
	core.emit(3, 'classified', { class: 'progress', p: 0.94, checks: [] });
	core.emit(4, 'plan', { calls: [{ tool: 'progress', args: {} }] });
	core.emit(5, 'tool_result', { call: 0, items: 2, withheld_fields: [], access: 'metadata' });
	core.emit(6, 'composing', {});
	core.emit(7, 'judged', { sentences: 1, stripped: 0, held: 0 });
}

describe('Conversation', () => {
	beforeEach(() => {
		vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('shows the failure of a question whose stream the core closed without its last event', async () => {
		const core = fakeCore();
		const ask = conversationOn(core);
		await ask.ask('What was finished in Delta this week?', 'delta');
		await flush();
		core.emit(1, 'accepted', { id: 'q_1' });
		core.emit(2, 'plan', { calls: [{ tool: 'progress', args: {} }] });

		// the closing row couldn't be written: the row says failed, and the browser keeps retrying
		core.setState({ status: 'failed' });
		core.drop();
		await flush();

		expect(ask.phase).toBe('failed');
		expect(ask.failure?.message).toBe('Something went wrong on the server.');
		expect(core.stream.closed).toBe(true);
		expect(core.gets).toEqual(['/v1/questions/q_1']);
	});

	it('keeps following a question that is still running when the connection drops', async () => {
		const core = fakeCore();
		const ask = conversationOn(core);
		await ask.ask('What was finished in Delta this week?', 'delta');
		await flush();
		core.emit(1, 'accepted', { id: 'q_1' });
		core.emit(2, 'project', { project: 'delta', p: 1, source: 'explicit' });

		core.drop();
		await flush();
		expect(ask.phase).toBe('running');
		expect(core.stream.closed).toBe(false);

		// the browser's retry replays from Last-Event-ID; an event seen already is not shown twice
		core.emit(2, 'project', { project: 'delta', p: 1, source: 'explicit' });
		core.emit(3, 'classified', { class: 'progress', p: 0.94, checks: [] });
		core.emit(4, 'answer', ANSWER);
		expect(ask.phase).toBe('delivered');
		expect(ask.answer).toEqual(ANSWER);
		expect(ask.entries.map(({ event }) => event.event)).toEqual([
			'accepted',
			'project',
			'classified',
			'answer'
		]);
	});

	it('skips an event whose data retention emptied, and asks how a question so emptied ended', async () => {
		const core = fakeCore();
		const ask = conversationOn(core);
		core.setState({ status: 'delivered', answer: ANSWER });
		await ask.restore('q_1');
		await flush();

		expect(() => {
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'project', null);
			core.emit(3, 'tool_result', null);
		}).not.toThrow();
		expect(ask.entries.map(({ event }) => event.event)).toEqual(['accepted']);
		expect(ask.project).toBeNull();
		expect(ask.withheld).toBe(0);
		expect(() => ask.entries.map(({ event }) => describeEvent(event))).not.toThrow();

		// a terminal event with no data: the state says how the question ended
		core.emit(4, 'answer', null);
		await flush();
		expect(ask.phase).toBe('delivered');
		expect(ask.answer).toEqual(ANSWER);
	});

	it('asks with an idempotency key and follows the stream of the question it was given', async () => {
		const core = fakeCore();
		const ask = conversationOn(core);
		await ask.ask('  What was finished in Delta this week?  ', 'delta');

		expect(core.posts).toEqual([
			{
				path: '/v1/questions',
				body: { text: 'What was finished in Delta this week?', project: 'delta' },
				key: 'key-1'
			}
		]);
		await flush();
		expect(core.stream.id).toBe('q_1');
		expect(ask.questionId).toBe('q_1');
		expect(ask.phase).toBe('running');
		expect(ask.busy).toBe(true);
	});

	it('shows the answer text only once the answer event has arrived', async () => {
		const core = fakeCore();
		const ask = conversationOn(core);
		await ask.ask('What was finished in Delta this week?', 'delta');
		await flush();

		runUpToTheAnswer(core);
		expect(ask.answer).toBeNull();
		expect(ask.phase).toBe('running');
		// nothing the screen holds so far carries the text of an answer
		expect(JSON.stringify(ask.entries)).not.toContain('Two items');

		core.emit(8, 'answer', ANSWER);
		expect(ask.answer).toEqual(ANSWER);
		expect(ask.phase).toBe('delivered');

		core.emit(9, 'done', { duration_ms: 1900, cost_usd: 0.004, judge_mode: 'jev', audit_id: 12 });
		expect(ask.done).toMatchObject({ duration_ms: 1900, judge_mode: 'jev' });
		expect(ask.busy).toBe(false);
	});

	it('ends in a refusal with its code and hint, and never holds an answer', async () => {
		const core = fakeCore();
		const ask = conversationOn(core);
		await ask.ask('Show me the function that exports CSV.', 'delta');
		await flush();

		core.emit(1, 'accepted', { id: 'q_1' });
		core.emit(2, 'classified', {
			class: 'forbidden',
			p: 0.97,
			checks: [{ id: 'asks_for_code', source: 'jev', p: 0.96, verdict: 'block' }]
		});
		core.emit(3, 'refused', {
			code: 'asks_for_code',
			message: "I can't show code.",
			hint: 'I can tell you whether the export is done and tested.'
		});

		expect(ask.phase).toBe('refused');
		expect(ask.refusal).toEqual({
			code: 'asks_for_code',
			message: "I can't show code.",
			hint: 'I can tell you whether the export is done and tested.'
		});
		expect(ask.answer).toBeNull();
		expect(ask.busy).toBe(false);
	});

	it('keeps the options of a clarify, and asks again with the same text and the pick', async () => {
		const core = fakeCore();
		core.queue('q_1', 'q_2');
		const ask = conversationOn(core);
		await ask.ask("What's done this week?", null);
		await flush();

		core.emit(1, 'accepted', { id: 'q_1' });
		core.emit(2, 'clarify', {
			kind: 'project',
			question: 'Which project do you mean?',
			options: [
				{ id: 'delta', label: 'Delta' },
				{ id: 'billing', label: 'Billing' }
			]
		});
		expect(ask.phase).toBe('clarify');
		expect(ask.clarify?.options.map((o) => o.id)).toEqual(['delta', 'billing']);
		expect(ask.canChoose).toBe(true);

		const first = core.stream;
		await ask.choose('billing');
		await flush();

		expect(first.closed).toBe(true);
		expect(core.posts[1]).toEqual({
			path: '/v1/questions',
			body: { text: "What's done this week?", clarify: { of: 'q_1', option: 'billing' } },
			key: 'key-2'
		});
		expect(ask.questionId).toBe('q_2');
		expect(ask.clarify).toBeNull();
		expect(core.stream.id).toBe('q_2');
	});

	it('gives the choice back when a pick could not be sent, and retries it with the same key', async () => {
		const core = fakeCore();
		core.queue('q_1', 'q_2');
		const ask = conversationOn(core);
		await ask.ask("What's done this week?", null);
		await flush();
		core.emit(1, 'accepted', { id: 'q_1' });
		core.emit(2, 'clarify', {
			kind: 'project',
			question: 'Which project do you mean?',
			options: [
				{ id: 'delta', label: 'Delta' },
				{ id: 'billing', label: 'Billing' }
			]
		});

		core.failNextPost(new ApiError(0, 'internal', "The core can't be reached."));
		await ask.choose('billing');
		await flush();

		expect(ask.phase).toBe('clarify');
		expect(ask.questionId).toBe('q_1');
		expect(ask.clarify?.options.map((o) => o.id)).toEqual(['delta', 'billing']);
		expect(ask.canChoose).toBe(true);
		expect(ask.failure?.message).toBeTruthy();
		expect(ask.entries.length).toBeGreaterThan(0);

		core.failNextPost(null);
		await ask.choose('billing');
		await flush();

		expect(core.posts.slice(1).map((p) => p.key)).toEqual(['key-2', 'key-2']);
		expect(ask.questionId).toBe('q_2');
		expect(ask.failure).toBeNull();
	});

	it('keeps the key of a send that failed, and takes a new one once a question was accepted', async () => {
		const core = fakeCore();
		core.queue('q_1', 'q_2');
		const ask = conversationOn(core);

		core.failNextPost(new ApiError(0, 'internal', "The core can't be reached."));
		await ask.ask('Is the build green?', 'delta');
		expect(ask.phase).toBe('failed');
		expect(ask.failure?.message).toBe("Mercury can't be reached.");
		expect(ask.busy).toBe(false);

		core.failNextPost(null);
		await ask.ask('Is the build green?', 'delta');
		await flush();
		// the same question, sent again: the same key, so the core won't start two
		expect(core.posts.map((p) => p.key)).toEqual(['key-1', 'key-1']);

		core.emit(1, 'accepted', { id: 'q_1' });
		core.emit(2, 'refused', { code: 'out_of_scope', message: 'No.', hint: 'Ask about progress.' });
		await ask.ask('Is the build green?', 'delta');
		// asked again on purpose, after it was accepted once: a new question
		expect(core.posts.map((p) => p.key)).toEqual(['key-1', 'key-1', 'key-2']);
	});

	it('ignores a second question while one is running', async () => {
		const core = fakeCore();
		const ask = conversationOn(core);
		await ask.ask('First?', 'delta');
		await ask.ask('Second?', 'delta');
		expect(core.posts).toHaveLength(1);
	});

	describe('after a reload', () => {
		it('asks where the question stands, then replays its stream once', async () => {
			const core = fakeCore();
			const ask = conversationOn(core);
			await ask.restore('q_1');
			await flush();

			expect(core.gets).toEqual(['/v1/questions/q_1']);
			expect(ask.phase).toBe('running');
			expect(core.stream.id).toBe('q_1');

			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'project', { project: 'delta', p: 1, source: 'explicit' });
			core.emit(3, 'classified', { class: 'progress', p: 0.9, checks: [] });
			// the browser reconnecting replays from where it was: nothing twice
			core.emit(3, 'classified', { class: 'progress', p: 0.9, checks: [] });
			core.emit(2, 'project', { project: 'delta', p: 1, source: 'explicit' });
			core.emit(4, 'composing', {});

			expect(ask.entries.map((e) => e.event.id)).toEqual(['1', '2', '3', '4']);
			// a replay is not live, so it has no arrival times to show
			expect(ask.entries.every((e) => e.at === null)).toBe(true);
		});

		it('shows a finished question from its state at once, and rebuilds the timeline', async () => {
			const core = fakeCore();
			core.setState({
				status: 'delivered',
				answer: ANSWER,
				judge_mode: 'jev',
				cost_usd: 0.004,
				duration_ms: 1900,
				audit_id: 12
			});
			const ask = conversationOn(core);
			await ask.restore('q_1');

			expect(ask.phase).toBe('delivered');
			expect(ask.answer).toEqual(ANSWER);
			expect(ask.done).toEqual({
				duration_ms: 1900,
				cost_usd: 0.004,
				judge_mode: 'jev',
				audit_id: 12
			});
			await flush();
			runUpToTheAnswer(core);
			core.emit(8, 'answer', ANSWER);
			expect(ask.entries).toHaveLength(8);
		});

		it('says so when the question is not there, and does not keep its id', async () => {
			const core = fakeCore();
			core.setState(new ApiError(404, 'invalid_request', 'No such question.'));
			const ask = conversationOn(core);
			const restoring = ask.restore('q_nope');
			// while it is fetched the core hasn't said it exists
			expect(ask.known).toBe(false);
			await restoring;

			expect(ask.phase).toBe('unavailable');
			expect(ask.questionId).toBeNull();
			expect(ask.known).toBe(false);
			expect(core.streams).toHaveLength(0);
		});

		it('fetches a question again when fetching it failed, and knows it once the core answers', async () => {
			const core = fakeCore();
			core.setState(new ApiError(0, 'internal', "The core can't be reached."));
			const ask = conversationOn(core);
			await ask.restore('q_1');
			expect(ask.phase).toBe('failed');
			expect(ask.known).toBe(false);

			core.setState({ status: 'delivered', answer: ANSWER });
			await ask.restore('q_1');
			expect(ask.phase).toBe('delivered');
			expect(ask.known).toBe(true);
			expect(core.gets).toEqual(['/v1/questions/q_1', '/v1/questions/q_1']);
		});

		it('answers a clarify brought back with the words this tab kept of its question', async () => {
			const core = fakeCore();
			core.queue('q_2');
			core.setState({ status: 'clarify', project: null });
			const ask = conversationOn(core);
			await ask.restore('q_1', "What's done this week?");
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'clarify', {
				kind: 'project',
				question: 'Which project do you mean?',
				options: [
					{ id: 'delta', label: 'Delta' },
					{ id: 'billing', label: 'Billing' }
				]
			});

			expect(ask.phase).toBe('clarify');
			expect(ask.canChoose).toBe(true);
			await ask.choose('delta');
			expect(core.posts).toEqual([
				{
					path: '/v1/questions',
					body: { text: "What's done this week?", clarify: { of: 'q_1', option: 'delta' } },
					key: 'key-1'
				}
			]);
		});

		it('cannot ask a clarify again after a reload, because the browser does not keep the words', async () => {
			const core = fakeCore();
			core.setState({ status: 'clarify' });
			const ask = conversationOn(core);
			await ask.restore('q_1');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'clarify', {
				kind: 'project',
				question: 'Which project do you mean?',
				options: [{ id: 'delta', label: 'Delta' }]
			});

			expect(ask.phase).toBe('clarify');
			expect(ask.canChoose).toBe(false);
			await ask.choose('delta');
			expect(core.posts).toHaveLength(0);
		});

		it('says the answer is no longer kept when the retention emptied it, not that the connection was lost', async () => {
			const core = fakeCore();
			core.setState({ status: 'delivered', answer: null, audit_id: 7, judge_mode: 'jev' });
			const ask = conversationOn(core);
			await ask.restore('q_1');
			expect(ask.phase).toBe('expired');
			expect(ask.busy).toBe(false);

			// the replay: every row emptied, the last one too
			await flush();
			core.emit(1, 'accepted', null);
			core.emit(2, 'answer', null);
			core.emit(3, 'done', null);
			await flush();
			expect(ask.phase).toBe('expired');
			expect(ask.answer).toBeNull();
			expect(ask.entries).toEqual([]);
		});

		it('says the same of a refusal the retention emptied', async () => {
			const core = fakeCore();
			core.setState({ status: 'refused', refusal: null });
			const ask = conversationOn(core);
			await ask.restore('q_1');
			expect(ask.phase).toBe('expired');
			expect(ask.refusal).toBeNull();
		});

		it('says the same of a question whose stream ends on an emptied clarify', async () => {
			const core = fakeCore();
			core.setState({ status: 'clarify', project: null });
			const ask = conversationOn(core);
			await ask.restore('q_1');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'clarify', null);
			await flush();
			expect(ask.phase).toBe('expired');
			expect(ask.canChoose).toBe(false);
		});

		it('does nothing when the question is already on the screen', async () => {
			const core = fakeCore();
			const ask = conversationOn(core);
			await ask.ask('Is the build green?', 'delta');
			await flush();
			await ask.restore('q_1');
			expect(core.gets).toEqual([]);
			expect(core.streams).toHaveLength(1);
		});
	});

	describe('a question held for sign-off', () => {
		it('shows why, then polls until the approver has decided', async () => {
			const core = fakeCore();
			const ask = conversationOn(core, 5000);
			await ask.ask('Is the export ready for the client?', 'delta');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'pending_signoff', { approval_id: 'ap_1', reason: 'review_band' });

			expect(ask.phase).toBe('pending_signoff');
			expect(ask.signoff).toEqual({ approval_id: 'ap_1', reason: 'review_band' });
			expect(ask.answer).toBeNull();

			core.setState({ status: 'pending_signoff' });
			await vi.advanceTimersByTimeAsync(5000);
			expect(core.gets).toEqual(['/v1/questions/q_1']);
			expect(ask.phase).toBe('pending_signoff');

			core.setState({
				status: 'delivered',
				answer: ANSWER,
				judge_mode: 'jev',
				cost_usd: 0.01,
				duration_ms: 4000,
				audit_id: 3
			});
			await vi.advanceTimersByTimeAsync(5000);
			expect(ask.phase).toBe('delivered');
			expect(ask.answer).toEqual(ANSWER);

			// decided: no more polling
			await vi.advanceTimersByTimeAsync(20000);
			expect(core.gets).toHaveLength(2);
		});

		it('follows the stream past the hold: the approved answer and its done row arrive on it', async () => {
			const core = fakeCore();
			const ask = conversationOn(core, 5000);
			await ask.ask('Is the export ready for the client?', 'delta');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'pending_signoff', { approval_id: 'ap_1', reason: 'review_band' });
			expect(ask.phase).toBe('pending_signoff');
			expect(core.stream.closed).toBe(false);

			core.emit(4, 'answer', ANSWER);
			core.emit(5, 'done', DONE);

			expect(ask.phase).toBe('delivered');
			expect(ask.answer).toEqual(ANSWER);
			expect(ask.done).toEqual(DONE);
			expect(ask.entries.map((e) => e.event.event)).toEqual([
				'accepted',
				'pending_signoff',
				'answer',
				'done'
			]);
			// decided on the stream: the poll stops
			await vi.advanceTimersByTimeAsync(20000);
			expect(core.gets).toEqual([]);
		});

		it('after a reload of an approved question, its replayed hold never puts it back to waiting', async () => {
			const core = fakeCore();
			const ask = conversationOn(core, 5000);
			core.setState({ status: 'delivered', answer: ANSWER, judge_mode: 'jev', audit_id: 4 });
			await ask.restore('q_1');
			await flush();
			expect(ask.phase).toBe('delivered');

			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'pending_signoff', { approval_id: 'ap_1', reason: 'review_band' });
			expect(ask.phase).toBe('delivered');
			core.emit(4, 'answer', ANSWER);
			core.emit(5, 'done', DONE);

			expect(ask.phase).toBe('delivered');
			expect(ask.done).toEqual(DONE);
			expect(ask.entries.map((e) => e.event.event)).toContain('done');
			await vi.advanceTimersByTimeAsync(20000);
			expect(core.gets).toEqual(['/v1/questions/q_1']);
		});

		it('shows the approver’s note when the answer was rejected', async () => {
			const core = fakeCore();
			const ask = conversationOn(core, 1000);
			await ask.ask('Is the export ready for the client?', 'delta');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'pending_signoff', { approval_id: 'ap_1', reason: 'class' });

			core.setState({
				status: 'refused',
				refusal: {
					code: 'rejected_by_approver',
					message: 'An approver did not release this answer.',
					hint: 'Ask the tech lead about the export first.'
				}
			});
			await vi.advanceTimersByTimeAsync(1000);

			expect(ask.phase).toBe('refused');
			expect(ask.refusal?.hint).toBe('Ask the tech lead about the export first.');
			expect(ask.answer).toBeNull();
		});

		/** Asks, is held, and is rejected on the stream with the approver's note. */
		async function rejected(core: ReturnType<typeof fakeCore>, ask: Conversation) {
			await ask.ask('Is the export ready for the client?', 'delta');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'pending_signoff', { approval_id: 'ap_1', reason: 'class' });
			core.emit(3, 'refused', {
				code: 'rejected_by_approver',
				message: 'An approver did not release this answer.',
				hint: 'Ask the tech lead about the export first.',
				cost_usd: 0.004,
				judge_mode: 'jev'
			});
		}

		it('asks again a question an approver rejected, naming it as follows', async () => {
			const core = fakeCore();
			const ask = conversationOn(core, 1000);
			await rejected(core, ask);
			expect(ask.phase).toBe('refused');
			expect(ask.canAskAgain).toBe(true);
			expect(ask.following).toBeNull();

			// the words go back in the box; the next question names the rejected one
			expect(ask.askAgain()).toBe('Is the export ready for the client?');
			expect(ask.following).toBe('q_1');
			core.queue('q_2');
			await ask.ask('Is the export for the client ready now?', 'delta');

			expect(core.posts[1]).toEqual({
				path: '/v1/questions',
				body: { text: 'Is the export for the client ready now?', project: 'delta', follows: 'q_1' },
				key: 'key-2'
			});
			expect(ask.questionId).toBe('q_2');
			expect(ask.following).toBeNull();
			expect(ask.canAskAgain).toBe(false);

			// the question after it is a question of its own
			await flush();
			core.emit(1, 'accepted', { id: 'q_2' });
			core.emit(2, 'answer', ANSWER);
			expect(ask.phase).toBe('delivered');
			core.queue('q_3');
			await ask.ask('What else is done?', 'delta');
			expect(core.posts[2].body).toEqual({ text: 'What else is done?', project: 'delta' });
		});

		it('offers to ask again only after an approver’s rejection', async () => {
			const core = fakeCore();
			const ask = conversationOn(core);
			await ask.ask('Show me the function that exports CSV.', 'delta');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'refused', {
				code: 'asks_for_code',
				message: 'Mercury does not show code.',
				hint: 'Ask what the Export module does.',
				cost_usd: 0.001,
				judge_mode: 'jev'
			});
			expect(ask.phase).toBe('refused');
			expect(ask.canAskAgain).toBe(false);
			expect(ask.askAgain()).toBe('');
			expect(ask.following).toBeNull();

			core.queue('q_2');
			await ask.ask('What does the Export module do?', 'delta');
			expect(core.posts[1].body).toEqual({
				text: 'What does the Export module do?',
				project: 'delta'
			});
		});

		it('keeps follows for a retry of a send that failed, but not once the core refused the request', async () => {
			const core = fakeCore();
			const ask = conversationOn(core, 1000);
			await rejected(core, ask);
			ask.askAgain();

			core.failNextPost(new ApiError(503, 'internal', 'The server is unavailable.'));
			await ask.ask('Is the export ready now?', 'delta');
			expect(ask.phase).toBe('failed');
			expect(ask.following).toBe('q_1');
			core.failNextPost(null);
			core.queue('q_2');
			await ask.ask('Is the export ready now?', 'delta');
			expect(core.posts[2]).toMatchObject({
				body: { text: 'Is the export ready now?', project: 'delta', follows: 'q_1' },
				key: 'key-2'
			});
			expect(core.posts[1].key).toBe('key-2');

			// a link the core won't make: the person's next send goes without it
			const other = fakeCore();
			const again = conversationOn(other, 1000);
			await rejected(other, again);
			again.askAgain();
			other.failNextPost(
				new ApiError(
					400,
					'invalid_request',
					FOLLOWS_REFUSED,
					"Name your own question that ended with an approver's rejection, or ask without it."
				)
			);
			await again.ask('Is the export ready now?', 'delta');
			expect(again.phase).toBe('failed');
			expect(again.following).toBeNull();
			other.queue('q_2');
			await again.ask('Is the export ready now?', 'delta');
			expect(other.posts[2].body).toEqual({ text: 'Is the export ready now?', project: 'delta' });
		});

		it('keeps follows after a 400 that is about the words, so the corrected question still names the rejected one', async () => {
			const core = fakeCore();
			const ask = conversationOn(core, 1000);
			await rejected(core, ask);
			ask.askAgain();

			core.failNextPost(
				new ApiError(400, 'invalid_request', 'The question must be 1 to 2,000 characters.')
			);
			await ask.ask(`Is the export ready now? ${'Please. '.repeat(300)}`, 'delta');
			expect(ask.phase).toBe('failed');
			expect(ask.following).toBe('q_1');

			core.failNextPost(null);
			core.queue('q_2');
			await ask.ask('Is the export ready now?', 'delta');
			expect(core.posts[2].body).toEqual({
				text: 'Is the export ready now?',
				project: 'delta',
				follows: 'q_1'
			});
			expect(ask.questionId).toBe('q_2');
			expect(ask.following).toBeNull();
		});

		it('forgets a question to ask again with a new conversation', async () => {
			const core = fakeCore();
			const ask = conversationOn(core, 1000);
			await rejected(core, ask);
			ask.askAgain();
			ask.reset();
			expect(ask.following).toBeNull();
			core.queue('q_2');
			await ask.ask('Is the export ready now?', 'delta');
			expect(core.posts[1].body).toEqual({ text: 'Is the export ready now?', project: 'delta' });
		});

		it('stops polling when the conversation is reset', async () => {
			const core = fakeCore();
			const ask = conversationOn(core, 1000);
			await ask.ask('Is the export ready?', 'delta');
			await flush();
			core.emit(1, 'pending_signoff', { approval_id: 'ap_1', reason: 'class' });
			ask.reset();
			await vi.advanceTimersByTimeAsync(10000);
			expect(core.gets).toEqual([]);
		});
	});

	describe('when the stream breaks', () => {
		it('takes the answer from the core’s state if the question had finished', async () => {
			const core = fakeCore();
			const ask = conversationOn(core);
			await ask.ask('Is the build green?', 'delta');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.setState({ status: 'delivered', answer: ANSWER, judge_mode: 'chat', audit_id: 4 });

			// the transport's own terminal error: no event id
			core.emit('', 'error', { code: 'internal', message: 'The connection to the core was lost.' });
			await flush();

			expect(ask.phase).toBe('delivered');
			expect(ask.answer).toEqual(ANSWER);
			expect(ask.failure).toBeNull();
		});

		it('offers to reconnect when the question is still running, and does not repeat events', async () => {
			const core = fakeCore();
			const ask = conversationOn(core);
			await ask.ask('Is the build green?', 'delta');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'project', { project: 'delta', p: 1, source: 'explicit' });
			core.emit('', 'error', { code: 'internal', message: 'The connection to the core was lost.' });
			await flush();
			expect(ask.phase).toBe('disconnected');

			ask.reconnect();
			await flush();
			expect(core.streams).toHaveLength(2);
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'project', { project: 'delta', p: 1, source: 'explicit' });
			core.emit(3, 'composing', {});
			// the break itself is not a row of the timeline
			expect(ask.entries.map((e) => e.event.event)).toEqual(['accepted', 'project', 'composing']);
			expect(ask.phase).toBe('running');
		});

		it('ends in a failure, in words, when the core says the question failed', async () => {
			const core = fakeCore();
			const ask = conversationOn(core);
			await ask.ask('Is the build green?', 'delta');
			await flush();
			core.emit(1, 'accepted', { id: 'q_1' });
			core.emit(2, 'error', { code: 'budget_exceeded', message: 'Over the budget.' });

			expect(ask.phase).toBe('failed');
			// the stream's budget_exceeded is the question's own cap, not the day's
			expect(ask.failure?.message).toBe(
				'This question reached its spending limit, so Mercury stopped it.'
			);
			expect(ask.answer).toBeNull();
		});
	});

	describe('ending the conversation', () => {
		it('closes the stream and forgets the question, and hears nothing more from it', async () => {
			const core = fakeCore();
			const ask = conversationOn(core);
			await ask.ask('Is the build green?', 'delta');
			await flush();
			const stream = core.stream;
			core.emit(1, 'accepted', { id: 'q_1' });

			ask.reset();
			expect(stream.closed).toBe(true);
			expect(ask.phase).toBe('idle');
			expect(ask.questionId).toBeNull();
			expect(ask.entries).toEqual([]);

			core.emit(2, 'answer', ANSWER, stream);
			expect(ask.answer).toBeNull();
			expect(ask.entries).toEqual([]);
		});

		it('drops a send that was answered after the conversation moved on', async () => {
			const core = fakeCore();
			const ask = conversationOn(core);
			const pending = ask.ask('Is the build green?', 'delta');
			ask.reset();
			await pending;
			await flush();
			expect(ask.phase).toBe('idle');
			expect(core.streams).toHaveLength(0);
		});
	});

	it('ends with the session: whoever is signed out has no question on screen', () => {
		conversation.questionId = 'q_1';
		conversation.text = 'Is the build green?';
		conversation.phase = 'running';
		session.status = 'signed-in';

		session.sessionEnded();

		expect(session.status).toBe('signed-out');
		expect(conversation.phase).toBe('idle');
		expect(conversation.questionId).toBeNull();
		expect(conversation.text).toBe('');
	});

	it('counts what the checks withheld and removed, for the notes under the answer', async () => {
		const core = fakeCore();
		const ask = conversationOn(core);
		await ask.ask('What is in progress in billing?', 'billing');
		await flush();
		core.emit(1, 'tool_result', {
			call: 0,
			items: 4,
			withheld_fields: ['/items/1/title', '/items/2/title'],
			access: 'metadata'
		});
		core.emit(2, 'judged', { sentences: 5, stripped: 1, held: 0 });

		expect(ask.withheld).toBe(2);
		expect(ask.stripped).toBe(1);
		expect(ask.held).toBe(0);
	});
});

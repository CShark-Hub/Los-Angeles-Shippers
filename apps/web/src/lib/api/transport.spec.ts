import { afterEach, describe, expect, expectTypeOf, it, vi } from 'vitest';

import {
	ApiError,
	EVENT_NAMES,
	httpTransport,
	type EventName,
	type StreamEvent
} from './transport';

function stubFetch(status: number, body: unknown) {
	const fetch = vi.fn(async () => new Response(JSON.stringify(body), { status }));
	vi.stubGlobal('fetch', fetch);
	return fetch;
}

/** Enough of `EventSource` to dispatch named events and see whether it was closed. */
class FakeEventSource {
	static readonly CONNECTING = 0;
	static readonly OPEN = 1;
	static readonly CLOSED = 2;
	static last: FakeEventSource;
	listeners = new Map<string, (event: Event) => void>();
	readyState = FakeEventSource.OPEN;
	closed = false;
	constructor(readonly url: string) {
		FakeEventSource.last = this;
	}
	addEventListener(name: string, listener: (event: Event) => void) {
		this.listeners.set(name, listener);
	}
	close() {
		this.closed = true;
		this.readyState = FakeEventSource.CLOSED;
	}
	emit(name: string, id: string, data: unknown) {
		this.emitRaw(name, id, JSON.stringify(data));
	}
	emitRaw(name: string, id: string, data: string) {
		this.listeners.get(name)?.(new MessageEvent(name, { data, lastEventId: id }));
	}
	/** The connection drops: the browser retries (`CONNECTING`) or gives up (`CLOSED`). */
	drop(readyState: number) {
		this.readyState = readyState;
		this.listeners.get('error')?.(new Event('error'));
	}
}

function openStream() {
	vi.stubGlobal('EventSource', FakeEventSource);
	const seen: StreamEvent[] = [];
	httpTransport().events('q_1', (event) => seen.push(event));
	return { source: FakeEventSource.last, seen };
}

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('httpTransport', () => {
	it('sends every request to its own origin with the cookie and the client header', async () => {
		const fetch = stubFetch(202, { id: 'q_1' });
		const api = httpTransport();
		await api.get('/v1/health');
		await api.post('/v1/questions', { text: 'What changed?' }, { idempotencyKey: 'k-1' });

		const calls = fetch.mock.calls as unknown as [string, RequestInit][];
		expect(calls.map(([url]) => url)).toEqual(['/v1/health', '/v1/questions']);
		const inits = calls.map(([, init]) => init);
		const postInit = inits[1];
		for (const init of inits) {
			expect(init.credentials).toBe('same-origin');
			expect(init.headers).toMatchObject({ 'X-Mercury-Client': 'web' });
		}
		expect(postInit.headers).toMatchObject({
			'Content-Type': 'application/json',
			'Idempotency-Key': 'k-1'
		});
		expect(postInit.body).toBe('{"text":"What changed?"}');
	});

	it('signs out with a DELETE that carries the cookie and the client header too', async () => {
		const fetch = vi.fn(async () => new Response(null, { status: 204 }));
		vi.stubGlobal('fetch', fetch);
		await expect(httpTransport().delete('/v1/session')).resolves.toBeUndefined();

		const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
		expect(url).toBe('/v1/session');
		expect(init.method).toBe('DELETE');
		expect(init.credentials).toBe('same-origin');
		expect(init.headers).toMatchObject({ 'X-Mercury-Client': 'web' });
		expect(init.body).toBeUndefined();
	});

	it('keeps how long a 429 asks the caller to wait', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(
				async () =>
					new Response(
						JSON.stringify({ error: { code: 'rate_limited', message: 'Slow down.', hint: null } }),
						{ status: 429, headers: { 'Retry-After': '6' } }
					)
			)
		);
		await expect(httpTransport().post('/v1/session', { token: 'mcy_x' })).rejects.toMatchObject({
			status: 429,
			code: 'rate_limited',
			retryAfter: 6,
			hint: undefined
		});
	});

	it("turns the core's error body into an ApiError", async () => {
		stubFetch(403, {
			error: { code: 'forbidden_class', message: 'Not for your role.', hint: 'Ask about health.' }
		});
		const failure = httpTransport().get('/v1/me');
		await expect(failure).rejects.toBeInstanceOf(ApiError);
		await expect(failure).rejects.toMatchObject({
			status: 403,
			code: 'forbidden_class',
			hint: 'Ask about health.'
		});
	});

	it('listens for every event the core can send, once', () => {
		// a name the core's types have and the list lacks makes this line a type error; a name the
		// list has and the core's types lack is one where the list is declared
		expectTypeOf<Exclude<EventName, (typeof EVENT_NAMES)[number]>>().toEqualTypeOf<never>();
		expect(new Set(EVENT_NAMES).size).toBe(EVENT_NAMES.length);
	});

	it('streams events until a terminal one, then closes', () => {
		vi.stubGlobal('EventSource', FakeEventSource);
		const seen: StreamEvent[] = [];
		httpTransport().events('q_1', (event) => seen.push(event));
		const source = FakeEventSource.last;

		expect(source.url).toBe('/v1/questions/q_1/events');
		expect([...source.listeners.keys()]).toEqual([...EVENT_NAMES]);
		source.emit('accepted', '1', { id: 'q_1' });
		expect(source.closed).toBe(false);
		// a connection error comes as a plain Event, and is left to the browser's retry
		source.drop(FakeEventSource.CONNECTING);
		expect(source.closed).toBe(false);
		source.emit('done', '2', { duration_ms: 10 });

		expect(seen.map((e) => [e.event, e.id])).toEqual([
			['accepted', '1'],
			['done', '2']
		]);
		expect(source.closed).toBe(true);
	});

	it("keeps a held question's stream open past pending_signoff, for the decision that follows it", () => {
		vi.stubGlobal('EventSource', FakeEventSource);
		const seen: StreamEvent[] = [];
		httpTransport().events('q_1', (event) => seen.push(event));
		const source = FakeEventSource.last;

		source.emit('pending_signoff', '3', { approval_id: 'ap_1', reason: 'review_band' });
		// the core ends the connection while it waits; the browser comes back with Last-Event-ID
		expect(source.closed).toBe(false);
		source.drop(FakeEventSource.CONNECTING);
		source.emit('answer', '5', { text: 'Done.', facts: [], confidence: 0.8 });
		expect(source.closed).toBe(false);
		source.emit('done', '6', { duration_ms: 10 });

		expect(seen.map((e) => e.event)).toEqual(['pending_signoff', 'answer', 'done']);
		expect(source.closed).toBe(true);
	});

	it('tells each drop the browser retries, so the caller can ask where the question stands', () => {
		vi.stubGlobal('EventSource', FakeEventSource);
		const seen: StreamEvent[] = [];
		let drops = 0;
		httpTransport().events(
			'q_1',
			(event) => seen.push(event),
			() => (drops += 1)
		);
		const source = FakeEventSource.last;
		source.emit('accepted', '1', { id: 'q_1' });
		source.drop(FakeEventSource.CONNECTING);
		source.drop(FakeEventSource.CONNECTING);

		expect(drops).toBe(2);
		expect(source.closed).toBe(false);
		expect(seen.map((e) => e.event)).toEqual(['accepted']);
	});

	it('reports a stream the browser closed for good as a terminal error', () => {
		const { source, seen } = openStream();
		source.emit('accepted', '1', { id: 'q_1' });
		// a 401 or a 502 on the reconnect: the browser gives up and closes the stream itself
		source.drop(FakeEventSource.CLOSED);

		expect(seen.map((e) => e.event)).toEqual(['accepted', 'error']);
		expect(seen[1]).toMatchObject({ id: '', data: { code: 'internal' } });
	});

	it('closes at a terminal event before reading its data, and reports unreadable data as its own error', () => {
		const { source, seen } = openStream();
		expect(() => source.emitRaw('error', '4', '')).not.toThrow();

		expect(source.closed).toBe(true);
		// its own: no id, so nobody takes its zero cost and missing judge for the core's record
		expect(seen).toEqual([
			{
				event: 'error',
				id: '',
				data: { code: 'internal', message: expect.any(String), cost_usd: 0, judge_mode: null }
			}
		]);
	});

	it('ends the stream with its own error when any event is unreadable', () => {
		const { source, seen } = openStream();
		source.emit('accepted', '1', { id: 'q_1' });
		source.emitRaw('plan', '2', '{"calls": [');

		expect(seen.map((e) => [e.event, e.id])).toEqual([
			['accepted', '1'],
			['error', '']
		]);
		expect(source.closed).toBe(true);
	});

	it("turns a core it can't reach, and a body that isn't JSON, into an ApiError", async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new TypeError('Failed to fetch');
			})
		);
		const unreachable = httpTransport().get('/v1/health');
		await expect(unreachable).rejects.toBeInstanceOf(ApiError);
		await expect(unreachable).rejects.toMatchObject({ status: 0, code: 'internal' });

		// what Vite's index.html fallback answers for a path it doesn't proxy
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response('<!doctype html>', { status: 200 }))
		);
		const html = httpTransport().get('/v1/health');
		await expect(html).rejects.toBeInstanceOf(ApiError);
		await expect(html).rejects.toMatchObject({ status: 200, code: 'internal' });
	});
});

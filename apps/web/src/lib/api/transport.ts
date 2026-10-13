/**
 * The only way the app talks to the core. Every request goes to the app's own origin, with the
 * session cookie (`credentials: 'same-origin'`) and `X-Mercury-Client: web`, which the core's CSRF
 * check requires on cookie-authenticated writes. Screens use a `Transport`; they never call
 * `fetch` or open an `EventSource` themselves.
 */

import type { CoreEvent, ErrorCode } from './types';

/** The header every request carries. */
export const CLIENT_HEADER = { 'X-Mercury-Client': 'web' } as const;

/**
 * The event names of a question's stream, as the core's API contract lists them. `EventSource`
 * delivers a named event only to a listener registered for that name, so the list lives here; a
 * test fails when it lacks a name the core's types have.
 */
export const EVENT_NAMES = [
	'accepted',
	'project',
	'classified',
	'refused',
	'clarify',
	'plan',
	'tool_checked',
	'tool_result',
	'composing',
	'judged',
	'pending_signoff',
	'answer',
	'done',
	'error'
] as const satisfies readonly EventName[];

export type EventName = CoreEvent['event'];

/** After one of these the core sends nothing more for the question (`pending_signoff` until decided). */
export const TERMINAL_EVENTS: readonly EventName[] = [
	'refused',
	'clarify',
	'pending_signoff',
	'done',
	'error'
];

export function isTerminal(event: EventName): boolean {
	return TERMINAL_EVENTS.includes(event);
}

/**
 * One event of a question's stream: the core's `Event` (its name and the data that name has) with
 * its id, what `Last-Event-ID` replays from. An `error` the transport reports itself, for a stream
 * it can no longer hear or an event it can't read, has the id `''`: it is no record of the core's.
 */
export type StreamEvent = CoreEvent & { id: string };

/**
 * A request the core refused: the status and the body's `{"error": {code, message, hint}}`.
 * `retryAfter` is the seconds a `429` asks the caller to wait. Status `0` is a core that could
 * not be reached at all.
 */
export class ApiError extends Error {
	constructor(
		readonly status: number,
		readonly code: ErrorCode,
		message: string,
		readonly hint?: string,
		readonly retryAfter?: number
	) {
		super(message);
		this.name = 'ApiError';
	}
}

export interface Transport {
	get<T>(path: string): Promise<T>;
	post<T>(path: string, body: unknown, options?: { idempotencyKey?: string }): Promise<T>;
	delete<T>(path: string): Promise<T>;
	/**
	 * Streams a question's events until a terminal one, and returns a function that stops early.
	 * `onDropped` hears each time the connection drops while the browser tries again: a stream the
	 * core closed on purpose (its question's closing row couldn't be written) comes back each time
	 * without the event that would end it, so the caller asks where the question stands.
	 */
	events(
		questionId: string,
		onEvent: (event: StreamEvent) => void,
		onDropped?: () => void
	): () => void;
}

async function apiError(response: Response): Promise<ApiError> {
	const body = (await response.json().catch(() => undefined)) as
		{ error?: { code?: ErrorCode; message?: string; hint?: string | null } } | undefined;
	const error = body?.error;
	const wait = Number(response.headers.get('Retry-After'));
	return new ApiError(
		response.status,
		error?.code ?? 'internal',
		error?.message ?? `The core answered ${response.status}.`,
		error?.hint ?? undefined,
		Number.isFinite(wait) && wait > 0 ? wait : undefined
	);
}

/** The transport to the core behind the app's own origin. */
export function httpTransport(): Transport {
	async function request<T>(
		method: string,
		path: string,
		body?: unknown,
		headers: Record<string, string> = {}
	): Promise<T> {
		let response: Response;
		try {
			response = await fetch(path, {
				method,
				credentials: 'same-origin',
				headers: {
					...CLIENT_HEADER,
					...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
					...headers
				},
				body: body === undefined ? undefined : JSON.stringify(body)
			});
		} catch {
			throw new ApiError(0, 'internal', "The core can't be reached.");
		}
		if (!response.ok) throw await apiError(response);
		if (response.status === 204) return undefined as T;
		try {
			return (await response.json()) as T;
		} catch {
			throw new ApiError(
				response.status,
				'internal',
				`The core answered ${response.status} with something that isn't JSON.`
			);
		}
	}

	return {
		get: (path) => request('GET', path),
		delete: (path) => request('DELETE', path),
		post: (path, body, options) =>
			request(
				'POST',
				path,
				body,
				options?.idempotencyKey ? { 'Idempotency-Key': options.idempotencyKey } : {}
			),
		events(questionId, onEvent, onDropped) {
			// A GET on the same origin: the cookie goes with it, and a dropped connection is
			// retried by the browser with Last-Event-ID, so the core replays what was missed.
			const source = new EventSource(`/v1/questions/${encodeURIComponent(questionId)}/events`);
			// The contract's terminal `error`, reported by the transport itself, so a caller always
			// hears how a stream ended. It always has the id `''`: it is no event of the core's, and
			// the transport can't know what the question cost nor which judge answered, so its zero
			// and its missing judge are no record. A caller asks the question's state, which knows.
			const fail = (message: string) => {
				source.close();
				onEvent({
					event: 'error',
					id: '',
					data: { code: 'internal', message, cost_usd: 0, judge_mode: null }
				});
			};
			for (const name of EVENT_NAMES) {
				source.addEventListener(name, (message) => {
					if (!(message instanceof MessageEvent)) {
						// The browser's own connection errors arrive as `error` too, without data. While it
						// retries (CONNECTING) the caller hears of each drop; once it gives up (CLOSED), say so.
						if (source.readyState === EventSource.CLOSED) {
							fail('The connection to the core was lost.');
						} else {
							onDropped?.();
						}
						return;
					}
					// after a terminal event the stream ends, whatever its data holds; but a held question
					// goes on past `pending_signoff`: the core ends the connection while it waits, and
					// the browser comes back with Last-Event-ID for the decision it replays
					if (isTerminal(name) && name !== 'pending_signoff') source.close();
					let data: unknown;
					try {
						data = JSON.parse(message.data);
					} catch {
						fail("The core sent an event the app can't read.");
						return;
					}
					// the one place JSON becomes a typed event: the core sends what its schema says
					onEvent({ event: name, id: message.lastEventId, data } as StreamEvent);
				});
			}
			return () => source.close();
		}
	};
}

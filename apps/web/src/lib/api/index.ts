import { ApiError, httpTransport, type Transport } from './transport';

export {
	ApiError,
	isTerminal,
	type EventName,
	type StreamEvent,
	type Transport
} from './transport';
export type {
	Accepted,
	AccessLimit,
	AccessMode,
	AdminAlert,
	AdminGrant,
	AdminOverview,
	AdminPeople,
	AdminPerson,
	AdminProbe,
	AdminRepositories,
	AdminRepository,
	AdminSettings,
	AdminToken,
	AlertKind,
	Answer,
	Approval,
	ApprovalDecision,
	ApprovalList,
	AuditItem,
	AuditPage,
	Bucket,
	Channel,
	CheckLine,
	CheckResult,
	Clarify,
	ClarifyAnswer,
	ComposerMode,
	CoreEvent,
	Decision,
	Done,
	Ending,
	ErrorCode,
	Evidence,
	Fact,
	FactStatus,
	FactTested,
	Flag,
	Grant,
	JudgeMode,
	Me,
	ModelRoleLine,
	NewQuestion,
	OutcomeCounts,
	OverviewWindow,
	PendingSignoff,
	ProbeState,
	ProjectList,
	ProjectPicked,
	ProjectSummary,
	QuestionClass,
	QuestionState,
	QuestionStatus,
	Refusal,
	RefusalCode,
	Role,
	SeriesPoint,
	SessionRequest,
	SignoffReason,
	TokenState,
	Verdict
} from './types';

/** True when the app runs on recorded questions instead of a core (`VITE_MERCURY_MOCK=true`). */
export const MOCK_MODE = import.meta.env.VITE_MERCURY_MOCK === 'true';

/** The accounts the sign-in page offers in mock mode; nothing in a production build, which has no mock. */
export const mockSignInAccounts: Promise<typeof import('./mock').SIGN_IN_ACCOUNTS> | undefined =
	MOCK_MODE ? import('./mock').then(({ SIGN_IN_ACCOUNTS }) => SIGN_IN_ACCOUNTS) : undefined;

/** The questions the Ask page offers in mock mode; nothing in a production build. */
export const mockSamples: Promise<typeof import('./mock').SAMPLES> | undefined = MOCK_MODE
	? import('./mock').then(({ SAMPLES }) => SAMPLES)
	: undefined;

/** The session's own route: a `401` there is an answer to the request, not a session that ended. */
const SESSION_PATH = '/v1/session';

let heardOfAnEndedSession: (() => void) | undefined;

/**
 * Who hears of it when a request is refused because the session is gone: a `401` on any route but
 * the session's own. There is one listener, the session; it decides whether anyone was signed in.
 */
export function onSessionEnded(listener: () => void): void {
	heardOfAnEndedSession = listener;
}

/** The same transport, telling [`onSessionEnded`] about every `401` that isn't a sign-in's. */
export function watchingTheSession(base: Transport): Transport {
	async function watch<T>(path: string, call: () => Promise<T>): Promise<T> {
		try {
			return await call();
		} catch (error) {
			if (error instanceof ApiError && error.status === 401 && path !== SESSION_PATH) {
				heardOfAnEndedSession?.();
			}
			throw error;
		}
	}
	return {
		get: (path) => watch(path, () => base.get(path)),
		post: (path, body, options) => watch(path, () => base.post(path, body, options)),
		delete: (path) => watch(path, () => base.delete(path)),
		events: (questionId, onEvent, onDropped) => base.events(questionId, onEvent, onDropped)
	};
}

let transport: Promise<Transport> | undefined;

/** The app's one transport: the recorded mock in mock mode, otherwise the core on this origin. */
export function getTransport(): Promise<Transport> {
	transport ??= (
		MOCK_MODE
			? import('./mock').then(({ mockTransport }) =>
					mockTransport({ signedIn: import.meta.env.VITE_MERCURY_MOCK_SIGNED_IN === 'true' })
				)
			: Promise.resolve(httpTransport())
	).then(watchingTheSession);
	return transport;
}

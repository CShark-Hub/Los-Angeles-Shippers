import type { Answer, CoreEvent, Done } from '../types';

/** One recorded event: the core's event, whose data the generated types check, and its pause. */
export type RecordedEvent = CoreEvent & { after_ms: number };

/** A question as the core streamed it: each event with the time since the one before. */
export interface Recording {
	/** What the sample button says. Left out for a recording only a clarify leads to. */
	sample?: string;
	/** What was asked to get this recording; `clarify` marks the answer to a clarify of another. */
	question: { text: string; project?: string; clarify?: { of: string; option: string } };
	id: string;
	events: RecordedEvent[];
	/** For a question held for sign-off: what an approver releases, and how long that takes. */
	approved?: { after_ms: number; answer: Answer; done: Done };
}

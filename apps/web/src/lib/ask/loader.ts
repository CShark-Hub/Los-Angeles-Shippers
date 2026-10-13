/**
 * What the loader says while a question runs: one stage, from the name of the last event, never from
 * anything an event says.
 */

import type { Entry, Phase } from './conversation.svelte';
import { callsTheCodeReader } from './timeline';

const STAGES: Record<string, string> = {
	accepted: 'Reading your question',
	project: 'Finding the project',
	classified: 'Checking the question',
	plan: 'Deciding what to look up',
	tool_checked: 'Checking what to look up',
	tool_result: 'Reading the repository',
	composing: 'Writing the answer',
	judged: 'Checking every sentence',
	answer: 'Almost there'
};

/** The stage the question is in, as the loader names it (without its trailing dots). */
export function stageOf(entries: Entry[], phase: Phase): string {
	if (phase === 'sending') return 'Sending your question';
	if (phase === 'loading') return 'Finding your question';
	const last = entries.at(-1)?.event.event;
	// The core sends every call's checks first and then runs the calls at once, so the time after the
	// last check is the code reader's, which is the slow call: say so, rather than "checking".
	if (
		(last === 'plan' || last === 'tool_checked') &&
		callsTheCodeReader(entries.map((entry) => entry.event))
	) {
		return 'Reading the code';
	}
	return (last && STAGES[last]) || 'Working on it';
}

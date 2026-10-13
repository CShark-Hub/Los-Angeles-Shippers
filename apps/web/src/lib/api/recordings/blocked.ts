import type { Fact } from '../types';
import type { Recording } from './recording';

const ID = 'q_01JB2M8A1B2C3D4E5F6G7H8J9K';

function blockedFact(
	id: string,
	kind: Fact['kind'],
	module: string,
	title: string,
	tested: Fact['tested'],
	ref: string
): Fact {
	return {
		id,
		kind,
		module,
		title,
		status: 'blocked',
		tested,
		confidence: 0.88,
		evidence: [{ type: 'issue', ref, state: 'open', at: '2026-09-28' }],
		people: [],
		title_withheld: false,
		updated_at: '2026-09-28T11:05:00Z',
		waiting_on: 'unknown'
	};
}

const tokenRefresh = blockedFact(
	'f_wxhuz68kgg',
	'fix',
	'Auth',
	'Retry failed token refresh',
	'missing',
	'#407'
);
const statementExport = blockedFact(
	'f_4cidsczy9t',
	'fix',
	'Export',
	'Statement export times out for large accounts',
	'unknown',
	'#411'
);
const singleSignOn = blockedFact(
	'f_h7ffkzdrkj',
	'feature',
	'Auth',
	'Single sign-on for the partner portal',
	'missing',
	'#405'
);

/**
 * A blocked question answered the way a live composer writes it: a bold first line and the items
 * as a Markdown list on one line, one sentence left out by a check and the core's note after it.
 */
export const blocked: Recording = {
	sample: 'An answer as a live model writes it',
	question: { text: 'What is blocked right now, and what is it waiting on?', project: 'delta' },
	id: ID,
	events: [
		{ after_ms: 0, event: 'accepted', data: { id: ID } },
		{ after_ms: 120, event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
		{
			after_ms: 600,
			event: 'classified',
			data: {
				class: 'blocked',
				p: 1,
				checks: [
					{ id: 'asks_for_code', source: 'jev', p: 0.03, verdict: 'pass' },
					{ id: 'contains_agent_instruction', source: 'jev', p: 0.08, verdict: 'pass' }
				]
			}
		},
		{ after_ms: 80, event: 'plan', data: { calls: [{ tool: 'blocked', args: {} }] } },
		{
			after_ms: 700,
			event: 'tool_checked',
			data: {
				call: 0,
				checks: [
					{ id: 'args_match_question', source: 'jev', p: 0.72, verdict: 'pass' },
					{ id: 'args_free_of_instructions', source: 'jev', p: 0.56, verdict: 'pass' }
				]
			}
		},
		{
			after_ms: 200,
			event: 'tool_result',
			data: { call: 0, items: 3, withheld_fields: [], access: 'code' }
		},
		{ after_ms: 50, event: 'composing', data: {} },
		{ after_ms: 2600, event: 'judged', data: { sentences: 2, stripped: 1, held: 0 } },
		{
			after_ms: 40,
			event: 'answer',
			data: {
				text: '**Three items are currently blocked, but the blocker details are unknown for all of them.**- **Auth – Retry failed token refresh**: blocked, waiting on unknown - **Export – Statement export times out for large accounts**: blocked, waiting on unknown - **Auth – Single sign-on for the partner portal**: blocked, tests missing Some details were withheld.',
				facts: [tokenRefresh, statementExport, singleSignOn],
				confidence: 0.88
			}
		},
		{
			after_ms: 20,
			event: 'done',
			data: {
				duration_ms: 5400,
				cost_usd: 0.0039,
				judge_mode: 'jev',
				composer: 'model',
				audit_id: 87
			}
		}
	]
};

import { exportBlocked, invoiceList } from './facts';
import type { Recording } from './recording';

const ID = 'q_01JB2M9B2C3D4E5F6G7H8J9K0L';

const QUESTION = 'How long would an export to Excel take?';

/**
 * An open question: one that is none of the five kinds, answered from the facts and from the
 * project's code reader. The reader is the slow call, so a long pause follows the last check, which
 * is where the loader says it is reading the code. Its figure is worded as an estimate.
 */
export const open: Recording = {
	sample: 'An estimate from the code',
	question: { text: QUESTION, project: 'delta' },
	id: ID,
	events: [
		{ after_ms: 0, event: 'accepted', data: { id: ID } },
		{ after_ms: 120, event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
		{
			after_ms: 520,
			event: 'classified',
			data: {
				class: 'open',
				p: 0.88,
				checks: [
					{ id: 'asks_for_code', source: 'jev', p: 0.04, verdict: 'pass' },
					{ id: 'contains_agent_instruction', source: 'jev', p: 0.02, verdict: 'pass' },
					{ id: 'asks_for_confidential', source: 'jev', p: 0.03, verdict: 'pass' }
				]
			}
		},
		{
			after_ms: 90,
			event: 'plan',
			data: {
				calls: [
					{ tool: 'progress', args: { since: '2026-09-28', limit: 20 } },
					{ tool: 'ask_code', args: { question: QUESTION } }
				]
			}
		},
		{
			after_ms: 640,
			event: 'tool_checked',
			data: {
				call: 0,
				checks: [
					{ id: 'scope', source: 'rule', verdict: 'pass' },
					{ id: 'deny_class', source: 'rule', verdict: 'pass' },
					{ id: 'args_match_question', source: 'jev', p: 0.91, verdict: 'pass' },
					{ id: 'args_free_of_instructions', source: 'jev', p: 0.88, verdict: 'pass' }
				]
			}
		},
		{
			after_ms: 660,
			event: 'tool_checked',
			data: {
				call: 1,
				checks: [
					{ id: 'scope', source: 'rule', verdict: 'pass' },
					{ id: 'deny_class', source: 'rule', verdict: 'pass' },
					{ id: 'args_match_question', source: 'jev', p: 0.86, verdict: 'pass' },
					{ id: 'args_free_of_instructions', source: 'jev', p: 0.79, verdict: 'pass' }
				]
			}
		},
		// the calls run at once after the last check: the code reader keeps the answer waiting
		{
			after_ms: 8000,
			event: 'tool_result',
			data: { call: 0, items: 3, withheld_fields: [], access: 'code', p_min: 0.87 }
		},
		{
			after_ms: 30,
			event: 'tool_result',
			data: { call: 1, items: 4, withheld_fields: [], access: 'code_model', p_min: 0.82 }
		},
		{ after_ms: 50, event: 'composing', data: {} },
		{
			after_ms: 3100,
			event: 'judged',
			data: { sentences: 3, stripped: 0, held: 0, p_min: 0.84 }
		},
		{
			after_ms: 40,
			event: 'answer',
			data: {
				text: 'An export to Excel looks like a few days of work. That is an estimate from the repository, not a promise. The export module already writes the invoice data in one format, so an Excel file would reuse it and add a new writer and its tests. The bank export format is still waiting on a decision, which could add time.',
				facts: [exportBlocked, invoiceList],
				confidence: 0.72
			}
		},
		{
			after_ms: 20,
			event: 'done',
			data: {
				duration_ms: 13300,
				cost_usd: 0.0842,
				judge_mode: 'jev',
				composer: 'model',
				audit_id: 2201
			}
		}
	]
};

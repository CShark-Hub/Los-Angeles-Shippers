import { invoiceList, roundingFix } from './facts';
import type { Recording } from './recording';

const ID = 'q_01JB2M3N4P5Q6R7S8T9V0W1X2Y';

/** A progress question answered from two facts. */
export const progress: Recording = {
	sample: 'An answer with its facts',
	question: { text: 'What was finished in Delta this week?', project: 'delta' },
	id: ID,
	events: [
		{ after_ms: 0, event: 'accepted', data: { id: ID } },
		{
			after_ms: 120,
			event: 'project',
			data: { project: 'delta', p: 0.99, source: 'explicit' }
		},
		{
			after_ms: 450,
			event: 'classified',
			data: {
				class: 'progress',
				p: 0.94,
				checks: [
					{ id: 'asks_for_code', source: 'jev', p: 0.02, verdict: 'pass' },
					{ id: 'contains_agent_instruction', source: 'jev', p: 0.01, verdict: 'pass' }
				]
			}
		},
		{
			after_ms: 80,
			event: 'plan',
			data: { calls: [{ tool: 'progress', args: { since: '2026-09-28', limit: 20 } }] }
		},
		{
			after_ms: 60,
			event: 'tool_checked',
			data: {
				call: 0,
				checks: [{ id: 'arguments_valid', source: 'rule', verdict: 'pass' }]
			}
		},
		{
			after_ms: 180,
			event: 'tool_result',
			data: { call: 0, items: 2, withheld_fields: [], access: 'metadata' }
		},
		{ after_ms: 50, event: 'composing', data: {} },
		{
			after_ms: 900,
			event: 'judged',
			data: { sentences: 2, stripped: 0, held: 0 }
		},
		{
			after_ms: 40,
			event: 'answer',
			data: {
				text: 'Two items were finished in Delta this week: the invoice list with filters, and a fix for rounding in totals. Both pass their tests.',
				facts: [invoiceList, roundingFix],
				confidence: 0.91
			}
		},
		{
			after_ms: 20,
			event: 'done',
			data: {
				duration_ms: 1900,
				cost_usd: 0.004,
				judge_mode: 'jev',
				composer: 'model',
				audit_id: 1834
			}
		}
	]
};

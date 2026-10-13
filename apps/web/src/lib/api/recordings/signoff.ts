import { exportBlocked } from './facts';
import type { Recording } from './recording';

const ID = 'q_01JB2M6F9G0H1J2K3L4M5N6P7Q';

/** A question whose answer an approver reads first; the mock releases it a few seconds later. */
export const signoff: Recording = {
	sample: 'An answer held for sign-off',
	question: { text: 'What is blocked in Delta?', project: 'delta' },
	id: ID,
	events: [
		{ after_ms: 0, event: 'accepted', data: { id: ID } },
		{
			after_ms: 100,
			event: 'project',
			data: { project: 'delta', p: 0.99, source: 'explicit' }
		},
		{
			after_ms: 420,
			event: 'classified',
			data: {
				class: 'blocked',
				p: 0.92,
				checks: [{ id: 'asks_for_code', source: 'jev', p: 0.01, verdict: 'pass' }]
			}
		},
		{
			after_ms: 80,
			event: 'plan',
			data: { calls: [{ tool: 'blocked', args: {} }] }
		},
		{
			after_ms: 60,
			event: 'tool_checked',
			data: { call: 0, checks: [{ id: 'arguments_valid', source: 'rule', verdict: 'pass' }] }
		},
		{
			after_ms: 200,
			event: 'tool_result',
			data: { call: 0, items: 1, withheld_fields: [], access: 'metadata' }
		},
		{ after_ms: 50, event: 'composing', data: {} },
		{
			after_ms: 800,
			event: 'judged',
			data: { sentences: 2, stripped: 0, held: 1 }
		},
		{
			after_ms: 30,
			event: 'pending_signoff',
			data: { approval_id: 'ap_01JB2M6F9G0H1J2K3L4M5N6P7Q', reason: 'review_band' }
		}
	],
	approved: {
		after_ms: 8000,
		answer: {
			text: 'One item is blocked in Delta: the SEPA export format waits for a decision.',
			facts: [exportBlocked],
			confidence: 0.8
		},
		done: {
			duration_ms: 8600,
			cost_usd: 0.005,
			judge_mode: 'jev',
			composer: 'model',
			audit_id: 1851
		}
	}
};

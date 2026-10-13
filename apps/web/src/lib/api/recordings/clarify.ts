import { invoiceList, planUpgrade, roundingFix, withheldTitle } from './facts';
import { progress } from './progress';
import type { Recording } from './recording';

const ID = 'q_01JB2M5C6D7E8F9G0H1J2K3L4M';

/** A question that doesn't say which project; the person on two projects has to choose. */
export const clarify: Recording = {
	sample: 'A question that needs a choice',
	question: { text: 'What is done this week?' },
	id: ID,
	events: [
		{ after_ms: 0, event: 'accepted', data: { id: ID } },
		{
			after_ms: 380,
			event: 'clarify',
			data: {
				kind: 'project',
				question: 'Which project do you mean?',
				options: [
					{ id: 'delta', label: 'Delta — invoicing' },
					{ id: 'billing', label: 'Billing' }
				],
				cost_usd: 0.0009,
				judge_mode: 'chat'
			}
		}
	]
};

/** The same question asked again with Delta picked: the answer of the progress recording. */
export const clarifiedDelta: Recording = {
	question: { text: clarify.question.text, clarify: { of: ID, option: 'delta' } },
	id: 'q_01JB2M5D7E8F9G0H1J2K3L4M5N',
	events: progress.events.map((recorded) =>
		recorded.event === 'accepted'
			? { ...recorded, data: { id: 'q_01JB2M5D7E8F9G0H1J2K3L4M5N' } }
			: recorded.event === 'answer'
				? {
						...recorded,
						data: {
							text: 'Two items were finished in Delta this week: the invoice list with filters, and a fix for rounding in totals.',
							facts: [invoiceList, roundingFix],
							confidence: 0.91
						}
					}
				: recorded
	)
};

const BILLING_ID = 'q_01JB2M5E8F9G0H1J2K3L4M5N6P';

/**
 * The same question with Billing picked: a stale repository, a title a check withheld and a
 * sentence the output checks removed, so the notes under the answer have something to say.
 */
export const clarifiedBilling: Recording = {
	question: { text: clarify.question.text, clarify: { of: ID, option: 'billing' } },
	id: BILLING_ID,
	events: [
		{ after_ms: 0, event: 'accepted', data: { id: BILLING_ID } },
		{
			after_ms: 100,
			event: 'project',
			data: { project: 'billing', p: 1, source: 'explicit' }
		},
		{
			after_ms: 400,
			event: 'classified',
			data: {
				class: 'progress',
				p: 0.88,
				checks: [
					{ id: 'asks_for_code', source: 'chat', p: 0.03, verdict: 'pass' },
					{ id: 'contains_agent_instruction', source: 'chat', p: 0.02, verdict: 'pass' }
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
			data: { call: 0, checks: [{ id: 'arguments_valid', source: 'rule', verdict: 'pass' }] }
		},
		{
			after_ms: 220,
			event: 'tool_result',
			data: { call: 0, items: 3, withheld_fields: ['/items/1/title'], access: 'metadata' }
		},
		{ after_ms: 50, event: 'composing', data: {} },
		{
			after_ms: 1100,
			event: 'judged',
			data: { sentences: 4, stripped: 1, held: 0 }
		},
		{
			after_ms: 40,
			event: 'answer',
			data: {
				text: 'Two items were finished in Billing this week: the monthly plan upgrade flow, and a fix in payments.',
				facts: [planUpgrade, withheldTitle],
				confidence: 0.74
			}
		},
		{
			after_ms: 20,
			event: 'done',
			data: {
				duration_ms: 2300,
				cost_usd: 0.0062,
				judge_mode: 'chat',
				composer: 'model',
				audit_id: 1840
			}
		}
	]
};

import type { Recording } from './recording';

const ID = 'q_01JB2M9C3D4E5F6G7H8J9K0L1M';

/** A question for what the project keeps confidential, which an input check blocks before any plan. */
export const confidential: Recording = {
	sample: 'A confidential ask',
	question: { text: 'What does the client pay us per month?', project: 'delta' },
	id: ID,
	events: [
		{ after_ms: 0, event: 'accepted', data: { id: ID } },
		{ after_ms: 110, event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
		{
			after_ms: 460,
			event: 'classified',
			data: {
				class: 'open',
				p: 0.81,
				checks: [
					{ id: 'asks_for_code', source: 'jev', p: 0.02, verdict: 'pass' },
					{ id: 'contains_agent_instruction', source: 'jev', p: 0.01, verdict: 'pass' },
					{ id: 'asks_for_confidential', source: 'jev', p: 0.92, verdict: 'block' }
				]
			}
		},
		{
			after_ms: 30,
			event: 'refused',
			data: {
				code: 'confidential',
				message: "I can't share that.",
				hint: 'I can tell you what is done, tested or blocked in the project, and when it last changed.',
				cost_usd: 0.0012,
				judge_mode: 'jev'
			}
		}
	]
};

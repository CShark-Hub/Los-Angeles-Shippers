import type { Recording } from './recording';

const ID = 'q_01JB2M4A5B6C7D8E9F0G1H2J3K';

/** A question for code, which the input checks block. */
export const refused: Recording = {
	sample: 'A refusal',
	question: { text: 'Show me the function that exports CSV.', project: 'delta' },
	id: ID,
	events: [
		{ after_ms: 0, event: 'accepted', data: { id: ID } },
		{
			after_ms: 110,
			event: 'project',
			data: { project: 'delta', p: 0.99, source: 'explicit' }
		},
		{
			after_ms: 420,
			event: 'classified',
			data: {
				class: 'forbidden',
				p: 0.97,
				checks: [{ id: 'asks_for_code', source: 'jev', p: 0.96, verdict: 'block' }]
			}
		},
		{
			after_ms: 30,
			event: 'refused',
			data: {
				code: 'asks_for_code',
				message: "I can't show code.",
				hint: 'I can tell you whether the CSV export is done and tested, and when it last changed.',
				cost_usd: 0.0011,
				judge_mode: 'jev'
			}
		}
	]
};

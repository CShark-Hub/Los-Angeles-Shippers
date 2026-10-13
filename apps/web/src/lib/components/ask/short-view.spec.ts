import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';

import type { StreamEvent } from '$lib/api';
import type { Entry } from '$lib/ask/conversation.svelte';

import ShortView from './ShortView.svelte';

/** The short view as the server renders it: the words a person reads, without markup. */
function said(entries: Entry[]): string {
	const { body } = render(ShortView, { props: { entries } });
	return body
		.replace(/<!--[\s\S]*?-->/g, '')
		.replace(/<[^>]+>/g, ' ')
		.replace(/&amp;/g, '&')
		.replace(/\s+/g, ' ')
		.trim();
}

function entries(...events: Omit<StreamEvent, 'id'>[]): Entry[] {
	return events.map((event, index) => ({
		event: { ...event, id: String(index + 1) } as StreamEvent,
		at: null
	}));
}

const SENTINEL = 'SENTINEL-TEXT-OF-AN-EVENT';

const PASSING = [{ id: 'asks_for_code', source: 'jev', p: 0.02, verdict: 'pass' }] as const;

describe('the short view', () => {
	it("says each stage's verdict and lowest p, and no text of any event", () => {
		const text = said(
			entries(
				{ event: 'accepted', data: { id: 'q_1' } },
				{ event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
				{ event: 'classified', data: { class: 'progress', p: 0.94, checks: [...PASSING] } },
				{
					event: 'plan',
					data: { calls: [{ tool: 'progress', args: { since: SENTINEL } }] }
				},
				{
					event: 'tool_checked',
					data: {
						call: 0,
						checks: [{ id: 'args_match_question', source: 'jev', p: 0.72, verdict: 'pass' }]
					}
				},
				{
					event: 'tool_result',
					data: { call: 0, items: 3, withheld_fields: ['/items/1/title'], access: 'metadata' }
				},
				{ event: 'composing', data: {} },
				{ event: 'judged', data: { sentences: 5, stripped: 1, held: 0 } },
				{
					event: 'answer',
					data: { text: `The export is ready. ${SENTINEL}`, facts: [], confidence: 0.9 }
				},
				{
					event: 'done',
					data: {
						duration_ms: 1900,
						cost_usd: 0.004,
						judge_mode: 'jev',
						composer: 'model',
						audit_id: 1
					}
				}
			)
		);
		expect(text).toContain('How Mercury checked this');
		expect(text).toContain('The question pass p 0.94');
		expect(text).toContain('The lookup pass p 0.72 progress');
		expect(text).toContain('The data needs review 3 items · 1 withheld');
		expect(text).toContain('The answer needs review 4 of 5 sentences kept');
		// what the answer said, what an argument held, a withheld pointer: none of it
		expect(text).not.toContain(SENTINEL);
		expect(text).not.toContain('The export is ready');
		expect(text).not.toContain('/items');
	});

	it("says which stages a refusal didn't reach", () => {
		const text = said(
			entries(
				{ event: 'accepted', data: { id: 'q_1' } },
				{ event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
				{
					event: 'classified',
					data: {
						class: 'forbidden',
						p: 0.97,
						checks: [{ id: 'asks_for_code', source: 'jev', p: 0.96, verdict: 'block' }]
					}
				},
				{
					event: 'refused',
					data: {
						code: 'asks_for_code',
						message: `No. ${SENTINEL}`,
						hint: SENTINEL,
						cost_usd: 0,
						judge_mode: 'jev'
					}
				}
			)
		);
		expect(text).toContain('The question blocked p 0.97');
		expect(text).toContain('The lookup not reached');
		expect(text).toContain('The data not reached');
		expect(text).toContain('The answer not reached');
		expect(text).not.toContain(SENTINEL);
	});

	it('holds nothing to operate: no button, no link, no disclosure', () => {
		const { body } = render(ShortView, { props: { entries: [] } });
		expect(body).not.toMatch(/<(button|a|details|summary|input|select)\b/);
	});
});

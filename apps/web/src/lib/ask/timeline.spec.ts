import { describe, expect, it } from 'vitest';

import type { StreamEvent } from '$lib/api';
import {
	checkLabel,
	describeEvent,
	seesFullTrail,
	stagesInOneLine,
	summarizeStages
} from './timeline';

/** The event of that name, as the core sends it, with an id. */
type Of<N extends StreamEvent['event']> = Extract<StreamEvent, { event: N }>;
const at = <N extends StreamEvent['event']>(event: Omit<Of<N>, 'id'>): StreamEvent =>
	({ ...event, id: '1' }) as StreamEvent;

describe('describeEvent', () => {
	it('names the project and who chose it', () => {
		const explicit = describeEvent(
			at({ event: 'project', data: { project: 'delta', p: 1, source: 'explicit' } })
		);
		expect(explicit).toMatchObject({ title: 'Project', detail: 'delta', p: 1 });
		expect(explicit.source).toBe('you chose it');
		const judged = describeEvent(
			at({ event: 'project', data: { project: 'billing', p: 0.82, source: 'judge' } })
		);
		expect(judged.source).toBe('picked by the judge');
	});

	it('shows each check with what decided it, its probability and its verdict', () => {
		const row = describeEvent(
			at({
				event: 'classified',
				data: {
					class: 'progress',
					p: 0.94,
					checks: [
						{ id: 'asks_for_code', source: 'jev', p: 0.02, verdict: 'pass' },
						{ id: 'contains_agent_instruction', source: 'rule', verdict: 'review' }
					]
				}
			})
		);
		expect(row).toMatchObject({ title: 'Kind of question', detail: 'what is done', p: 0.94 });
		expect(row.checks).toEqual([
			{ label: 'asks for code', source: 'jev', p: 0.02, verdict: 'pass' },
			{ label: 'contains agent instruction', source: 'rule', p: null, verdict: 'review' }
		]);
		expect(row.tone).toBe('warn');
	});

	it('marks a blocking check, and says when the chat model judged', () => {
		const row = describeEvent(
			at({
				event: 'classified',
				data: {
					class: 'forbidden',
					p: 0.97,
					checks: [{ id: 'asks_for_code', source: 'chat', p: 0.96, verdict: 'block' }]
				}
			})
		);
		expect(row.tone).toBe('bad');
		expect(row.checks[0]).toMatchObject({ source: 'chat', verdict: 'block' });
	});

	it('lists the planned calls with their arguments, leaving out what is empty', () => {
		const row = describeEvent(
			at({
				event: 'plan',
				data: {
					calls: [
						{ tool: 'progress', args: { since: '2026-09-28', limit: 20, module: null } },
						{ tool: 'health', args: {} }
					]
				}
			})
		);
		expect(row.detail).toBe('progress (since 2026-09-28, limit 20) · health');
	});

	it('counts what the repository returned and what a check withheld, never showing it', () => {
		const row = describeEvent(
			at({
				event: 'tool_result',
				data: { call: 0, items: 3, withheld_fields: ['/items/1/title'], access: 'metadata' }
			})
		);
		expect(row.title).toBe('Repository answered call 1');
		expect(row.detail).toBe('3 items · 1 detail withheld · read as metadata only');
		expect(row.detail).not.toContain('/items');
		expect(row.tone).toBe('warn');
	});

	it('reports what the output checks removed and held', () => {
		expect(
			describeEvent(at({ event: 'judged', data: { sentences: 4, stripped: 1, held: 0 } }))
		).toMatchObject({ detail: '4 sentences checked · 1 removed', tone: 'warn' });
		expect(
			describeEvent(at({ event: 'judged', data: { sentences: 1, stripped: 0, held: 0 } }))
		).toMatchObject({ detail: '1 sentence checked', tone: 'good' });
	});

	it('leaves the answer text out of its row', () => {
		const row = describeEvent(
			at({
				event: 'answer',
				data: { text: 'Two items were finished in Delta.', facts: [], confidence: 0.9 }
			})
		);
		expect(JSON.stringify(row)).not.toContain('Two items');
		expect(row.title).toBe('Answer ready');
	});

	it('names a refusal, a clarify and a failure by their code, not by their text, with what each cost', () => {
		const refused = describeEvent(
			at({
				event: 'refused',
				data: {
					code: 'asks_for_code',
					message: "I can't show code.",
					hint: 'Ask about tests.',
					cost_usd: 0.001,
					judge_mode: 'jev'
				}
			})
		);
		expect(refused).toMatchObject({
			title: 'Refused',
			detail: 'asks for code · cost 0.1¢ · judge: Jev',
			tone: 'bad'
		});
		expect(JSON.stringify(refused)).not.toContain("can't show");

		expect(
			describeEvent(
				at({
					event: 'clarify',
					data: {
						kind: 'project',
						question: 'Which project?',
						options: [],
						cost_usd: 0,
						judge_mode: null
					}
				})
			)
		).toMatchObject({
			title: 'Needs your choice',
			detail: 'which project · cost 0¢ · no judge asked'
		});
		expect(
			describeEvent(
				at({
					event: 'error',
					data: { code: 'budget_exceeded', message: 'x', cost_usd: 0.25, judge_mode: 'chat' }
				})
			)
		).toMatchObject({
			title: 'Failed',
			detail: 'budget exceeded · cost 25¢ · judge: chat (fallback)'
		});
	});

	it('says which judge answered, who wrote the answer and what it cost', () => {
		const row = describeEvent(
			at({
				event: 'done',
				data: {
					duration_ms: 6420,
					cost_usd: 0.004,
					judge_mode: 'jev',
					composer: 'model',
					audit_id: 12
				}
			})
		);
		expect(row).toMatchObject({
			title: 'Done',
			detail: 'judge: Jev · composer: model · cost 0.4¢',
			tone: 'good'
		});
	});

	it('shows the degraded mode on the last row: the rules judged and the template wrote', () => {
		const row = describeEvent(
			at({
				event: 'done',
				data: {
					duration_ms: 180,
					cost_usd: 0,
					judge_mode: 'rules',
					composer: 'template',
					audit_id: 41
				}
			})
		);
		expect(row.detail).toBe('judge: rules · composer: template · cost 0¢');
		expect(row.tone).toBe('warn');
		const chat = describeEvent(
			at({
				event: 'done',
				data: {
					duration_ms: 900,
					cost_usd: 0.002,
					judge_mode: 'chat',
					composer: 'model',
					audit_id: 7
				}
			})
		);
		expect(chat.detail).toContain('judge: chat (fallback)');
		expect(chat.tone).toBe('warn');
	});

	it('never shows the question the code reader is asked', () => {
		const row = describeEvent(
			at({
				event: 'plan',
				data: {
					calls: [
						{ tool: 'progress', args: {} },
						{ tool: 'ask_code', args: { question: 'THE-WORDS-OF-THE-QUESTION' } }
					]
				}
			})
		);
		expect(row.detail).toBe('progress · ask_code (your question)');
		expect(JSON.stringify(row)).not.toContain('THE-WORDS-OF-THE-QUESTION');
	});

	it('shows the lowest p of the data and the answer, and none where rules decided', () => {
		const data: Of<'tool_result'>['data'] = {
			call: 0,
			items: 3,
			withheld_fields: [],
			access: 'code_model'
		};
		expect(describeEvent(at({ event: 'tool_result', data: { ...data, p_min: 0.82 } })).p).toBe(
			0.82
		);
		expect(describeEvent(at({ event: 'tool_result', data })).p).toBeNull();
		const judged: Of<'judged'>['data'] = { sentences: 3, stripped: 0, held: 0 };
		expect(describeEvent(at({ event: 'judged', data: { ...judged, p_min: 0.84 } })).p).toBe(0.84);
		expect(describeEvent(at({ event: 'judged', data: judged })).p).toBeNull();
	});

	it('says why an answer waits for an approver', () => {
		const row = describeEvent(
			at({ event: 'pending_signoff', data: { approval_id: 'ap_1', reason: 'review_band' } })
		);
		expect(row).toMatchObject({ title: 'Held for sign-off', tone: 'warn' });
		expect(row.detail).toContain('not sure');
	});
});

describe('checkLabel', () => {
	it('turns an id into words', () => {
		expect(checkLabel('sentence_evaluates_people')).toBe('sentence evaluates people');
	});
});

/** The events of a question, in the order the core sends them, with an id each. */
function stream(...events: Omit<StreamEvent, 'id'>[]): StreamEvent[] {
	return events.map((event, index) => ({ ...event, id: String(index + 1) }) as StreamEvent);
}

type Judged = Extract<StreamEvent, { event: 'judged' }>['data'];

const SENTINEL = 'SENTINEL-TEXT-OF-AN-EVENT';

const PASSING = [
	{ id: 'asks_for_code', source: 'jev', p: 0.02, verdict: 'pass' },
	{ id: 'contains_agent_instruction', source: 'jev', p: 0.01, verdict: 'pass' }
] as const;

const LOOKED_UP = [
	{ id: 'args_match_question', source: 'jev', p: 0.91, verdict: 'pass' },
	{ id: 'args_free_of_instructions', source: 'jev', p: 0.74, verdict: 'pass' },
	{ id: 'scope', source: 'rule', verdict: 'pass' }
] as const;

/** A question that went through every stage and was answered, with a stage's figures as given. */
function answered(change: { judged?: Partial<Judged>; calls?: number; items?: number } = {}) {
	const calls = Array.from({ length: change.calls ?? 1 }, (_, i) => ({
		tool: i === 0 ? ('progress' as const) : ('tested' as const),
		args: {}
	}));
	return stream(
		{ event: 'accepted', data: { id: 'q_1' } },
		{ event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
		{ event: 'classified', data: { class: 'progress', p: 0.94, checks: [...PASSING] } },
		{ event: 'plan', data: { calls } },
		...calls.map((_, call) => ({
			event: 'tool_checked' as const,
			data: { call, checks: [...LOOKED_UP] }
		})),
		...calls.map((_, call) => ({
			event: 'tool_result' as const,
			data: { call, items: change.items ?? 2, withheld_fields: [], access: 'metadata' as const }
		})),
		{ event: 'composing', data: {} },
		{ event: 'judged', data: { sentences: 5, stripped: 0, held: 0, ...change.judged } },
		{ event: 'answer', data: { text: 'Two items were finished.', facts: [], confidence: 0.9 } },
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
	);
}

describe('seesFullTrail', () => {
	it('only the tech lead and the admin see the full trail', () => {
		expect(seesFullTrail('tech_lead')).toBe(true);
		expect(seesFullTrail('admin')).toBe(true);
		// the smaller view is the default: any other role, or none known
		expect(seesFullTrail('pm')).toBe(false);
		expect(seesFullTrail('board')).toBe(false);
		expect(seesFullTrail(undefined)).toBe(false);
	});
});

describe('summarizeStages', () => {
	it('gives four rows for an answered question, each with its verdict and lowest p', () => {
		const rows = summarizeStages(answered());
		expect(rows.map((row) => row.stage)).toEqual(['question', 'lookup', 'data', 'answer']);
		expect(rows.every((row) => row.reached && row.verdict === 'pass')).toBe(true);
		// the lowest of the project's and the kind's probability; the lookup's two legit checks
		expect(rows[0].p).toBe(0.94);
		expect(rows[1].p).toBe(0.74);
	});

	it('marks the stage that refused, and leaves the rest not reached', () => {
		const lookup = summarizeStages(
			stream(
				{ event: 'accepted', data: { id: 'q_1' } },
				{ event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
				{ event: 'classified', data: { class: 'progress', p: 0.94, checks: [...PASSING] } },
				{ event: 'plan', data: { calls: [{ tool: 'progress', args: {} }] } },
				{ event: 'tool_checked', data: { call: 0, checks: [...LOOKED_UP] } },
				{
					event: 'refused',
					data: { code: 'strict_mode', message: 'm', hint: 'h', cost_usd: 0, judge_mode: 'jev' }
				}
			)
		);
		expect(lookup.map((row) => [row.reached, row.verdict])).toEqual([
			[true, 'pass'],
			[true, 'block'],
			[false, null],
			[false, null]
		]);

		const question = summarizeStages(
			stream(
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
					data: { code: 'asks_for_code', message: 'm', hint: 'h', cost_usd: 0, judge_mode: 'jev' }
				}
			)
		);
		expect(question.map((row) => [row.reached, row.verdict])).toEqual([
			[true, 'block'],
			[false, null],
			[false, null],
			[false, null]
		]);
	});

	it('blocks the question row when the policy refuses a clean question', () => {
		const rows = summarizeStages(
			stream(
				{ event: 'accepted', data: { id: 'q_1' } },
				{ event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
				{ event: 'classified', data: { class: 'health', p: 0.9, checks: [...PASSING] } },
				{
					event: 'refused',
					data: { code: 'forbidden_class', message: 'm', hint: 'h', cost_usd: 0, judge_mode: 'jev' }
				}
			)
		);
		expect(rows[0]).toMatchObject({ reached: true, verdict: 'block' });
		expect(rows[1].reached).toBe(false);
	});

	it("says a clarify needs the person's choice, in fixed words, and reaches no further", () => {
		const rows = summarizeStages(
			stream(
				{ event: 'accepted', data: { id: 'q_1' } },
				{ event: 'project', data: { project: 'delta', p: 0.55, source: 'judge' } },
				{
					event: 'clarify',
					data: {
						kind: 'project',
						question: `Which project? ${SENTINEL}`,
						options: [{ id: 'delta', label: `Delta ${SENTINEL}` }],
						cost_usd: 0,
						judge_mode: 'jev'
					}
				}
			)
		);
		expect(rows[0]).toMatchObject({
			reached: true,
			verdict: 'review',
			detail: 'needs your choice'
		});
		expect(rows.slice(1).every((row) => !row.reached && row.verdict === null)).toBe(true);
		expect(JSON.stringify(rows)).not.toContain(SENTINEL);
	});

	it('counts the items, the withheld values and the calls that answered', () => {
		const events = answered({ calls: 3, items: 4 });
		// a failed call leaves its part out: only two of the three came back
		const lost = events.filter((e) => !(e.event === 'tool_result' && e.data.call === 2));
		expect(summarizeStages(lost)[2]).toMatchObject({
			verdict: 'pass',
			detail: '8 items · 2 of 3 calls answered'
		});

		const withheld = stream(
			{ event: 'plan', data: { calls: [{ tool: 'progress', args: {} }] } },
			{
				event: 'tool_result',
				data: { call: 0, items: 12, withheld_fields: ['/items/1/title'], access: 'metadata' }
			}
		);
		expect(summarizeStages(withheld)[2]).toMatchObject({
			verdict: 'review',
			detail: '12 items · 1 withheld'
		});
	});

	it("names the lookup's calls in words, and says when there was nothing to look up", () => {
		expect(summarizeStages(answered({ calls: 2 }))[1].detail).toBe('progress · tests');
		const none = summarizeStages(stream({ event: 'plan', data: { calls: [] } }));
		expect(none[1]).toMatchObject({ reached: true, verdict: 'pass', detail: 'nothing to look up' });
	});

	it('names the code reader among the calls, and takes the lowest p of the data', () => {
		const rows = summarizeStages(
			stream(
				{
					event: 'plan',
					data: {
						calls: [
							{ tool: 'progress', args: {} },
							{ tool: 'ask_code', args: { question: SENTINEL } }
						]
					}
				},
				{
					event: 'tool_result',
					data: { call: 0, items: 3, withheld_fields: [], access: 'code', p_min: 0.87 }
				},
				{
					event: 'tool_result',
					data: { call: 1, items: 4, withheld_fields: [], access: 'code_model', p_min: 0.82 }
				},
				{ event: 'judged', data: { sentences: 3, stripped: 0, held: 0, p_min: 0.84 } }
			)
		);
		expect(rows[1].detail).toBe('progress · the code reader');
		expect(rows[2]).toMatchObject({ verdict: 'pass', p: 0.82, detail: '7 items' });
		expect(rows[3]).toMatchObject({ verdict: 'pass', p: 0.84 });
		expect(JSON.stringify(rows)).not.toContain(SENTINEL);
	});

	it('says the sentences kept of all and a hold for sign-off', () => {
		expect(summarizeStages(answered())[3]).toMatchObject({
			verdict: 'pass',
			detail: '5 of 5 sentences kept'
		});
		expect(summarizeStages(answered({ judged: { stripped: 2 } }))[3]).toMatchObject({
			verdict: 'review',
			detail: '3 of 5 sentences kept'
		});
		const held = stream(
			{ event: 'judged', data: { sentences: 3, stripped: 0, held: 1 } },
			{ event: 'pending_signoff', data: { approval_id: 'ap_1', reason: 'review_band' } }
		);
		expect(summarizeStages(held)[3]).toMatchObject({
			reached: true,
			verdict: 'review',
			detail: '3 of 3 sentences kept · held for sign-off'
		});
	});

	it("shows the answer row blocked when an approver didn't release the answer", () => {
		const rows = summarizeStages(
			stream(
				{ event: 'judged', data: { sentences: 3, stripped: 0, held: 1 } },
				{ event: 'pending_signoff', data: { approval_id: 'ap_1', reason: 'class' } },
				{
					event: 'refused',
					data: {
						code: 'rejected_by_approver',
						message: 'm',
						hint: 'h',
						cost_usd: 0,
						judge_mode: 'jev'
					}
				}
			)
		);
		expect(rows[3]).toMatchObject({ reached: true, verdict: 'block' });
	});

	it("takes the lowest of the project's and the class's probability", () => {
		const rows = summarizeStages(
			stream(
				{ event: 'project', data: { project: 'delta', p: 0.61, source: 'judge' } },
				{ event: 'classified', data: { class: 'progress', p: 0.94, checks: [...PASSING] } }
			)
		);
		expect(rows[0].p).toBe(0.61);
		// no project event (a rule chose it): the class's probability stands alone
		const alone = summarizeStages(
			stream({ event: 'classified', data: { class: 'progress', p: 0.88, checks: [...PASSING] } })
		);
		expect(alone[0].p).toBe(0.88);
	});

	it("never takes an input check's own probability as the question's: those are risks", () => {
		const rows = summarizeStages(
			stream(
				{ event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
				{ event: 'classified', data: { class: 'progress', p: 0.94, checks: [...PASSING] } }
			)
		);
		// the checks hold p 0.02 and 0.01, the chance of a problem, not of being fine
		expect(rows[0].p).toBe(0.94);
	});

	it("reads only the lookup's legit checks for its p", () => {
		const rows = summarizeStages(
			stream(
				{ event: 'plan', data: { calls: [{ tool: 'progress', args: {} }] } },
				{
					event: 'tool_checked',
					data: {
						call: 0,
						checks: [
							{ id: 'args_match_question', source: 'jev', p: 0.8, verdict: 'pass' },
							// a risk check added later must not show upside down
							{ id: 'some_risk_check', source: 'jev', p: 0.03, verdict: 'pass' },
							{ id: 'args_free_of_instructions', source: 'jev', p: 0.65, verdict: 'pass' }
						]
					}
				}
			)
		);
		expect(rows[1].p).toBe(0.65);
		const rulesOnly = summarizeStages(
			stream(
				{ event: 'plan', data: { calls: [{ tool: 'progress', args: {} }] } },
				{
					event: 'tool_checked',
					data: { call: 0, checks: [{ id: 'scope', source: 'rule', verdict: 'pass' }] }
				}
			)
		);
		expect(rulesOnly[1].p).toBeNull();
	});

	it("takes the strictest verdict of a stage's checks", () => {
		const rows = summarizeStages(
			stream(
				{
					event: 'classified',
					data: {
						class: 'progress',
						p: 0.9,
						checks: [
							{ id: 'asks_for_code', source: 'jev', p: 0.2, verdict: 'review' },
							{ id: 'contains_agent_instruction', source: 'jev', p: 0.01, verdict: 'pass' }
						]
					}
				},
				{ event: 'plan', data: { calls: [{ tool: 'progress', args: {} }] } },
				{
					event: 'tool_checked',
					data: {
						call: 0,
						checks: [
							{ id: 'args_match_question', source: 'jev', p: 0.8, verdict: 'pass' },
							{ id: 'scope', source: 'rule', verdict: 'review' }
						]
					}
				}
			)
		);
		expect(rows[0].verdict).toBe('review');
		expect(rows[1].verdict).toBe('review');
	});

	it('ends the rows where an error stopped the question', () => {
		const rows = summarizeStages(
			stream(
				{ event: 'accepted', data: { id: 'q_1' } },
				{ event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
				{ event: 'classified', data: { class: 'progress', p: 0.94, checks: [...PASSING] } },
				{
					event: 'error',
					data: { code: 'budget_exceeded', message: 'x', cost_usd: 0.3, judge_mode: 'jev' }
				}
			)
		);
		expect(rows.map((row) => row.reached)).toEqual([true, false, false, false]);
	});

	it('gives four rows not reached before anything has come', () => {
		const rows = summarizeStages(stream({ event: 'accepted', data: { id: 'q_1' } }));
		expect(rows).toHaveLength(4);
		expect(rows.every((row) => !row.reached && row.verdict === null && row.p === null)).toBe(true);
	});

	it('never reads text from an event', () => {
		const text = (what: string) => `${what} ${SENTINEL}`;
		const rows = summarizeStages(
			stream(
				{ event: 'accepted', data: { id: 'q_1' } },
				{ event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
				{ event: 'classified', data: { class: 'progress', p: 0.94, checks: [...PASSING] } },
				{
					event: 'plan',
					data: { calls: [{ tool: 'progress', args: { since: text('args') } }] }
				},
				{ event: 'tool_checked', data: { call: 0, checks: [...LOOKED_UP] } },
				{ event: 'tool_result', data: { call: 0, items: 1, withheld_fields: [], access: 'code' } },
				{ event: 'judged', data: { sentences: 1, stripped: 0, held: 0 } },
				{ event: 'answer', data: { text: text('answer'), facts: [], confidence: 0.9 } },
				{
					event: 'refused',
					data: {
						code: 'strict_mode',
						message: text('message'),
						hint: text('hint'),
						cost_usd: 0,
						judge_mode: 'jev'
					}
				}
			)
		);
		expect(JSON.stringify(rows)).not.toContain(SENTINEL);
	});
});

describe('stagesInOneLine', () => {
	/** The line over the short view and on the folded trail, for a question's events. */
	const line = (events: StreamEvent[]) => stagesInOneLine(summarizeStages(events));

	/** A refusal, as the core ends a question with it. */
	const refused = (code: Of<'refused'>['data']['code']) =>
		({
			event: 'refused',
			data: { code, message: 'm', hint: 'h', cost_usd: 0, judge_mode: 'jev' }
		}) as const;

	/** The question's own stage, asked of a project and checked. */
	const asked = (
		checks: Of<'classified'>['data']['checks'] = [...PASSING]
	): Omit<StreamEvent, 'id'>[] => [
		{ event: 'accepted', data: { id: 'q_1' } },
		{ event: 'project', data: { project: 'delta', p: 0.99, source: 'explicit' } },
		{ event: 'classified', data: { class: 'progress', p: 0.94, checks } }
	];

	/** The lookup and the data of one call, both clean. */
	const lookedUp: Omit<StreamEvent, 'id'>[] = [
		{ event: 'plan', data: { calls: [{ tool: 'progress', args: {} }] } },
		{ event: 'tool_checked', data: { call: 0, checks: [...LOOKED_UP] } },
		{ event: 'tool_result', data: { call: 0, items: 2, withheld_fields: [], access: 'metadata' } }
	];

	it('says all four stages passed when they did', () => {
		expect(line(answered())).toEqual({ tone: 'good', words: 'All four stages passed' });
	});

	it('counts the stages that need review, in the singular and the plural', () => {
		expect(line(answered({ judged: { stripped: 2 } }))).toEqual({
			tone: 'warn',
			words: '1 stage needs review'
		});
		// a value withheld from the data, and sentences removed from the answer
		const two = answered({ judged: { stripped: 1 } }).map((event) =>
			event.event === 'tool_result'
				? { ...event, data: { ...event.data, withheld_fields: ['/items/0/title'] } }
				: event
		);
		expect(line(two)).toEqual({ tone: 'warn', words: '2 stages need review' });
	});

	it('names the stage a block stopped the question at', () => {
		const block = [{ id: 'asks_for_code', source: 'jev', p: 0.96, verdict: 'block' }] as const;
		expect(line(stream(...asked([...block]), refused('asks_for_code')))).toEqual({
			tone: 'bad',
			words: 'Blocked at the question'
		});
		expect(line(stream(...asked(), ...lookedUp.slice(0, 2), refused('strict_mode')))).toEqual({
			tone: 'bad',
			words: 'Blocked at the lookup'
		});
		expect(line(stream(...asked(), ...lookedUp, refused('strict_mode')))).toEqual({
			tone: 'bad',
			words: 'Blocked at the data'
		});
		expect(
			line(
				stream(
					...asked(),
					...lookedUp,
					{ event: 'judged', data: { sentences: 3, stripped: 0, held: 1 } },
					{ event: 'pending_signoff', data: { approval_id: 'ap_1', reason: 'class' } },
					refused('rejected_by_approver')
				)
			)
		).toEqual({ tone: 'bad', words: 'Blocked at the answer' });
	});

	it('lets a block win over a review', () => {
		const unsure = [{ id: 'asks_for_code', source: 'jev', p: 0.4, verdict: 'review' }] as const;
		const events = stream(...asked([...unsure]), ...lookedUp.slice(0, 2), refused('strict_mode'));
		expect(summarizeStages(events).map((row) => row.verdict)).toEqual([
			'review',
			'block',
			null,
			null
		]);
		expect(line(events)).toEqual({ tone: 'bad', words: 'Blocked at the lookup' });
	});

	it('counts the stages passed, plainly, when the question ended before every stage', () => {
		const failed = {
			event: 'error',
			data: { code: 'budget_exceeded', message: 'x', cost_usd: 0.3, judge_mode: 'jev' }
		} as const;
		expect(line(stream(...asked(), failed))).toEqual({
			tone: 'neutral',
			words: '1 of 4 stages passed'
		});
		expect(line(stream(...asked(), ...lookedUp, failed))).toEqual({
			tone: 'neutral',
			words: '3 of 4 stages passed'
		});
	});
});

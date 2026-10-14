import { describe, expect, it } from 'vitest';

import type { StreamEvent } from '$lib/api';

import type { Entry } from './conversation.svelte';
import { stageOf } from './loader';

const SENTINEL = 'SENTINEL-TEXT-OF-AN-EVENT';

function entries(...events: Omit<StreamEvent, 'id'>[]): Entry[] {
	return events.map((event, index) => ({
		event: { ...event, id: String(index + 1) } as StreamEvent,
		at: null
	}));
}

const accepted: Omit<StreamEvent, 'id'> = { event: 'accepted', data: { id: 'q_1' } };
type Call = Extract<StreamEvent, { event: 'plan' }>['data']['calls'][number];
const planned = (...tools: ('progress' | 'ask_code')[]): Omit<StreamEvent, 'id'> => ({
	event: 'plan',
	data: {
		calls: tools.map((tool): Call =>
			tool === 'ask_code' ? { tool, args: { question: 'a question' } } : { tool, args: {} }
		)
	}
});
const checked = (call: number): Omit<StreamEvent, 'id'> => ({
	event: 'tool_checked',
	data: { call, checks: [{ id: 'scope', source: 'rule', verdict: 'pass' }] }
});
const result = (call: number): Omit<StreamEvent, 'id'> => ({
	event: 'tool_result',
	data: { call, items: 1, withheld_fields: [], access: 'metadata' }
});

describe('the loader', () => {
	it('says reading the code while the code reader’s call runs', () => {
		const running = entries(accepted, planned('progress', 'ask_code'), checked(0), checked(1));
		expect(stageOf(running, 'running')).toBe('Reading the code');
		// the plan alone, before its checks, is the code reader's too once it holds the call
		expect(stageOf(entries(accepted, planned('ask_code')), 'running')).toBe('Reading the code');
	});

	it('says reading the repository once the calls answered', () => {
		expect(stageOf(entries(accepted, planned('ask_code'), checked(0), result(0)), 'running')).toBe(
			'Reading the repository'
		);
		// a plan without the code reader is the lookup's, as before
		expect(stageOf(entries(accepted, planned('progress'), checked(0)), 'running')).toBe(
			'Checking what to look up'
		);
		expect(stageOf(entries(accepted, planned('progress')), 'running')).toBe(
			'Deciding what to look up'
		);
	});

	it('names the stage from the last event’s name, never its text', () => {
		const stages: [Omit<StreamEvent, 'id'>, string][] = [
			[accepted, 'Reading your question'],
			[
				{ event: 'project', data: { project: SENTINEL, p: 1, source: 'explicit' } },
				'Finding the project'
			],
			[
				{ event: 'classified', data: { class: 'progress', p: 0.9, checks: [] } },
				'Checking the question'
			],
			[{ event: 'composing', data: {} }, 'Writing the answer'],
			[
				{ event: 'judged', data: { sentences: 1, stripped: 0, held: 0 } },
				'Checking every sentence'
			],
			[{ event: 'answer', data: { text: SENTINEL, facts: [], confidence: 0.9 } }, 'Almost there']
		];
		for (const [event, words] of stages) {
			const said = stageOf(entries(event), 'running');
			expect(said).toBe(words);
			expect(said).not.toContain(SENTINEL);
		}
	});

	it('says sending and finding while the question has no events', () => {
		expect(stageOf([], 'sending')).toBe('Sending your question');
		expect(stageOf([], 'loading')).toBe('Finding your question');
		expect(stageOf([], 'running')).toBe('Working on it');
	});
});

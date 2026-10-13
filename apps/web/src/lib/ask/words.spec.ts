import { describe, expect, it } from 'vitest';

import {
	accessWords,
	classWords,
	composerWords,
	evidenceWords,
	judgeName,
	judgeWords,
	percent,
	probability,
	probeNotice,
	signoffWords,
	statusChip,
	testedChip,
	toolWords,
	waitingWords
} from './words';

describe('words for what the core sends as codes', () => {
	it('names a class as part of a sentence, and keeps a class it does not know', () => {
		expect(classWords('progress')).toBe('what is done');
		expect(classWords('open')).toBe('anything else about the project');
		expect(classWords('forbidden')).toBe('something Mercury does not answer');
	});

	it('colours what a fact says in the order of how much it needs a look', () => {
		expect(statusChip('done')).toEqual({ label: 'Done', tone: 'good' });
		expect(statusChip('blocked').tone).toBe('bad');
		expect(statusChip('unclear').tone).toBe('warn');
		expect(testedChip('passing')).toEqual({ label: 'Tests passing', tone: 'good' });
		expect(testedChip('failing').tone).toBe('bad');
		expect(testedChip('missing').label).toBe('No tests');
		expect(waitingWords('decision')).toBe('Waiting for a decision');
	});

	it('writes numbers the way the screen shows them', () => {
		expect(percent(0.914)).toBe('91%');
		expect(probability(0.9)).toBe('0.90');
	});

	it('writes evidence short, with the reference only when there is one', () => {
		expect(evidenceWords({ type: 'pr', ref: '#412', state: 'merged', at: '2026-10-01' })).toBe(
			'PR #412 merged'
		);
		expect(evidenceWords({ type: 'ci', ref: null, state: 'passed', at: '2026-10-01' })).toBe(
			'CI passed'
		);
	});

	it('says who judged, so a degraded judge is not mistaken for the normal one', () => {
		expect(judgeWords('jev')).toBe('Jev');
		expect(judgeWords('chat')).toContain('fallback');
		expect(judgeWords('rules')).toContain('degraded');
		expect(accessWords('code')).toContain('never shown');
	});

	it('names the judge as the timeline does, and says when the template wrote the answer', () => {
		expect((['jev', 'chat', 'rules'] as const).map(judgeName)).toEqual([
			'Jev',
			'chat (fallback)',
			'rules'
		]);
		expect(composerWords('model')).toBe('the model');
		expect(composerWords('template')).toContain('degraded');
	});

	it('says why an answer waits for an approver', () => {
		expect(signoffWords('class')).toMatch(/always read/);
		expect(signoffWords('review_band')).toMatch(/not sure/);
	});

	it('names every tool, the code reader too', () => {
		const tools = ['describe', 'progress', 'tested', 'blocked', 'health', 'changes', 'ask_code'];
		expect(tools.map(toolWords)).toEqual([
			"the project's description",
			'progress',
			'tests',
			'blockers',
			'build health',
			'changes',
			'the code reader'
		]);
		// a tool the app doesn't know yet is said plainly, never as a code
		expect(toolWords('read_notes')).toBe('read notes');
	});
});

describe('probeNotice', () => {
	it('is silent for a repository that is fresh', () => {
		expect(probeNotice({ status: 'healthy', as_of: '2026-10-05T10:00:00Z' }, 'now')).toBeNull();
	});

	it('warns when the data may be old, off or not read yet', () => {
		const when = '25 minutes ago';
		expect(probeNotice({ status: 'stale', as_of: '2026-10-05T10:00:00Z' }, when)).toBe(
			'The data may be out of date: the repository was last read 25 minutes ago.'
		);
		expect(probeNotice({ status: 'quarantined', as_of: '2026-10-05T10:00:00Z' }, when)).toMatch(
			/switched off/
		);
		expect(probeNotice(null, when)).toMatch(/has not heard/);
	});
});

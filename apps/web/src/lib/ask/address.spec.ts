import { describe, expect, it } from 'vitest';

import { questionIn } from './address';

describe('questionIn', () => {
	it('finds the question an address names', () => {
		expect(questionIn('/ask/q_01JB2M3N4P5Q6R7S8T9V0W1X2Y')).toBe('q_01JB2M3N4P5Q6R7S8T9V0W1X2Y');
		expect(questionIn('/ask/q_1/')).toBe('q_1');
	});

	it('names none for the Ask page itself, or for any other page', () => {
		expect(questionIn('/ask')).toBeNull();
		expect(questionIn('/ask/')).toBeNull();
		expect(questionIn('/settings')).toBeNull();
		expect(questionIn('/settings/q_1')).toBeNull();
		expect(questionIn('/')).toBeNull();
	});

	it('reads an encoded id, and gives up on one that is not valid', () => {
		expect(questionIn('/ask/q%5F1')).toBe('q_1');
		expect(questionIn('/ask/%E0%A4%A')).toBeNull();
	});
});

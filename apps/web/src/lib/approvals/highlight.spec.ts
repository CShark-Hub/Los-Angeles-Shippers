import { describe, expect, it } from 'vitest';
import type { Flag } from '$lib/api';
import { pieces, unplaced } from './highlight';

const flag = (sentence: string, check = 'sentence_contains_code_or_identifier', p = 0.6): Flag => ({
	sentence,
	check,
	p
});

describe('a held draft with its flagged sentences', () => {
	const text = 'Export is done. The SEPA export for the client is still a draft. Billing is next.';

	it('cuts the text at each flagged sentence and puts it back together unchanged', () => {
		const flags = [flag('The SEPA export for the client is still a draft.')];
		const cut = pieces(text, flags);
		expect(cut.map((p) => p.text)).toEqual([
			'Export is done. ',
			'The SEPA export for the client is still a draft.',
			' Billing is next.'
		]);
		expect(cut.map((p) => p.flags.length)).toEqual([0, 1, 0]);
		expect(cut.map((p) => p.text).join('')).toBe(text);
	});

	it('keeps every check of a sentence that two checks flagged on that one piece', () => {
		const sentence = 'Billing is next.';
		const flags = [flag(sentence), flag(sentence, 'sentence_mentions_client', 0.4)];
		const cut = pieces(text, flags);
		expect(cut.at(-1)).toEqual({ text: sentence, flags });
		expect(cut.map((p) => p.text).join('')).toBe(text);
	});

	it('marks flagged sentences in the order the draft has them', () => {
		const cut = pieces(text, [flag('Billing is next.'), flag('Export is done.')]);
		expect(cut.filter((p) => p.flags.length > 0).map((p) => p.text)).toEqual([
			'Export is done.',
			'Billing is next.'
		]);
	});

	it('shows a flag whose sentence the draft no longer holds beside it, never drops it', () => {
		const lost = flag('A sentence the draft does not hold.');
		expect(pieces(text, [lost])).toEqual([{ text, flags: [] }]);
		expect(unplaced(text, [lost, flag('Export is done.')])).toEqual([lost]);
	});
});

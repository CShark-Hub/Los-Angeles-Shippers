import { describe, expect, it } from 'vitest';

import {
	answerBlocks,
	answerCard,
	grouped,
	inline,
	leftOutNote,
	markedBlocks,
	WITHHELD_NOTE,
	WITHHELD_VALUES_NOTE
} from './answer-text';

describe('answer text', () => {
	it('leaves plain prose as one paragraph', () => {
		const text =
			'Two items were finished in Delta this week: the invoice list with filters, and a fix for rounding in totals. Both pass their tests.';
		expect(answerBlocks(text)).toEqual([{ kind: 'paragraph', parts: [{ text, strong: false }] }]);
	});

	it('reads bold runs and a list written on one line, as a live composer writes them', () => {
		const text =
			'**Three items are currently blocked, but the blocker details are unknown for all of them.**- **Auth – Retry failed token refresh**: blocked, waiting on unknown - **Export – Statement export times out for large accounts**: blocked, waiting on unknown - **Auth – Single sign-on for the partner portal**: blocked, tests missing';
		expect(answerBlocks(text)).toEqual([
			{
				kind: 'paragraph',
				parts: [
					{
						text: 'Three items are currently blocked, but the blocker details are unknown for all of them.',
						strong: true
					}
				]
			},
			{
				kind: 'list',
				items: [
					[
						{ text: 'Auth – Retry failed token refresh', strong: true },
						{ text: ': blocked, waiting on unknown', strong: false }
					],
					[
						{ text: 'Export – Statement export times out for large accounts', strong: true },
						{ text: ': blocked, waiting on unknown', strong: false }
					],
					[
						{ text: 'Auth – Single sign-on for the partner portal', strong: true },
						{ text: ': blocked, tests missing', strong: false }
					]
				]
			}
		]);
	});

	it('reads items on their own lines and bold inside a sentence', () => {
		expect(
			answerBlocks('In the **Auth** module:\n- a fix is blocked\n- a feature is blocked')
		).toEqual([
			{
				kind: 'paragraph',
				parts: [
					{ text: 'In the ', strong: false },
					{ text: 'Auth', strong: true },
					{ text: ' module:', strong: false }
				]
			},
			{
				kind: 'list',
				items: [
					[{ text: 'a fix is blocked', strong: false }],
					[{ text: 'a feature is blocked', strong: false }]
				]
			}
		]);
	});

	it('keeps a dash inside prose as text', () => {
		const text = 'The e-mail export - the one for statements - is done.';
		expect(answerBlocks(text)).toEqual([{ kind: 'paragraph', parts: [{ text, strong: false }] }]);
	});

	it("sets the core's note about what it left out apart", () => {
		expect(answerBlocks(`Two items are blocked. ${WITHHELD_NOTE}`)).toEqual([
			{ kind: 'paragraph', parts: [{ text: 'Two items are blocked.', strong: false }] },
			{ kind: 'note', text: WITHHELD_NOTE }
		]);
		expect(answerBlocks(WITHHELD_NOTE)).toEqual([{ kind: 'note', text: WITHHELD_NOTE }]);
	});

	it("sets the core's note apart wherever it stands, and says it once", () => {
		const unread =
			"The progress data couldn't be read from the project's probe, so it is left out.";
		// the core adds its sentences about data it couldn't read after the note
		expect(answerBlocks(`Two items are blocked. ${WITHHELD_NOTE} ${unread}`)).toEqual([
			{ kind: 'paragraph', parts: [{ text: 'Two items are blocked.', strong: false }] },
			{ kind: 'paragraph', parts: [{ text: unread, strong: false }] },
			{ kind: 'note', text: WITHHELD_NOTE }
		]);
		expect(answerBlocks(`${WITHHELD_NOTE} ${unread}`)).toEqual([
			{ kind: 'paragraph', parts: [{ text: unread, strong: false }] },
			{ kind: 'note', text: WITHHELD_NOTE }
		]);
		expect(answerBlocks(`- **Auth**: blocked\n${WITHHELD_NOTE}\n${WITHHELD_NOTE}`)).toEqual([
			{
				kind: 'list',
				items: [
					[
						{ text: 'Auth', strong: true },
						{ text: ': blocked', strong: false }
					]
				]
			},
			{ kind: 'note', text: WITHHELD_NOTE }
		]);
	});

	it("leaves the note's words inside another sentence as text", () => {
		const text = `Nothing new: ${WITHHELD_NOTE.slice(0, -1)} from the client, it says.`;
		expect(answerBlocks(text)).toEqual([{ kind: 'paragraph', parts: [{ text, strong: false }] }]);
		const glued = `Done.${WITHHELD_NOTE}`;
		expect(answerBlocks(glued)).toEqual([
			{ kind: 'paragraph', parts: [{ text: glued, strong: false }] }
		]);
	});

	it('drops a mark without its pair and keeps every word', () => {
		expect(inline('**Done** and **tested')).toEqual([
			{ text: 'Done', strong: true },
			{ text: ' and tested', strong: false }
		]);
		expect(inline('no marks')).toEqual([{ text: 'no marks', strong: false }]);
	});

	it('never turns text into markup: angle brackets stay text', () => {
		const blocks = answerBlocks('**<b>x</b>** - **<img src=x>**: done');
		expect(JSON.stringify(blocks)).toContain('<b>x</b>');
		expect(blocks.every((b) => b.kind !== 'note')).toBe(true);
	});
});

describe('an answer card', () => {
	const unread = "The progress data couldn't be read from the project's probe, so it is left out.";

	function said(card: ReturnType<typeof answerCard>): string[] {
		return [
			...card.blocks.map((block) => (block.kind === 'note' ? block.text : JSON.stringify(block))),
			...card.notes
		];
	}

	it("says the sentences left out once, with their count, wherever the core's note stood", () => {
		const card = answerCard(`Two items are blocked. ${WITHHELD_NOTE} ${unread}`, {
			withheld: 0,
			leftOut: 3
		});
		expect(card.blocks.map((block) => block.kind)).toEqual(['paragraph', 'paragraph']);
		expect(card.notes).toEqual([leftOutNote(3)]);
		expect(said(card).join(' ')).not.toContain(WITHHELD_NOTE);
	});

	it("lets the core's note speak alone when it is all the answer says", () => {
		const card = answerCard(WITHHELD_NOTE, { withheld: 2, leftOut: 2 });
		expect(card.blocks).toEqual([{ kind: 'note', text: WITHHELD_NOTE }]);
		// neither the count nor the withheld values: both would say it again
		expect(card.notes).toEqual([]);
	});

	it('says what the data checks withheld, and what was left out, for an answer that has words of its own', () => {
		const card = answerCard('Two items are blocked.', { withheld: 1, leftOut: 1 });
		expect(card.notes).toEqual([WITHHELD_VALUES_NOTE, leftOutNote(1)]);
		expect(leftOutNote(1)).toBe('1 sentence was left out because it did not pass a check.');
		expect(answerCard('Two items are blocked.', { withheld: 0, leftOut: 0 }).notes).toEqual([]);
	});

	it("keeps the core's note when nothing else says it, and adds nothing that repeats it", () => {
		const card = answerCard(`Two items are blocked. ${WITHHELD_NOTE}`, { withheld: 1, leftOut: 0 });
		expect(card.blocks.at(-1)).toEqual({ kind: 'note', text: WITHHELD_NOTE });
		expect(card.notes).toEqual([]);
	});
});

describe('a held draft with flagged sentences', () => {
	const draft =
		'**Two items are blocked.**- **Auth – Retry failed token refresh**: blocked - **Export – Statement export**: waiting on the client';
	const flagged = '**Export – Statement export**: waiting on the client';
	const at = draft.indexOf(flagged);
	const ranges = [{ start: at, end: at + flagged.length, marks: ['commits'] }];

	it('marks the flagged sentence where the Markdown puts it, bold and plain alike', () => {
		const blocks = markedBlocks(draft, ranges);
		expect(blocks[1]).toEqual({
			kind: 'list',
			items: [
				[
					{ text: 'Auth – Retry failed token refresh', strong: true },
					{ text: ': blocked', strong: false }
				],
				[
					{ text: 'Export – Statement export', strong: true, marks: ['commits'] },
					{ text: ': waiting on the client', strong: false, marks: ['commits'] }
				]
			]
		});
	});

	it('keeps one marked stretch together, so a screen draws one mark for it', () => {
		const blocks = markedBlocks(draft, ranges);
		const item = blocks[1].kind === 'list' ? blocks[1].items[1] : [];
		const groups = grouped(item);
		expect(groups).toHaveLength(1);
		expect(groups[0].marks).toEqual(['commits']);
		expect(groups[0].parts.map((p) => p.text).join('')).toBe(
			'Export – Statement export: waiting on the client'
		);
	});

	it('marks a plain sentence as one run, as before', () => {
		const text = 'One item is done. The client will have it on Friday.';
		const sentence = 'The client will have it on Friday.';
		const start = text.indexOf(sentence);
		const blocks = markedBlocks(text, [
			{ start, end: start + sentence.length, marks: ['commits'] }
		]);
		expect(blocks).toEqual([
			{
				kind: 'paragraph',
				parts: [
					{ text: 'One item is done. ', strong: false },
					{ text: sentence, strong: false, marks: ['commits'] }
				]
			}
		]);
	});
});

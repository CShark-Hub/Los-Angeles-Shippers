import { render } from 'svelte/server';
import type { Component } from 'svelte';
import { describe, expect, it } from 'vitest';

import type { Answer, Refusal } from '$lib/api';
import { leftOutNote, WITHHELD_NOTE, WITHHELD_VALUES_NOTE } from '$lib/ask/answer-text';
import type { Thread, Turn } from '$lib/ask/history.svelte';

import AnswerCard from './AnswerCard.svelte';
import ChatSidebar from './ChatSidebar.svelte';
import EarlierReply from './EarlierReply.svelte';
import RefusalNotice from './RefusalNotice.svelte';

/**
 * The Ask screen's pieces as the server renders them: the words a person reads, without markup.
 * Nothing here runs in a browser; what a piece says is decided before any script runs.
 */
function words<P extends Record<string, unknown>>(piece: Component<P>, props: P): string {
	const { body } = render(piece, { props });
	return body
		.replace(/<!--[\s\S]*?-->/g, '')
		.replace(/<[^>]+>/g, ' ')
		.replace(/&lt;/g, '<')
		.replace(/&gt;/g, '>')
		.replace(/&quot;/g, '"')
		.replace(/&#39;/g, "'")
		.replace(/&amp;/g, '&')
		.replace(/\s+/g, ' ')
		.trim();
}

function times(text: string, part: string): number {
	return text.split(part).length - 1;
}

const REJECTED: Refusal = {
	code: 'rejected_by_approver',
	message: 'An approver did not release this answer.',
	hint: 'Ask the tech lead about the export first.'
};

function turn(change: Partial<Turn>): Turn {
	return {
		id: 'q_1',
		text: 'Is the export ready for the client?',
		project: 'delta',
		phase: 'delivered',
		answer: null,
		refusal: null,
		clarify: null,
		done: null,
		notes: [],
		withheld: 0,
		stripped: 0,
		fromCode: false,
		preview: '',
		...change
	};
}

describe('an earlier question of the conversation', () => {
	it('keeps the refusal with its hint, the approver’s note, and its reason', () => {
		const said = words(EarlierReply, { turn: turn({ phase: 'refused', refusal: REJECTED }) });
		expect(said).toContain("Mercury can't answer that.");
		expect(said).toContain(REJECTED.message);
		expect(said).toContain(REJECTED.hint);
		expect(said).toContain('Reason: rejected_by_approver');
	});

	it('is not read out as news: only the question on screen is', () => {
		const { body } = render(EarlierReply, {
			props: { turn: turn({ phase: 'refused', refusal: REJECTED }) }
		});
		expect(body).not.toContain('role="status"');
		expect(body).not.toContain('data-focus-target');
		const live = render(RefusalNotice, { props: { refusal: REJECTED } }).body;
		expect(live).toContain('role="status"');
		expect(live).toContain('data-focus-target');
	});

	it('says nothing empty for a refusal without a hint', () => {
		const { body } = render(RefusalNotice, {
			props: { refusal: { ...REJECTED, hint: '' }, live: false }
		});
		expect(body).not.toMatch(/<p class="text-text-2">\s*<\/p>/);
		expect(words(RefusalNotice, { refusal: { ...REJECTED, hint: '' }, live: false })).toContain(
			'Reason: rejected_by_approver'
		);
	});

	it('offers to ask again under an approver’s rejection of the question on screen only', () => {
		const onaskagain = () => {};
		expect(words(RefusalNotice, { refusal: REJECTED, onaskagain })).toContain('Ask again');
		// not without a way to do it, not for another refusal, not for an earlier question
		expect(words(RefusalNotice, { refusal: REJECTED })).not.toContain('Ask again');
		expect(
			words(RefusalNotice, { refusal: { ...REJECTED, code: 'asks_for_code' }, onaskagain })
		).not.toContain('Ask again');
		expect(words(RefusalNotice, { refusal: REJECTED, onaskagain, live: false })).not.toContain(
			'Ask again'
		);
		expect(
			words(EarlierReply, { turn: turn({ phase: 'refused', refusal: REJECTED }) })
		).not.toContain('Ask again');
		// once chosen, the question waits in the box
		const chosen = words(RefusalNotice, { refusal: REJECTED, onaskagain, askingAgain: true });
		expect(chosen).not.toContain('Ask again');
		expect(chosen).toContain('Your question is back in the box below');
	});

	it("keeps a confidential refusal's hint and its reason", () => {
		const confidential: Refusal = {
			code: 'confidential',
			message: "I can't share that.",
			hint: 'I can tell you what is done, tested or blocked in the project.'
		};
		const said = words(RefusalNotice, { refusal: confidential });
		expect(said).toContain(confidential.message);
		expect(said).toContain(confidential.hint);
		expect(said).toContain('Reason: confidential');
		// as an earlier question of the conversation, it says the same
		const earlier = words(EarlierReply, {
			turn: turn({ phase: 'refused', refusal: confidential })
		});
		expect(earlier).toContain(confidential.hint);
		expect(earlier).toContain('Reason: confidential');
	});

	it('says an answer the retention emptied is no longer kept', () => {
		expect(words(EarlierReply, { turn: turn({ phase: 'expired' }) })).toBe(
			'The answer is no longer kept.'
		);
	});
});

describe('the answer card', () => {
	const unread = "The progress data couldn't be read from the project's probe, so it is left out.";
	const answer = (text: string): Answer => ({ text, facts: [], confidence: 0.4 });

	it("sets the core's note apart wherever it stands, and says the sentences left out once", () => {
		const said = words(AnswerCard, {
			answer: answer(`Two items are blocked. ${WITHHELD_NOTE} ${unread}`),
			done: null,
			leftOut: 2
		});
		expect(said).toContain('Two items are blocked.');
		expect(said).toContain(unread);
		expect(times(said, WITHHELD_NOTE)).toBe(0);
		expect(times(said, leftOutNote(2))).toBe(1);
	});

	it("says the core's note alone when it is all the answer says", () => {
		const said = words(AnswerCard, {
			answer: answer(WITHHELD_NOTE),
			done: null,
			withheld: 1,
			leftOut: 1
		});
		expect(times(said, 'Some details were withheld')).toBe(1);
		expect(said).not.toContain(WITHHELD_VALUES_NOTE);
		expect(said).not.toContain(leftOutNote(1));
	});

	it('marks an answer read from the code, and no other', () => {
		const MARK = 'Read from the code';
		const marked = words(AnswerCard, { answer: answer('Two items.'), done: null, fromCode: true });
		expect(marked).toContain(MARK);
		const html = render(AnswerCard, {
			props: { answer: answer('Two items.'), done: null, fromCode: true }
		}).body;
		expect(html).toContain("Part of this answer comes from the project's code reader");
		// an answer from facts alone, and an earlier one that wasn't, carry no mark
		expect(words(AnswerCard, { answer: answer('Two items.'), done: null })).not.toContain(MARK);
		expect(
			words(EarlierReply, {
				turn: turn({ phase: 'delivered', answer: answer('Two items.'), fromCode: false })
			})
		).not.toContain(MARK);
		// an earlier answer keeps the mark it had
		expect(
			words(EarlierReply, {
				turn: turn({ phase: 'delivered', answer: answer('Two items.'), fromCode: true })
			})
		).toContain(MARK);
	});

	it('says a note the page gives twice once', () => {
		const stale = 'The data may be out of date: the repository was last read 3 days ago.';
		const said = words(AnswerCard, {
			answer: answer('Two items are blocked.'),
			done: null,
			notes: [stale, stale]
		});
		expect(times(said, stale)).toBe(1);
	});
});

describe('the side list', () => {
	function thread(last: Partial<Turn>): Thread {
		return { key: 'c1', turns: [turn(last)], at: Date.now(), updated: Date.now() };
	}
	const list = (items: Thread[]) => ({ items, current: null, projects: [], onnew: () => {} });

	it('says how each conversation’s last question stands, in words as well as a dot', () => {
		expect(words(ChatSidebar, list([thread({ phase: 'expired' })]))).toContain('No longer kept:');
		expect(words(ChatSidebar, list([thread({ phase: 'refused', refusal: REJECTED })]))).toContain(
			'Refused:'
		);
		const answered = words(ChatSidebar, list([thread({ phase: 'delivered' })]));
		expect(answered).toContain('Answered:');
		expect(answered).not.toContain('Answering');
	});
});

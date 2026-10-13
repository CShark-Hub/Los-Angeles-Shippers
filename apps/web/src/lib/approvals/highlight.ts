/**
 * A held draft as the approver reads it: the text in pieces, each flagged sentence its own piece
 * with the checks that flagged it, so the screen can mark it. The text is the draft's, untouched:
 * the pieces put back together are exactly what approving delivers.
 */

import type { Flag } from '$lib/api';

export interface Piece {
	text: string;
	/** The checks that flagged this piece; empty for the text between flagged sentences. */
	flags: Flag[];
}

/** The draft's text cut at its flagged sentences, in order. */
export function pieces(text: string, flags: readonly Flag[]): Piece[] {
	const sentences = [...new Set(flags.map((f) => f.sentence))]
		.map((sentence) => ({ sentence, at: text.indexOf(sentence) }))
		.filter(({ sentence, at }) => sentence !== '' && at >= 0)
		.sort((a, b) => a.at - b.at);
	const out: Piece[] = [];
	let cursor = 0;
	for (const { sentence, at } of sentences) {
		// two flags on overlapping text: the first one marks it
		if (at < cursor) continue;
		if (at > cursor) out.push({ text: text.slice(cursor, at), flags: [] });
		out.push({ text: sentence, flags: flags.filter((f) => f.sentence === sentence) });
		cursor = at + sentence.length;
	}
	if (cursor < text.length || out.length === 0) out.push({ text: text.slice(cursor), flags: [] });
	return out;
}

/** Where each flagged sentence sits in the draft, by position, for a screen that lays the text out. */
export function flaggedRanges(
	text: string,
	flags: readonly Flag[]
): { start: number; end: number; marks: Flag[] }[] {
	const out: { start: number; end: number; marks: Flag[] }[] = [];
	let at = 0;
	for (const piece of pieces(text, flags)) {
		if (piece.flags.length)
			out.push({ start: at, end: at + piece.text.length, marks: piece.flags });
		at += piece.text.length;
	}
	return out;
}

/** Flags whose sentence the draft doesn't hold word for word: shown beside it, never lost. */
export function unplaced(text: string, flags: readonly Flag[]): Flag[] {
	return flags.filter((f) => f.sentence === '' || !text.includes(f.sentence));
}

/**
 * An answer's text, cut into what a screen shows: paragraphs, a list and the note the core adds
 * when it left something out. Composers often write a little Markdown — `**bold**` and items that
 * start with `- `, sometimes all on one line — which read as stray stars and dashes when shown as
 * plain text. Only those two marks are read; nothing becomes markup but `<strong>` and `<li>`, and
 * every piece stays text, so the words on screen are exactly the words the core's checks passed.
 *
 * The text is read by its positions, so a range of it (a sentence a check flagged on a held draft)
 * can be marked wherever the Markdown puts it.
 */

/**
 * The sentence the core adds when it left sentences out (`compose::WITHHELD_NOTE`). It is not
 * always last: the core may add its sentences about data it couldn't read, or data that may be out
 * of date, after it.
 */
export const WITHHELD_NOTE = 'Some details were withheld.';

/** The page's note when the data checks withheld text values from what the answer rests on. */
export const WITHHELD_VALUES_NOTE = 'Some details were withheld from this answer.';

/** The page's note on the sentences the output checks left out, counted. */
export function leftOutNote(n: number): string {
	return `${n} sentence${n === 1 ? ' was' : 's were'} left out because ${n === 1 ? 'it' : 'they'} did not pass a check.`;
}

/** What the checks kept back from an answer, counted from its events. */
export interface KeptBack {
	/** Text values the data checks withheld. */
	withheld: number;
	/** Sentences the output checks left out. */
	leftOut: number;
}

/**
 * An answer as its card shows it: the blocks of its text and the notes under them about what the
 * checks kept back, each thing said once. The core's own note says less than the page's note on
 * the sentences left out, which counts them, so it goes when that note is there, unless it is all
 * the answer says. While the core's note is on the card, the page's notes, which would say the same
 * again, are not.
 */
export function answerCard(text: string, kept: KeptBack): { blocks: Block[]; notes: string[] } {
	const all = answerBlocks(text);
	const said = all.filter((block) => block.kind !== 'note');
	const blocks = kept.leftOut > 0 && said.length > 0 ? said : all;
	const noted = blocks.length > said.length;
	const notes: string[] = [];
	if (!noted && kept.withheld > 0) notes.push(WITHHELD_VALUES_NOTE);
	if (!noted && kept.leftOut > 0) notes.push(leftOutNote(kept.leftOut));
	return { blocks, notes };
}

/** A run of text, bold or not, with what marks it (a flag), if anything does. */
export type Inline<T = never> = { text: string; strong: boolean; marks?: T[] };

/** What a screen lays out, in order. */
export type Block<T = never> =
	| { kind: 'paragraph'; parts: Inline<T>[] }
	| { kind: 'list'; items: Inline<T>[][] }
	| { kind: 'note'; text: string };

/** A range of the text, by position, and what marks it. */
export type Range<T> = { start: number; end: number; marks: T[] };

/** A dash that starts an item: before a bold run, after a stop, a space or a bold run. */
const ITEM = /(^|[\s.:;!?]|\*\*)-\s+(?=\*\*)/g;

export function answerBlocks(text: string): Block[] {
	return markedBlocks<never>(text, []);
}

export function markedBlocks<T>(text: string, ranges: readonly Range<T>[]): Block<T>[] {
	let start = 0;
	let end = text.length;
	while (start < end && /\s/.test(text[start])) start += 1;
	while (end > start && /\s/.test(text[end - 1])) end -= 1;

	// where a line ends, or an item written on the same line begins
	const cuts: { at: number; next: number }[] = [];
	// the core's note is set apart wherever it stands, and said once however often it is there
	const notes = notesIn(text, start, end);
	for (const note of notes) cuts.push({ at: note.at, next: note.at + note.text.length });
	for (let i = start; i < end; i += 1) if (text[i] === '\n') cuts.push({ at: i, next: i + 1 });
	for (const found of text.slice(0, end).matchAll(ITEM)) {
		const dash = (found.index ?? 0) + found[1].length;
		if (dash > start && dash < end) cuts.push({ at: dash, next: dash });
	}
	cuts.sort((a, b) => a.at - b.at);

	const blocks: Block<T>[] = [];
	let cursor = start;
	for (const segment of [...cuts, { at: end, next: end }]) {
		let s = cursor;
		let e = segment.at;
		cursor = segment.next;
		while (s < e && /\s/.test(text[s])) s += 1;
		while (e > s && /\s/.test(text[e - 1])) e -= 1;
		if (s >= e) continue;
		const item = /^[-•]\s+/.exec(text.slice(s, e));
		if (item) {
			const parts = runs(text, s + item[0].length, e, ranges);
			const last = blocks.at(-1);
			if (last?.kind === 'list') last.items.push(parts);
			else blocks.push({ kind: 'list', items: [parts] });
		} else {
			blocks.push({ kind: 'paragraph', parts: runs(text, s, e, ranges) });
		}
	}
	if (notes.length) blocks.push({ kind: 'note', text: notes[0].text });
	return blocks;
}

/**
 * Where the core's note stands in `text[start, end)` as a sentence of its own, never inside
 * another.
 */
function notesIn(text: string, start: number, end: number): { at: number; text: string }[] {
	const out: { at: number; text: string }[] = [];
	const note = WITHHELD_NOTE;
	const length = note.length;
	for (let i = text.indexOf(note, start); i >= 0 && i + length <= end;) {
		const alone =
			(i === start || /\s/.test(text[i - 1])) &&
			(i + length === end || /\s/.test(text[i + length]));
		if (alone) out.push({ at: i, text: note });
		i = text.indexOf(note, i + (alone ? length : 1));
	}
	return out;
}

/** `**bold**` runs as strong; a mark without its pair is dropped, the words around it kept. */
export function inline(text: string): Inline[] {
	return runs<never>(text, 0, text.length, []);
}

/** The runs of `text[start, end)`: bold or not, each cut where a range begins or ends. */
function runs<T>(
	text: string,
	start: number,
	end: number,
	ranges: readonly Range<T>[]
): Inline<T>[] {
	const marks: number[] = [];
	for (let i = text.indexOf('**', start); i >= 0 && i + 2 <= end; i = text.indexOf('**', i + 2)) {
		marks.push(i);
	}
	const paired = marks.length % 2 === 0;
	const out: Inline<T>[] = [];
	for (let k = 0; k <= marks.length; k += 1) {
		const from = k === 0 ? start : marks[k - 1] + 2;
		const to = k < marks.length ? marks[k] : end;
		// with an odd number of marks the last one has no pair: what follows it is plain
		const strong = k % 2 === 1 && (paired || k < marks.length);
		for (const piece of cut(from, to, ranges)) {
			if (piece.from >= piece.to) continue;
			const run: Inline<T> = { text: text.slice(piece.from, piece.to), strong };
			if (piece.marks.length) run.marks = piece.marks;
			const last = out.at(-1);
			if (last && last.strong === run.strong && same(last.marks, run.marks)) last.text += run.text;
			else out.push(run);
		}
	}
	return out;
}

/** `[from, to)` cut where a range begins or ends, each piece with the ranges that hold all of it. */
function cut<T>(from: number, to: number, ranges: readonly Range<T>[]) {
	const points = new Set([from, to]);
	for (const r of ranges) {
		if (r.start > from && r.start < to) points.add(r.start);
		if (r.end > from && r.end < to) points.add(r.end);
	}
	const sorted = [...points].sort((a, b) => a - b);
	return sorted.slice(0, -1).map((at, i) => ({
		from: at,
		to: sorted[i + 1],
		marks: ranges.filter((r) => r.start <= at && r.end >= sorted[i + 1]).flatMap((r) => r.marks)
	}));
}

function same<T>(a: T[] | undefined, b: T[] | undefined): boolean {
	return (a ?? []).length === (b ?? []).length && (a ?? []).every((x, i) => x === (b ?? [])[i]);
}

/** Runs side by side that share their marks, together: one marked stretch reads as one. */
export function grouped<T>(parts: readonly Inline<T>[]): { marks: T[]; parts: Inline<T>[] }[] {
	const out: { marks: T[]; parts: Inline<T>[] }[] = [];
	for (const part of parts) {
		const last = out.at(-1);
		if (last && same(last.marks, part.marks)) last.parts.push(part);
		else out.push({ marks: part.marks ?? [], parts: [part] });
	}
	return out;
}

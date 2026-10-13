import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import type { ProjectSummary } from '$lib/api';

import { EXAMPLES, examplesFor, gridFor } from './examples';

function project(classes: string[]): ProjectSummary {
	return {
		project: 'delta',
		display_name: 'Delta',
		modules: [],
		classes: classes as ProjectSummary['classes'],
		probe: null
	};
}

describe('the tiles of a new conversation', () => {
	it('offers the open tiles only where a project answers open questions', () => {
		const closed = examplesFor([project(['progress', 'tested'])]);
		expect(closed).toHaveLength(4);
		expect(closed.some((example) => example.open)).toBe(false);

		const opened = examplesFor([project(['progress']), project(['progress', 'open'])]);
		expect(opened).toHaveLength(EXAMPLES.length);
		expect(opened.filter((example) => example.open)).toHaveLength(2);

		// nothing known yet: the four everyone may ask
		expect(examplesFor([])).toHaveLength(4);
	});

	it('asks a question in each tile as a person would, and nothing that names code', () => {
		for (const example of EXAMPLES) {
			expect(example.text).toMatch(/\?$/);
			expect(example.text.toLowerCase()).not.toMatch(/\bcode\b|function|file/);
		}
	});
});

/** The columns a grid's class names, on a wide screen. */
function columnsOf(columns: string): number {
	const match = /^xl:grid-cols-(\d)$/.exec(columns);
	if (!match) throw new Error(`not a column class: ${columns}`);
	return Number(match[1]);
}

/**
 * The rows as the browser fills them, each as the cells it takes: each tile in turn, a wide one two
 * cells, and a wide one that doesn't fit in what is left of a row goes to the next, leaving a hole.
 */
function rowsOf(count: number, columns: number, wide: number): number[] {
	const rows = [0];
	for (let i = 0; i < count; i++) {
		const span = Math.min(i < wide ? 2 : 1, columns);
		if (rows[rows.length - 1] + span > columns) {
			// what is left of the row stays empty
			rows.push(0);
		}
		rows[rows.length - 1] += span;
	}
	return rows;
}

describe('the grid of the start screen', () => {
	it('gives every count the screen can get its columns and its wide tiles', () => {
		expect(Array.from({ length: 8 }, (_, i) => gridFor(i + 1))).toEqual([
			{ columns: 'xl:grid-cols-1', wide: 0 },
			{ columns: 'xl:grid-cols-2', wide: 0 },
			{ columns: 'xl:grid-cols-3', wide: 0 },
			{ columns: 'xl:grid-cols-4', wide: 0 },
			// a wide tile and one, then three
			{ columns: 'xl:grid-cols-3', wide: 1 },
			{ columns: 'xl:grid-cols-3', wide: 0 },
			// a wide tile and two, then four
			{ columns: 'xl:grid-cols-4', wide: 1 },
			{ columns: 'xl:grid-cols-4', wide: 0 }
		]);
		// nothing to show yet still names a class
		expect(gridFor(0)).toEqual({ columns: 'xl:grid-cols-1', wide: 0 });
	});

	it('leaves no row of a wide screen short, and no hole before a wide tile', () => {
		for (let count = 1; count <= 16; count++) {
			const { columns, wide } = gridFor(count);
			const across = columnsOf(columns);
			const rows = rowsOf(count, across, wide);
			expect(rows, `${count} tiles`).toEqual(rows.map(() => across));
			// some tiles stay narrow: never all of them wide
			expect(wide).toBeLessThan(count);
		}
	});

	it('puts no more than four tiles in a row, and up to four in one row', () => {
		for (let count = 1; count <= 16; count++) {
			const across = columnsOf(gridFor(count).columns);
			expect(across).toBeLessThanOrEqual(4);
			if (count <= 4) expect(across).toBe(count);
		}
	});

	it('writes every column class out in full, so the stylesheet finds it', () => {
		const source = readFileSync(new URL('./examples.ts', import.meta.url), 'utf8');
		const used = new Set(Array.from({ length: 16 }, (_, i) => gridFor(i + 1).columns));
		for (const columns of used) expect(source).toContain(`'${columns}'`);
	});
});

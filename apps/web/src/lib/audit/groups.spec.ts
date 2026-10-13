import { describe, expect, it } from 'vitest';
import type { AuditItem } from '$lib/api';
import { groupRows, type RowGroup } from './groups';

/** A row as `GET /v1/audit` gives it; only what the grouping reads matters here. */
function row(
	seq: number,
	question: string | null,
	more: Partial<Pick<AuditItem, 'type' | 'project' | 'user'>> = {}
): AuditItem {
	return {
		seq,
		question_id: question,
		type: 'received',
		at: `2026-10-06T08:46:${String(seq).padStart(2, '0')}.000Z`,
		user: null,
		token: null,
		project: null,
		payload: null,
		payload_sha256: 'c'.repeat(64),
		prev_hash: '0'.repeat(64),
		hash: '0'.repeat(64),
		...more
	};
}

/** Each group as its question and the numbers of its rows. */
const runs = (groups: RowGroup[]) =>
	groups.map((group) => [group.question, group.items.map((item) => item.seq)]);

const Q1 = 'q_01JB2M3N4P5Q6R7S8T9V0W1X01';
const Q2 = 'q_01JB2M3N4P5Q6R7S8T9V0W1X02';

describe('the audit rows under their headers', () => {
	it("keeps the chain's order when two questions' rows interleave, heading a question again", () => {
		const items = [row(1, Q1), row(2, Q2), row(3, Q1), row(4, Q2), row(5, Q2)];
		const groups = groupRows(items);
		expect(runs(groups)).toEqual([
			[Q1, [1]],
			[Q2, [2]],
			[Q1, [3]],
			[Q2, [4, 5]]
		]);
		expect(groups.flatMap((group) => group.items)).toEqual(items);
		// a question shown twice is two runs, each with a key of its own
		const keys = groups.map((group) => group.key);
		expect(new Set(keys).size).toBe(keys.length);
		expect(keys).toEqual(['rows-1', 'rows-2', 'rows-3', 'rows-4']);
	});

	it('puts the rows of one question that follow each other under one header, with the first project and person they name', () => {
		const groups = groupRows([
			row(1, Q1, { type: 'received', project: 'delta' }),
			row(2, Q1, { type: 'routed', project: 'delta', user: 'pm@example.com' }),
			row(3, Q1, { type: 'answer', project: 'delta', user: 'lead@example.com' })
		]);
		expect(groups).toHaveLength(1);
		expect(groups[0]).toMatchObject({
			key: 'rows-1',
			question: Q1,
			project: 'delta',
			user: 'pm@example.com'
		});
		expect(groups[0].items.map((item) => item.seq)).toEqual([1, 2, 3]);
	});

	it('keeps the rows of no question together while they follow each other, each naming its own project', () => {
		const groups = groupRows([
			row(1, null, { type: 'probe_judge', project: 'delta' }),
			row(2, null, { type: 'probe_judge', project: 'delta' }),
			row(3, Q1, { project: 'delta' }),
			row(4, null, { type: 'token_revoked', user: 'admin@example.com' })
		]);
		expect(runs(groups)).toEqual([
			[null, [1, 2]],
			[Q1, [3]],
			[null, [4]]
		]);
		// their header names no project or person: each row says its own
		expect(groups[0]).toMatchObject({ key: 'rows-1', project: null, user: null });
		expect(groups[2]).toMatchObject({ key: 'rows-4', project: null, user: null });
		expect(groupRows([])).toEqual([]);
	});

	it('puts the rows of the next page after the others, continuing the last run when they share its question', () => {
		const first = [row(1, null), row(2, Q1), row(3, Q1)];
		const before = groupRows(first);

		const same = groupRows([...first, row(4, Q1), row(5, Q2)]);
		expect(runs(same)).toEqual([
			[null, [1]],
			[Q1, [2, 3, 4]],
			[Q2, [5]]
		]);
		// the run that grew keeps its key, so its header stays where it was
		expect(same[1].key).toBe(before[1].key);

		const other = groupRows([...first, row(4, Q2), row(5, Q1)]);
		expect(other.slice(0, before.length)).toEqual(before);
		expect(runs(other.slice(before.length))).toEqual([
			[Q2, [4]],
			[Q1, [5]]
		]);
	});
});

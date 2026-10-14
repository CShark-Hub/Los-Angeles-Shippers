import { describe, expect, it } from 'vitest';
import type { AuditItem } from '$lib/api';
import { checkChain, rowHash, storedTime } from './chain';

/** The first three rows of a run of the stack on Compose, as `GET /v1/audit` gives them: the core's own hashes. */
// their payloads as the core sent them; the generated type has no fields for a payload, whose
// digest, not its content, is what the hash covers
const ROWS = [
	{
		seq: 1,
		question_id: null,
		type: 'probe_judge',
		at: '2026-10-06T08:46:28.001Z',
		user: null,
		token: null,
		project: 'delta',
		payload: {
			cost_usd: 0.0002,
			items: 15,
			judge_mode: 'chat',
			purpose: 'title_safety'
		},
		payload_sha256: 'c6d4c37a11b1f5da5cda2a97f02afaa25b77b99c95b0700dcde82263d227d663',
		prev_hash: '0000000000000000000000000000000000000000000000000000000000000000',
		hash: '844b4719db28ce619b372ef1f88d6db75a8c96aa3d4cb54a499f1e55a98490c2'
	},
	{
		seq: 2,
		question_id: null,
		type: 'probe_judge',
		at: '2026-10-06T08:46:28.021Z',
		user: null,
		token: null,
		project: 'delta',
		payload: {
			cost_usd: 0,
			items: 1,
			judge_mode: 'rules',
			purpose: 'status_from_candidates'
		},
		payload_sha256: '6600e4d0c40ac34e54a830e8334e23eea8ac34f55f22547be9e0f35c87b2c5dd',
		prev_hash: '844b4719db28ce619b372ef1f88d6db75a8c96aa3d4cb54a499f1e55a98490c2',
		hash: '618b3fb61d71ec6283854c3082f2d861f25db2f86c08b0c47051ea8fa6ff7d02'
	},
	{
		seq: 3,
		question_id: null,
		type: 'probe_judge',
		at: '2026-10-06T08:46:28.029Z',
		user: null,
		token: null,
		project: 'delta',
		payload: {
			cost_usd: 0,
			items: 3,
			judge_mode: 'rules',
			purpose: 'kind'
		},
		payload_sha256: '35c4a0a63da82d0b1ee0456a3b99818f56bcf52585133057477e449f4933693e',
		prev_hash: '618b3fb61d71ec6283854c3082f2d861f25db2f86c08b0c47051ea8fa6ff7d02',
		hash: '1070da81de85dfffc7a8f47ceeb63c52477931f5727299916d727b3d1988d1dc'
	}
] as unknown as AuditItem[];

describe('the audit chain on a page', () => {
	it("recomputes each row's hash exactly as the core wrote it", async () => {
		for (const row of ROWS) expect(await rowHash(row)).toBe(row.hash);
	});

	it('reads a time without milliseconds as the core stored it, with them', () => {
		expect(storedTime('2026-10-04T15:12:03Z')).toBe('2026-10-04T15:12:03.000Z');
		expect(storedTime('2026-10-06T08:46:28.021Z')).toBe('2026-10-06T08:46:28.021Z');
	});

	it('holds when every row matches and each one links to the row before it', async () => {
		expect(await checkChain(ROWS)).toEqual({ state: 'holds', rows: 3, linked: 2 });
		expect(await checkChain([])).toEqual({ state: 'empty' });
	});

	it('names the first row whose content was changed', async () => {
		const changed = ROWS.map((row) => ({ ...row }));
		changed[1].type = 'probe_registered';
		expect(await checkChain(changed)).toEqual({ state: 'broken', seq: 2, what: 'hash' });
		const moved = ROWS.map((row) => ({ ...row }));
		moved[2].at = '2026-10-06T08:46:29.029Z';
		expect(await checkChain(moved)).toEqual({ state: 'broken', seq: 3, what: 'hash' });
	});

	it('names a row that no longer follows the one before it', async () => {
		// a row taken out and the next one rehashed on a made-up predecessor still fails its link
		const [first, , third] = ROWS;
		const forged = { ...third, seq: 2, prev_hash: '0'.repeat(64) };
		forged.hash = await rowHash(forged);
		expect(await checkChain([first, forged])).toEqual({ state: 'broken', seq: 2, what: 'link' });
	});

	it('checks only the links it can see: rows apart from each other are each checked alone', async () => {
		expect(await checkChain([ROWS[0], ROWS[2]])).toEqual({ state: 'holds', rows: 2, linked: 0 });
	});
});

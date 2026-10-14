import { render } from 'svelte/server';
import { describe, expect, it, vi } from 'vitest';

import { mockOverview, mockPeople, mockRepositories } from '$lib/api/mock-admin';
import { ApiError, type Transport } from '$lib/api/transport';
import type { AdminGrant, AdminOverview, AdminPerson, Role } from '$lib/api/types';
import PersonDetail from '$lib/components/admin/PersonDetail.svelte';
import RepositoryCard from '$lib/components/admin/RepositoryCard.svelte';
import { AdminRead, overviewPath, windowOf } from './admin.svelte';
import { commands, MCTL, quote } from './commands';
import {
	approves,
	filterPeople,
	flagged,
	grantable,
	mayApprove,
	noFilter,
	roleChoices,
	summarize
} from './people';
import { bucketLabel, bucketTitle, labelled, percentOf, share, ticks, total } from './stats';
import { alertWords, allClearWords, tokenChip } from './words';

const NOW = new Date('2026-10-08T12:30:00Z');

describe('the commands the panel shows', () => {
	it('quotes a value only when a shell would split it', () => {
		expect(quote('anna@example.com')).toBe('anna@example.com');
		expect(quote('delta,gamma')).toBe('delta,gamma');
		expect(quote('Anna Carter')).toBe("'Anna Carter'");
		expect(quote("it's")).toBe(`'it'\\''s'`);
		expect(quote('$(rm -rf /)')).toBe("'$(rm -rf /)'");
	});

	it('writes each change as the admin command that makes it, through mctl', () => {
		expect(commands.grant('anna@example.com', ['delta'], true)).toBe(
			'mctl user grant --email anna@example.com --projects delta --approver'
		);
		expect(commands.ungrant('anna@example.com', ['delta', 'gamma'])).toBe(
			'mctl user ungrant --email anna@example.com --projects delta,gamma'
		);
		// a grant again without the flag takes the flag off and keeps the grant
		expect(commands.dropApprover('anna@example.com', ['delta', 'portfolio'])).toBe(
			'mctl user grant --email anna@example.com --projects delta,portfolio'
		);
		expect(commands.setRole('anna@example.com', 'tech_lead')).toBe(
			'mctl user set-role --email anna@example.com --role tech_lead'
		);
		expect(commands.addPerson('anna@example.com', 'Anna K', 'pm')).toBe(
			"mctl user add --email anna@example.com --name 'Anna K' --role pm"
		);
		expect(commands.createToken('anna@example.com', 'work laptop')).toBe(
			"mctl token create --email anna@example.com --label 'work laptop' --expires 90d"
		);
		expect(commands.revokeToken('publicid0001')).toBe('mctl token revoke publicid0001');
		expect(commands.quarantine('delta', 'odd output')).toBe(
			"mctl probe quarantine delta --reason 'odd output'"
		);
		expect(commands.release('delta')).toBe('mctl probe release delta');
		expect(MCTL).toBe('docker compose exec core mercury-core admin');
	});
});

describe('the figures', () => {
	it('puts a share in words, with a dash when there is nothing to share', () => {
		expect(percentOf(1, 4)).toBe('25%');
		expect(percentOf(1, 300)).toBe('<1%');
		expect(percentOf(0, 0)).toBe('–');
		expect(share(3, 0)).toBe(0);
	});

	it('draws the axis on clean steps from zero past the peak', () => {
		expect(ticks(0)).toEqual([0, 1]);
		expect(ticks(7)).toEqual([0, 2, 4, 6, 8]);
		expect(ticks(42)).toEqual([0, 20, 40, 60]);
		expect(ticks(130)).toEqual([0, 50, 100, 150]);
	});

	it('labels every column of a week, and about six of a month with the last among them', () => {
		const week = labelled(7);
		expect([0, 1, 2, 3, 4, 5, 6].every(week)).toBe(true);
		const month = labelled(30);
		const shown = Array.from({ length: 30 }, (_, i) => i).filter(month);
		expect(shown).toContain(29);
		expect(shown.length).toBeLessThanOrEqual(7);
	});

	it("names a day's bucket by its day and an hour's by its time", () => {
		expect(bucketLabel('2026-10-08T00:00:00Z', 'day', 'en-GB')).toMatch(/8/);
		expect(bucketLabel('2026-10-08T14:00:00Z', 'hour', 'en-GB')).toMatch(/\d{2}:00/);
	});

	it("names a day's bucket by its UTC day for a viewer west of UTC, and an hour in their time", () => {
		// the viewer's zone is the process's: New York, where the core's day starts the evening before
		const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
		process.env.TZ = 'America/New_York';
		try {
			expect(bucketLabel('2026-10-08T00:00:00Z', 'day', 'en-GB')).toBe('Thu 8');
			expect(bucketTitle('2026-10-08T00:00:00Z', 'day', 'en-GB')).toBe('Thursday 8 October');
			expect(bucketLabel('2026-10-08T14:00:00Z', 'hour', 'en-GB')).toBe('10:00');
			expect(bucketTitle('2026-10-08T14:00:00Z', 'hour', 'en-GB')).toBe('Thu 8 Oct, 10:00–11:00');
		} finally {
			process.env.TZ = zone;
		}
	});
});

describe("the mock's admin data adds up like the core's", () => {
	for (const window of ['24h', '7d', '30d'] as const) {
		it(`in ${window}: the series, the channels and the projects each sum to the totals`, () => {
			const o: AdminOverview = mockOverview(window, NOW);
			expect(o.series).toHaveLength(window === '24h' ? 24 : window === '7d' ? 7 : 30);
			expect(o.bucket).toBe(window === '24h' ? 'hour' : 'day');
			const all = total(o.totals);
			expect(o.series.reduce((n, p) => n + total(p.counts), 0)).toBe(all);
			expect(o.channels.reduce((n, c) => n + total(c.counts), 0)).toBe(all);
			expect(o.channels.map((c) => c.channel)).toEqual(['cli', 'web', 'mcp']);
			expect(o.projects.reduce((n, p) => n + total(p.counts), 0)).toBe(all);
			expect(o.refusals.reduce((n, r) => n + r.count, 0)).toBe(o.totals.refused);
			expect(o.judge.jev + o.judge.chat + o.judge.rules + o.judge.none).toBe(all);
		});
	}

	it('lists every person with tokens in each state, and never a secret', () => {
		const people = mockPeople(NOW);
		const states = new Set(people.people.flatMap((p) => p.tokens.map((t) => t.state)));
		expect(states).toEqual(new Set(['active', 'expired', 'revoked']));
		expect(JSON.stringify(people)).not.toMatch(/mcy_/);
		expect(JSON.stringify(mockRepositories(NOW))).not.toMatch(/mcy_/);
	});
});

describe('the People screen', () => {
	const { people, projects } = mockPeople(NOW);

	it('finds people by a part of their name or address, any case', () => {
		expect(
			filterPeople(people, { ...noFilter(), query: 'REYES' }).map((p) => p.display_name)
		).toEqual(['Dana Reyes']);
		expect(filterPeople(people, { ...noFilter(), query: 'sam.p' })).toHaveLength(1);
	});

	it('narrows by role, project and status together', () => {
		const pmsOnBilling = filterPeople(people, { ...noFilter(), role: 'pm', project: 'billing' });
		expect(pmsOnBilling.map((p) => p.email).sort()).toEqual([
			'dana.reyes@example.com',
			'olivia.brown@example.com'
		]);
		const activeOnly = filterPeople(people, {
			...noFilter(),
			project: 'billing',
			status: 'active'
		});
		expect(activeOnly.every((p) => p.disabled_at === null)).toBe(true);
	});

	it('counts what the header shows, approvers by a flag in effect', () => {
		expect(summarize(people)).toEqual({
			people: 6,
			disabled: 1,
			activeTokens: 5,
			withoutProject: 1,
			approvers: 1
		});
	});

	it('lets only a project manager or a tech lead approve, and counts no one else’s flag', () => {
		const roles: Role[] = ['pm', 'tech_lead', 'board', 'admin'];
		expect(roles.filter(mayApprove)).toEqual(['pm', 'tech_lead']);
		const sam = people.find((p) => p.email === 'sam.patel@example.com') as AdminPerson;
		const delta = sam.grants.find((g) => g.project === 'delta') as AdminGrant;
		expect(approves(sam, delta)).toBe(true);
		// a board member's flag from before the rule: still on the grant, in effect nowhere
		const lee = people.find((p) => p.email === 'lee.chen@example.com') as AdminPerson;
		expect(lee.role).toBe('board');
		expect(flagged(lee)).toEqual(['portfolio']);
		expect(approves(lee, lee.grants[0])).toBe(false);
	});

	it('offers no board or admin role to anyone holding an approver flag, in effect or not', () => {
		const of = (email: string) =>
			roleChoices(people.find((p) => p.email === email) as AdminPerson).map(
				(c) => `${c.role}${c.refused ? ' (refused)' : ''}`
			);
		expect(of('sam.patel@example.com')).toEqual(['pm', 'board (refused)', 'admin (refused)']);
		expect(of('dana.reyes@example.com')).toEqual(['tech_lead', 'board', 'admin']);
		expect(of('lee.chen@example.com')).toEqual(['pm', 'tech_lead', 'admin (refused)']);
		expect(of('kai.moreno@example.com')).toEqual(['tech_lead', 'board', 'admin']);
		expect(of('admin@example.com')).toEqual(['pm', 'tech_lead', 'board']);
		const [refused] = roleChoices(
			people.find((p) => p.email === 'lee.chen@example.com') as AdminPerson
		).filter((c) => c.refused !== null);
		expect(refused.refused).toBe(
			'They hold an approver flag, which only a project manager or a tech lead may hold.'
		);
	});

	it('shows a person only the commands the core takes', () => {
		/** The person's panel as the server renders it: its markup, and the words a person reads. */
		const panel = (email: string) => {
			const person = people.find((p) => p.email === email) as AdminPerson;
			const { body } = render(PersonDetail, { props: { person, projects } });
			const words = body
				.replace(/<!--[\s\S]*?-->/g, '')
				.replace(/<[^>]+>/g, ' ')
				.replace(/&#39;|&apos;/g, "'")
				.replace(/\s+/g, ' ');
			return { body, words };
		};

		// a board member: no approver flag to grant; the old one grants nothing, and the way to clear it
		const lee = panel('lee.chen@example.com');
		expect(lee.body).not.toContain('type="checkbox"');
		expect(lee.words).toContain('Not as an approver: only a project manager or a tech lead');
		expect(lee.words).toContain('Approver flag, no effect');
		expect(lee.words).toContain(
			'mctl user grant --email lee.chen@example.com --projects portfolio'
		);
		expect(lee.words).not.toContain('--approver');
		expect(lee.body).toMatch(/<option value="admin"[^>]*disabled/);

		// an approver: may grant as one, but not be moved to board or admin
		const sam = panel('sam.patel@example.com');
		expect(sam.body).toContain('type="checkbox"');
		expect(sam.words).not.toContain('Not as an approver');
		expect(sam.words).not.toContain('no effect');
		expect(sam.body).toMatch(/<option value="board"[^>]*disabled/);
		expect(sam.body).not.toMatch(/<option value="pm"[^>]*disabled/);
		expect(sam.words).toContain('Not offered: board member, administrator.');

		// a disabled person: the core changes none of their access, so nothing to take back
		const olivia = panel('olivia.brown@example.com');
		expect(olivia.words).not.toContain('Take back');
		expect(olivia.words).not.toContain('user ungrant');
	});

	it('marks an approver on a repository only by a flag in effect', () => {
		// a repository's row carries the role and the flag together
		expect(approves({ role: 'tech_lead' }, { approver: true })).toBe(true);
		expect(approves({ role: 'board' }, { approver: true })).toBe(false);
		expect(approves({ role: 'pm' }, { approver: false })).toBe(false);

		const delta = mockRepositories(NOW).repositories.find((r) => r.project === 'delta');
		if (!delta) throw new Error('the mock has no delta');
		const repository = {
			...delta,
			people: [
				{
					email: 'dana.reyes@example.com',
					display_name: 'Dana Reyes',
					role: 'pm' as const,
					approver: true,
					disabled_at: null
				},
				{
					email: 'lee.chen@example.com',
					display_name: 'Lee Chen',
					role: 'board' as const,
					approver: true,
					disabled_at: null
				}
			]
		};
		const { body } = render(RepositoryCard, { props: { repository } });
		const words = body
			.replace(/<!--[\s\S]*?-->/g, '')
			.replace(/<[^>]+>/g, ' ')
			.replace(/\s+/g, ' ');
		expect(words).toContain('Dana Reyes Project manager Approver');
		expect(words).toContain('Lee Chen Board member Approver flag, no effect');
	});

	it('offers to grant only what the person lacks, portfolio included', () => {
		const dana = people.find((p) => p.email === 'dana.reyes@example.com') as AdminPerson;
		expect(grantable(dana, projects)).toEqual(['gamma', 'portfolio']);
	});

	it('marks a token that runs out within a week', () => {
		const soon = {
			public_id: 'publicid0001',
			label: 'laptop',
			state: 'active' as const,
			created_at: '2026-07-12T00:00:00Z',
			expires_at: '2026-10-11T00:00:00Z',
			revoked_at: null,
			last_used_at: null
		};
		expect(tokenChip(soon, NOW)).toEqual({ label: 'Expires soon', tone: 'warn' });
		expect(tokenChip({ ...soon, expires_at: '2026-12-01T00:00:00Z' }, NOW).tone).toBe('good');
		expect(tokenChip({ ...soon, state: 'revoked' }, NOW).label).toBe('Revoked');
	});
});

describe('the alerts in words', () => {
	it('says the share of the budget, rounded down, as the core does', () => {
		const words = alertWords(
			{
				kind: 'spend',
				project: null,
				since: null,
				judge_mode: null,
				spent_usd: 1.599,
				budget_usd: 2
			},
			(iso) => iso
		);
		expect(words).toContain('79%');
	});

	it('says a daily budget of 0 lets nothing be asked, with no share of it', () => {
		const words = alertWords(
			{
				kind: 'spend',
				project: null,
				since: null,
				judge_mode: null,
				spent_usd: 0,
				budget_usd: 0
			},
			(iso) => iso
		);
		expect(words).toBe('The daily budget is 0, so nothing can be asked.');
	});

	it('names a probe that never registered', () => {
		const words = alertWords(
			{
				kind: 'probe_silent',
				project: 'gamma',
				since: null,
				judge_mode: null,
				spent_usd: null,
				budget_usd: null
			},
			(iso) => iso
		);
		expect(words).toBe('The gamma probe has never registered.');
	});

	it('says what "all clear" checked, and never vouches for a judge no question went through', () => {
		const week = mockOverview('7d', NOW);
		const clear = allClearWords(week);
		expect(clear).toHaveLength(3);
		expect(clear[0]).toBe("Today's spend is under 80% of the daily budget.");
		expect(clear[1]).toContain('questions that ended in the last 24 hours');
		expect(clear[2]).toContain('heard from in the last 15 minutes');

		// nothing went through the judge (none needed one, or nothing ended): it says so
		const quiet = allClearWords({ ...week, judge: { jev: 0, chat: 0, rules: 0, none: 4 } });
		expect(quiet[1]).toBe(
			'No question in this window went through the judge, so nothing yet says how it runs.'
		);
		expect(quiet.join(' ')).not.toMatch(/Jev|runs on/);
	});
});

describe('a read of the admin panel', () => {
	it('reads the window the address names, the default otherwise', () => {
		expect(windowOf('30d')).toBe('30d');
		expect(windowOf('1y')).toBe('7d');
		expect(overviewPath('24h')).toBe('/v1/admin/overview?window=24h');
	});

	it('keeps what came and when', async () => {
		const api = { get: vi.fn(async () => ({ people: [] })) } as unknown as Transport;
		const read = new AdminRead<{ people: unknown[] }>(
			async () => api,
			async () => {}
		);
		await read.load('/v1/admin/people');
		expect(read.data).toEqual({ people: [] });
		expect(Number.isNaN(Date.parse(read.readAt ?? ''))).toBe(false);
		expect(read.failure).toBeNull();
	});

	it("drops what was on screen and reads the role again on the core's 403", async () => {
		let calls = 0;
		const api = {
			get: vi.fn(async () => {
				calls += 1;
				if (calls === 1) return { people: [1] };
				throw new ApiError(403, 'invalid_request', 'Only an admin reads the admin panel.');
			})
		} as unknown as Transport;
		const roleChanged = vi.fn(async () => {});
		const read = new AdminRead<{ people: unknown[] }>(async () => api, roleChanged);
		await read.load('/v1/admin/people');
		await read.load('/v1/admin/people');
		expect(read.data).toBeNull();
		expect(read.failure?.message).toBe('Only an admin reads the admin panel.');
		expect(roleChanged).toHaveBeenCalledOnce();
	});
});

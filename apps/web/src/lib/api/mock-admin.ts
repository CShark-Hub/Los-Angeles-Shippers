/**
 * Mock mode's admin panel: what the core's `GET /v1/admin/*` would say about a small team, so the
 * panel can be built and tried without a core. The numbers come from a seeded generator, so they
 * are the same at every reload, and every total adds up as the core's do: the series, the channels
 * and the projects each sum to the window's totals, the refusal codes to its refusals.
 */

import type {
	AdminOverview,
	AdminPeople,
	AdminPerson,
	AdminRepositories,
	AdminToken,
	OutcomeCounts,
	OverviewWindow
} from './types';

const HOUR = 60 * 60 * 1000;
const DAY = 24 * HOUR;

type Outcome = keyof OutcomeCounts;
const OUTCOMES: readonly Outcome[] = ['answered', 'degraded', 'held', 'refused', 'other'];

/** A small, fast generator with a seed: the same numbers at every reload. */
function seeded(seed: number): () => number {
	let state = seed >>> 0;
	return () => {
		state = (state + 0x6d2b79f5) >>> 0;
		let t = state;
		t = Math.imul(t ^ (t >>> 15), t | 1);
		t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

function zero(): OutcomeCounts {
	return { answered: 0, degraded: 0, held: 0, refused: 0, other: 0 };
}

function add(into: OutcomeCounts, from: OutcomeCounts): void {
	for (const outcome of OUTCOMES) into[outcome] += from[outcome];
}

/** One bucket's questions: mostly answered, a few refused, now and then held or degraded. */
function bucketCounts(volume: number, random: () => number, degradedHere: boolean): OutcomeCounts {
	const n = Math.max(0, Math.round(volume));
	const counts = zero();
	for (let i = 0; i < n; i++) {
		const r = random();
		if (r < 0.12) counts.refused += 1;
		else if (r < 0.17) counts.held += 1;
		else if (r < 0.2) counts.other += 1;
		else if (degradedHere && r < 0.55) counts.degraded += 1;
		else counts.answered += 1;
	}
	return counts;
}

/** Splits each outcome of `counts` over the `weights`, so the parts add up to the whole. */
function split<K extends string>(
	counts: OutcomeCounts,
	weights: Record<K, number>
): Record<K, OutcomeCounts> {
	const keys = Object.keys(weights) as K[];
	const sum = keys.reduce((s, k) => s + weights[k], 0);
	const parts = Object.fromEntries(keys.map((k) => [k, zero()])) as Record<K, OutcomeCounts>;
	for (const outcome of OUTCOMES) {
		let left = counts[outcome];
		keys.forEach((key, i) => {
			const part =
				i === keys.length - 1 ? left : Math.round((counts[outcome] * weights[key]) / sum);
			const taken = Math.min(part, left);
			parts[key][outcome] += taken;
			left -= taken;
		});
	}
	return parts;
}

function startOfHour(at: Date): Date {
	return new Date(Math.floor(at.getTime() / HOUR) * HOUR);
}

function startOfDay(at: Date): Date {
	return new Date(Math.floor(at.getTime() / DAY) * DAY);
}

export function mockOverview(window: OverviewWindow, now: Date): AdminOverview {
	const hourly = window === '24h';
	const buckets = hourly ? 24 : window === '7d' ? 7 : 30;
	const span = hourly ? HOUR : DAY;
	const last = hourly ? startOfHour(now) : startOfDay(now);
	const random = seeded(hourly ? 24 : buckets * 7919);
	const series = Array.from({ length: buckets }, (_, i) => {
		const start = new Date(last.getTime() - (buckets - 1 - i) * span);
		let volume: number;
		if (hourly) {
			const hour = start.getUTCHours();
			volume = hour >= 7 && hour <= 17 ? 3 + random() * 6 : random() * 1.4;
		} else {
			const weekday = start.getUTCDay();
			const weekend = weekday === 0 || weekday === 6;
			// the team started asking more as the month went on
			const growth = 0.7 + (0.6 * i) / buckets;
			volume = (weekend ? 4 + random() * 5 : 22 + random() * 18) * growth;
		}
		// the Decisions endpoint was down one afternoon: the chat fallback judged for a while
		const degradedHere = hourly ? i === 15 : i === buckets - 3;
		return { start: start.toISOString(), counts: bucketCounts(volume, random, degradedHere) };
	});
	const totals = zero();
	for (const point of series) add(totals, point.counts);
	const all = OUTCOMES.reduce((s, o) => s + totals[o], 0);

	const channels = split(totals, { web: 61, cli: 24, mcp: 15 });
	// a question no project took was refused or clarified first
	const unrouted: OutcomeCounts = {
		...zero(),
		refused: Math.round(totals.refused * 0.35),
		other: Math.round(totals.other * 0.5)
	};
	const routed = zero();
	for (const outcome of OUTCOMES) routed[outcome] = totals[outcome] - unrouted[outcome];
	const byProject = split(routed, { delta: 64, billing: 36 });
	const cost = (counts: OutcomeCounts) =>
		Number(
			(
				(counts.answered + counts.degraded + counts.held) * 0.0091 +
				counts.refused * 0.0012
			).toFixed(4)
		);

	const refusalWeights = {
		asks_for_code: 30,
		forbidden_project: 22,
		out_of_scope: 18,
		asks_to_evaluate_people: 12,
		forbidden_class: 9,
		contains_agent_instruction: 6,
		rejected_by_approver: 3
	} as const;
	const refusalCodes = Object.keys(refusalWeights) as (keyof typeof refusalWeights)[];
	let refusalsLeft = totals.refused;
	const refusals = refusalCodes
		.map((code, i) => {
			const count =
				i === refusalCodes.length - 1
					? refusalsLeft
					: Math.min(refusalsLeft, Math.round((totals.refused * refusalWeights[code]) / 100));
			refusalsLeft -= count;
			return { code, count };
		})
		.filter((r) => r.count > 0)
		.sort((a, b) => b.count - a.count);

	const classed = all - unrouted.other;
	const classWeights = { progress: 38, blocked: 21, health: 16, tested: 14, changes: 11 } as const;
	let classLeft = classed;
	const classes = (Object.keys(classWeights) as (keyof typeof classWeights)[])
		.map((name, i, names) => {
			const count =
				i === names.length - 1 ? classLeft : Math.round((classed * classWeights[name]) / 100);
			classLeft -= count;
			return { class: name, count: Math.max(0, count) };
		})
		.sort((a, b) => b.count - a.count);

	const refusedJudged = Math.round(totals.refused * 0.7);
	const chat = Math.round(totals.degraded * 0.85);
	const scale = hourly ? 0.86 : window === '7d' ? 1 : 1.08;

	return {
		window,
		// the first bucket's start, as the core says it: the buckets add up to the totals
		since: series[0].start,
		until: now.toISOString(),
		bucket: hourly ? 'hour' : 'day',
		totals,
		series,
		channels: (['cli', 'web', 'mcp'] as const).map((channel) => ({
			channel,
			counts: channels[channel]
		})),
		projects: [
			{ project: 'delta', counts: byProject.delta, cost_usd: cost(byProject.delta) },
			{ project: 'billing', counts: byProject.billing, cost_usd: cost(byProject.billing) },
			{ project: null, counts: unrouted, cost_usd: cost(unrouted) }
		],
		classes,
		refusals,
		judge: {
			jev: totals.answered + totals.held + refusedJudged + totals.other,
			chat,
			rules: totals.degraded - chat,
			none: totals.refused - refusedJudged
		},
		duration_ms: {
			p50: Math.round(6200 * scale),
			p95: Math.round(14600 * scale),
			max: Math.round(22800 * scale)
		},
		cost_usd: Number((cost(byProject.delta) + cost(byProject.billing) + cost(unrouted)).toFixed(4)),
		askers: hourly ? 4 : 6,
		signoffs: {
			pending: 2,
			approved: Math.max(0, totals.held - 3),
			rejected: Math.min(1, totals.held),
			expired: 0
		},
		spend: {
			day: startOfDay(now).toISOString().slice(0, 10),
			everyone_usd: 0.3142,
			daily_budget_usd: 2,
			per_user_daily_usd: 1,
			people: [
				{
					user_id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X2Y',
					email: 'dana.reyes@example.com',
					display_name: 'Dana Reyes',
					spent_usd: 0.1418
				},
				{
					user_id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X4A',
					email: 'sam.patel@example.com',
					display_name: 'Sam Patel',
					spent_usd: 0.1102
				},
				{
					user_id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X3Z',
					email: 'admin@example.com',
					display_name: 'Mock Admin',
					spent_usd: 0.0622
				}
			]
		},
		alerts: [
			{
				kind: 'probe_silent',
				project: 'billing',
				since: new Date(now.getTime() - 25 * 60_000).toISOString(),
				judge_mode: null,
				spent_usd: null,
				budget_usd: null
			}
		],
		settings: {
			models: [
				{ role: 'judge', backend: 'openrouter', kind: 'jev', model: 'typesafe/jev-1.13' },
				{
					role: 'judge_fallback',
					backend: 'openrouter',
					kind: 'chat',
					model: 'openai/gpt-4.1-mini-2025-04-14'
				},
				{
					role: 'planner',
					backend: 'openrouter',
					kind: null,
					model: 'openai/gpt-4.1-mini-2025-04-14'
				},
				{
					role: 'composer',
					backend: 'openrouter',
					kind: null,
					model: 'anthropic/claude-4.6-sonnet-20260217'
				}
			],
			budgets: { per_question_usd: 0.05, per_user_daily_usd: 1, global_daily_usd: 2 },
			checks: [
				check('project', 'input', { clarify_below: 0.7 }),
				check('class', 'input', { clarify_below: 0.6 }),
				check('asks_for_code', 'input', { block_at: 0.5 }),
				check('asks_to_evaluate_people', 'input', { block_at: 0.5 }),
				check('asks_for_client_commitment', 'input', { block_at: 0.5 }),
				check('contains_agent_instruction', 'input', { block_at: 0.5 }),
				check('args_match_question', 'tool', { pass_at: 0.25, block_at: 0.1 }),
				check('args_free_of_instructions', 'tool', { pass_at: 0.5, block_at: 0.2 }),
				check('field_contains_agent_instruction', 'data', { block_at: 0.5 }),
				check('sentence_contains_code_or_identifier', 'output', { block_at: 0.8, review_at: 0.7 }),
				check('sentence_contains_path_or_secret', 'output', { block_at: 0.5 }),
				check('sentence_mentions_client', 'output', { block_at: 0.5 }),
				check('sentence_evaluates_people', 'output', { block_at: 0.5 }),
				check('sentence_commits_to_client', 'output', { block_at: 0.5 }),
				check('grounded', 'output', { pass_at: 0.6, block_at: 0.6 })
			]
		}
	};
}

function check(
	id: string,
	stage: 'input' | 'tool' | 'data' | 'output',
	thresholds: Partial<Record<'block_at' | 'review_at' | 'pass_at' | 'clarify_below', number>>
) {
	return {
		id,
		stage,
		block_at: thresholds.block_at ?? null,
		review_at: thresholds.review_at ?? null,
		pass_at: thresholds.pass_at ?? null,
		clarify_below: thresholds.clarify_below ?? null
	};
}

function token(
	public_id: string,
	label: string,
	now: Date,
	{
		createdDaysAgo,
		validDays = 90,
		revokedDaysAgo,
		usedMinutesAgo
	}: {
		createdDaysAgo: number;
		validDays?: number;
		revokedDaysAgo?: number;
		usedMinutesAgo?: number;
	}
): AdminToken {
	const created = now.getTime() - createdDaysAgo * DAY;
	const expires = created + validDays * DAY;
	const revoked = revokedDaysAgo === undefined ? null : now.getTime() - revokedDaysAgo * DAY;
	return {
		public_id,
		label,
		state: revoked !== null ? 'revoked' : expires <= now.getTime() ? 'expired' : 'active',
		created_at: new Date(created).toISOString(),
		expires_at: new Date(expires).toISOString(),
		revoked_at: revoked === null ? null : new Date(revoked).toISOString(),
		last_used_at:
			usedMinutesAgo === undefined
				? null
				: new Date(now.getTime() - usedMinutesAgo * 60_000).toISOString()
	};
}

const PM_CLASSES = [
	{ class: 'progress', signoff: false },
	{ class: 'blocked', signoff: false },
	{ class: 'health', signoff: true },
	{ class: 'changes', signoff: false },
	{ class: 'open', signoff: false }
] as const;

const LEAD_CLASSES = [
	{ class: 'progress', signoff: false },
	{ class: 'tested', signoff: false },
	{ class: 'blocked', signoff: false },
	{ class: 'health', signoff: false },
	{ class: 'changes', signoff: false },
	{ class: 'open', signoff: false }
] as const;

export function mockPeople(now: Date): AdminPeople {
	const ago = (days: number) => new Date(now.getTime() - days * DAY).toISOString();
	const minutesAgo = (minutes: number) => new Date(now.getTime() - minutes * 60_000).toISOString();
	const people: AdminPerson[] = [
		{
			id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X3Z',
			email: 'admin@example.com',
			display_name: 'Mock Admin',
			role: 'admin',
			created_at: ago(9),
			disabled_at: null,
			grants: [
				{ project: 'billing', approver: false, classes: [...LEAD_CLASSES] },
				{ project: 'delta', approver: false, classes: [...LEAD_CLASSES] }
			],
			tokens: [
				token('mockTokn0002', 'admin laptop', now, { createdDaysAgo: 9, usedMinutesAgo: 0 })
			],
			questions_30d: 14,
			last_asked_at: minutesAgo(48),
			spent_today_usd: 0.0622
		},
		{
			id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X2Y',
			email: 'dana.reyes@example.com',
			display_name: 'Dana Reyes',
			role: 'pm',
			created_at: ago(8),
			disabled_at: null,
			grants: [
				{ project: 'billing', approver: false, classes: [...PM_CLASSES] },
				{ project: 'delta', approver: false, classes: [...PM_CLASSES] }
			],
			tokens: [
				token('mockTokn0001', 'work laptop', now, { createdDaysAgo: 8, usedMinutesAgo: 6 }),
				token('mockTokn0005', 'phone', now, { createdDaysAgo: 8, revokedDaysAgo: 3 })
			],
			questions_30d: 96,
			last_asked_at: minutesAgo(6),
			spent_today_usd: 0.1418
		},
		{
			id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X4A',
			email: 'sam.patel@example.com',
			display_name: 'Sam Patel',
			role: 'tech_lead',
			created_at: ago(8),
			disabled_at: null,
			grants: [
				{ project: 'delta', approver: true, classes: [...LEAD_CLASSES] },
				{ project: 'gamma', approver: false, classes: null }
			],
			tokens: [
				token('mockTokn0003', 'desktop', now, { createdDaysAgo: 85, usedMinutesAgo: 140 }),
				token('mockTokn0004', 'agent (MCP)', now, { createdDaysAgo: 6, usedMinutesAgo: 22 })
			],
			questions_30d: 71,
			last_asked_at: minutesAgo(22),
			spent_today_usd: 0.1102
		},
		{
			id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X5B',
			email: 'lee.chen@example.com',
			display_name: 'Lee Chen',
			role: 'board',
			created_at: ago(7),
			disabled_at: null,
			grants: [
				{ project: 'portfolio', approver: true, classes: [{ class: 'portfolio', signoff: true }] }
			],
			tokens: [
				token('mockTokn0006', 'tablet', now, { createdDaysAgo: 7, usedMinutesAgo: 60 * 26 })
			],
			questions_30d: 9,
			last_asked_at: minutesAgo(60 * 26),
			spent_today_usd: 0
		},
		{
			id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X6C',
			email: 'kai.moreno@example.com',
			display_name: 'Kai Moreno',
			role: 'pm',
			created_at: ago(1),
			disabled_at: null,
			grants: [],
			tokens: [],
			questions_30d: 0,
			last_asked_at: null,
			spent_today_usd: 0
		},
		{
			id: 'u_01JB2M3N4P5Q6R7S8T9V0W1X7D',
			email: 'olivia.brown@example.com',
			display_name: 'Olivia Brown',
			role: 'pm',
			created_at: ago(8),
			disabled_at: ago(2),
			grants: [{ project: 'billing', approver: false, classes: [...PM_CLASSES] }],
			tokens: [
				token('mockTokn0007', 'laptop', now, {
					createdDaysAgo: 8,
					revokedDaysAgo: 2,
					usedMinutesAgo: 60 * 50
				}),
				token('mockTokn0008', 'old laptop', now, { createdDaysAgo: 40, validDays: 30 })
			],
			questions_30d: 12,
			last_asked_at: minutesAgo(60 * 50),
			spent_today_usd: 0
		}
	];
	return {
		people: people.sort((a, b) => a.email.localeCompare(b.email)),
		projects: ['billing', 'delta', 'gamma'],
		classes_known: true
	};
}

export function mockRepositories(now: Date): AdminRepositories {
	const minutesAgo = (minutes: number) => new Date(now.getTime() - minutes * 60_000).toISOString();
	const person = (
		email: string,
		display_name: string,
		role: 'pm' | 'tech_lead' | 'board' | 'admin',
		approver: boolean,
		disabled_at: string | null = null
	) => ({ email, display_name, role, approver, disabled_at });
	return {
		repositories: [
			{
				project: 'billing',
				display_name: 'Billing',
				access_max: 'metadata',
				probe: {
					status: 'stale',
					access: 'metadata',
					access_limited_by: null,
					schema: 'mercury.facts/0.1',
					registered_at: minutesAgo(60 * 24 * 6),
					last_seen_at: minutesAgo(25),
					quarantine_reason: null,
					modules: ['Payments', 'Plans'],
					tools: ['describe', 'progress', 'blocked', 'health']
				},
				people: [
					person('admin@example.com', 'Mock Admin', 'admin', false),
					person('dana.reyes@example.com', 'Dana Reyes', 'pm', false),
					person('olivia.brown@example.com', 'Olivia Brown', 'pm', false, minutesAgo(60 * 48))
				],
				questions_30d: 61,
				last_asked_at: minutesAgo(31)
			},
			{
				project: 'delta',
				display_name: 'Delta — invoicing',
				access_max: 'code',
				probe: {
					status: 'healthy',
					access: 'metadata',
					access_limited_by: 'token',
					schema: 'mercury.facts/0.1',
					registered_at: minutesAgo(60 * 24 * 7),
					last_seen_at: minutesAgo(1),
					quarantine_reason: null,
					modules: ['Invoicing', 'Export', 'Auth'],
					tools: ['describe', 'progress', 'tested', 'blocked', 'health', 'changes']
				},
				people: [
					person('admin@example.com', 'Mock Admin', 'admin', false),
					person('dana.reyes@example.com', 'Dana Reyes', 'pm', false),
					person('sam.patel@example.com', 'Sam Patel', 'tech_lead', true)
				],
				questions_30d: 128,
				last_asked_at: minutesAgo(6)
			},
			{
				project: 'gamma',
				display_name: 'Gamma',
				access_max: 'metadata',
				probe: null,
				people: [person('sam.patel@example.com', 'Sam Patel', 'tech_lead', false)],
				questions_30d: 0,
				last_asked_at: null
			},
			{
				project: 'legacy',
				display_name: null,
				access_max: null,
				probe: {
					status: 'quarantined',
					access: 'metadata',
					access_limited_by: 'manifest',
					schema: 'mercury.facts/0.1',
					registered_at: minutesAgo(60 * 24 * 12),
					last_seen_at: minutesAgo(60 * 24 * 4),
					quarantine_reason: 'Sent a field the probe spec does not have',
					modules: ['Reports'],
					tools: ['describe', 'progress']
				},
				people: [],
				questions_30d: 3,
				last_asked_at: minutesAgo(60 * 24 * 4)
			}
		]
	};
}

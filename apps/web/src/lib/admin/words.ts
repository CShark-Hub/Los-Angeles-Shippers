/**
 * Words for the codes the admin panel shows: what became of a question, why it was refused, a
 * token's state, a probe's. Short labels for tables and legends, with a line that explains each.
 */

import type {
	AccessLimit,
	AdminAlert,
	AdminOverview,
	AdminProbe,
	AdminToken,
	Channel,
	JudgeMode,
	OutcomeCounts,
	QuestionClass,
	RefusalCode
} from '$lib/api';
import type { Tone } from '$lib/ask/words';
import { describeCost } from '$lib/format';

export type Outcome = keyof OutcomeCounts;

/** The outcomes in the order a stack is built from its baseline: the good news first. */
export const OUTCOMES: readonly Outcome[] = ['answered', 'degraded', 'held', 'refused', 'other'];

export const OUTCOME_WORDS: Record<Outcome, { label: string; meaning: string }> = {
	answered: { label: 'Answered', meaning: 'Delivered, checked by Jev.' },
	degraded: {
		label: 'Degraded',
		meaning: 'Delivered while the judge ran on its fallback: the chat model or the rules.'
	},
	held: { label: 'Held', meaning: 'Went to an approver before anyone saw it.' },
	refused: { label: 'Refused', meaning: 'Mercury said no, with a hint of what to ask instead.' },
	other: { label: 'Other', meaning: 'A clarify, a failure, or still running.' }
};

/** The chart colour of each outcome, a token of the look: the same outcome, the same colour. */
export const OUTCOME_COLOR: Record<Outcome, string> = {
	answered: 'var(--series-1)',
	degraded: 'var(--series-2)',
	held: 'var(--series-3)',
	refused: 'var(--series-4)',
	other: 'var(--series-other)'
};

const REFUSALS: Record<RefusalCode, string> = {
	forbidden_class: 'Kind of question not allowed',
	forbidden_project: 'Project not granted',
	asks_for_code: 'Asked for code',
	asks_to_evaluate_people: 'Asked to rate people',
	asks_for_client_commitment: 'Asked for a commitment',
	contains_agent_instruction: 'Instructions in the question',
	confidential: 'Asked for something confidential',
	out_of_scope: 'Out of scope',
	strict_mode: 'Strict mode (judge unsure)',
	rejected_by_approver: 'Rejected by an approver'
};

export function refusalLabel(code: RefusalCode): string {
	return REFUSALS[code] ?? code;
}

const CLASSES: Record<QuestionClass, string> = {
	progress: 'Progress',
	tested: 'Tests',
	blocked: 'Blockers',
	health: 'Build health',
	changes: 'Changes',
	open: 'Open question',
	portfolio: 'Portfolio',
	forbidden: 'Forbidden',
	other: 'Other'
};

export function classLabel(question: QuestionClass): string {
	return CLASSES[question] ?? question;
}

const CHANNELS: Record<Channel, string> = {
	cli: 'Terminal (CLI)',
	web: 'Web client',
	mcp: 'AI agents (MCP)'
};

export function channelLabel(channel: Channel): string {
	return CHANNELS[channel] ?? channel;
}

const JUDGES: Record<JudgeMode | 'none', { label: string; tone: Tone }> = {
	jev: { label: 'Jev', tone: 'good' },
	chat: { label: 'Chat fallback', tone: 'warn' },
	rules: { label: 'Rules only', tone: 'bad' },
	none: { label: 'No judge needed', tone: 'neutral' }
};

export function judgeLabel(mode: JudgeMode | 'none'): { label: string; tone: Tone } {
	return JUDGES[mode] ?? { label: mode, tone: 'neutral' };
}

const DAY_MS = 24 * 60 * 60 * 1000;

/** A token's state as a chip: one that runs out within a week says so. */
export function tokenChip(
	token: AdminToken,
	now: Date = new Date()
): { label: string; tone: Tone } {
	if (token.state === 'revoked') return { label: 'Revoked', tone: 'neutral' };
	if (token.state === 'expired') return { label: 'Expired', tone: 'neutral' };
	const left = new Date(token.expires_at).getTime() - now.getTime();
	if (left < 7 * DAY_MS) return { label: 'Expires soon', tone: 'warn' };
	return { label: 'Active', tone: 'good' };
}

export type ProbeCondition = 'healthy' | 'stale' | 'quarantined' | 'unregistered';

export function probeCondition(probe: AdminProbe | null): ProbeCondition {
	return probe === null ? 'unregistered' : probe.status;
}

export const PROBE_WORDS: Record<ProbeCondition, { label: string; tone: Tone; meaning: string }> = {
	healthy: { label: 'Healthy', tone: 'good', meaning: 'Registered in the last three minutes.' },
	stale: {
		label: 'Stale',
		tone: 'warn',
		meaning: 'Silent for three minutes or more: answers may be out of date.'
	},
	quarantined: {
		label: 'Quarantined',
		tone: 'bad',
		meaning: 'Never called until an admin releases it.'
	},
	unregistered: {
		label: 'Not registered',
		tone: 'neutral',
		meaning: 'Its probe has never registered with the core.'
	}
};

const LIMITS: Record<AccessLimit, string> = {
	token: "the repository token's permissions",
	manifest: "the repository's own manifest",
	access_max: "the server's limit for this project",
	size_cap: 'the size cap',
	no_code_reader: 'no code reader configured'
};

export function limitWords(limit: AccessLimit): string {
	return LIMITS[limit] ?? limit;
}

/**
 * What "no alert holds" vouches for, one sentence per alert, as the core checks each: today's
 * spend, the judge as the questions that ended show it, every project's probe. The judge's check
 * reads only questions that ended, so when no question in the window went through the judge it
 * vouches for nothing, and the words say so rather than that the judge is fine.
 */
export function allClearWords(overview: Pick<AdminOverview, 'judge' | 'spend'>): string[] {
	const judged = overview.judge.jev + overview.judge.chat + overview.judge.rules;
	return [
		overview.spend.daily_budget_usd === null
			? "The daily budget can't be read, so the spend isn't checked."
			: "Today's spend is under 80% of the daily budget.",
		judged === 0
			? 'No question in this window went through the judge, so nothing yet says how it runs.'
			: "The questions that ended in the last 24 hours don't show the judge stuck on a fallback for more than 5 minutes.",
		"Every project's probe has registered and was heard from in the last 15 minutes; a quarantined one isn't counted."
	];
}

/** An alert in one sentence. */
export function alertWords(alert: AdminAlert, describeTime: (iso: string) => string): string {
	switch (alert.kind) {
		case 'spend': {
			const spent = alert.spent_usd ?? 0;
			const budget = alert.budget_usd ?? 0;
			// a budget of 0 is an alert of its own: every question is refused, whatever was spent
			if (budget <= 0) return 'The daily budget is 0, so nothing can be asked.';
			const share = `${Math.floor((spent / budget) * 100)}%`;
			return `Today's spend is at ${share} of the daily budget (${describeCost(spent)} of ${describeCost(budget)}).`;
		}
		case 'judge_degraded': {
			const mode = alert.judge_mode
				? judgeLabel(alert.judge_mode).label.toLowerCase()
				: 'a fallback';
			return `The judge has run on ${mode} since ${alert.since ? describeTime(alert.since) : 'a while'}.`;
		}
		case 'probe_silent':
			return alert.since
				? `The ${alert.project ?? 'project'} probe has been silent since ${describeTime(alert.since)}.`
				: `The ${alert.project ?? 'project'} probe has never registered.`;
	}
}

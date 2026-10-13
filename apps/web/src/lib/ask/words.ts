/**
 * Words for what the core sends as codes: question classes, fact states, who judged. The core's own
 * texts (a refusal's message and hint, a clarify's question) are shown as it wrote them; these are
 * only for the enums the app has to put into a sentence or a chip.
 */

import type {
	AccessMode,
	ComposerMode,
	Evidence,
	FactStatus,
	FactTested,
	JudgeMode,
	PendingSignoff,
	ProbeState,
	QuestionClass
} from '$lib/api';

const CLASSES: Record<QuestionClass, string> = {
	progress: 'what is done',
	tested: 'what is tested',
	blocked: 'what is blocked',
	health: 'build health',
	changes: 'recent changes',
	open: 'anything else about the project',
	portfolio: 'all your projects at once',
	forbidden: 'something Mercury does not answer',
	other: 'something else'
};

/** A question class, as part of a sentence ("what is done"). */
export function classWords(question: QuestionClass): string {
	return CLASSES[question] ?? question;
}

export type Tone = 'neutral' | 'good' | 'warn' | 'bad' | 'info';

const STATUSES: Record<FactStatus, { label: string; tone: Tone }> = {
	done: { label: 'Done', tone: 'good' },
	in_review: { label: 'In review', tone: 'info' },
	in_progress: { label: 'In progress', tone: 'info' },
	blocked: { label: 'Blocked', tone: 'bad' },
	not_started: { label: 'Not started', tone: 'neutral' },
	unclear: { label: 'Unclear', tone: 'warn' }
};

export function statusChip(status: FactStatus): { label: string; tone: Tone } {
	return STATUSES[status] ?? { label: status, tone: 'neutral' };
}

const TESTED: Record<FactTested, { label: string; tone: Tone }> = {
	passing: { label: 'Tests passing', tone: 'good' },
	failing: { label: 'Tests failing', tone: 'bad' },
	missing: { label: 'No tests', tone: 'warn' },
	unknown: { label: 'Tests unknown', tone: 'neutral' }
};

export function testedChip(tested: FactTested): { label: string; tone: Tone } {
	return TESTED[tested] ?? { label: tested, tone: 'neutral' };
}

const WAITING: Record<string, string> = {
	review: 'Waiting for a review',
	dependency: 'Waiting for another item',
	environment: 'Waiting for an environment',
	external: 'Waiting for someone outside the team',
	decision: 'Waiting for a decision',
	unknown: 'Waiting for something'
};

export function waitingWords(waitingOn: string): string {
	return WAITING[waitingOn] ?? 'Waiting for something';
}

/** The share a ratio is, as a whole percentage ("91%"). */
export function percent(ratio: number): string {
	return `${Math.round(ratio * 100)}%`;
}

/** A probability the way the timeline shows it, two decimals ("0.94"). */
export function probability(p: number): string {
	return p.toFixed(2);
}

/** One piece of evidence, short: "PR #412 merged", "CI passed". The core's reference holds its `#`. */
export function evidenceWords(evidence: Evidence): string {
	const type = { pr: 'PR', issue: 'Issue', ci: 'CI', release: 'Release' }[evidence.type];
	return [type, evidence.ref, evidence.state].filter(Boolean).join(' ');
}

const JUDGES: Record<JudgeMode, string> = {
	jev: 'Jev',
	chat: 'the chat model (fallback)',
	rules: 'rules only (degraded)'
};

/** Who judged the question and the answer. */
export function judgeWords(mode: JudgeMode): string {
	return JUDGES[mode] ?? mode;
}

const JUDGE_NAMES: Record<JudgeMode, string> = {
	jev: 'Jev',
	chat: 'chat (fallback)',
	rules: 'rules'
};

/** The judge as the timeline names it: "judge: rules", in the words docs/deploy.md uses. */
export function judgeName(mode: JudgeMode): string {
	return JUDGE_NAMES[mode] ?? mode;
}

const COMPOSERS: Record<ComposerMode, string> = {
	model: 'the model',
	template: 'the template, without a model (degraded)'
};

/** Who wrote the answer. */
export function composerWords(mode: ComposerMode): string {
	return COMPOSERS[mode] ?? mode;
}

const ACCESS: Record<AccessMode, string> = {
	metadata: 'metadata only',
	code: 'code, never shown',
	code_model: 'code with a model, never shown'
};

export function accessWords(mode: AccessMode): string {
	return ACCESS[mode] ?? mode;
}

const TOOLS: Record<string, string> = {
	describe: "the project's description",
	progress: 'progress',
	tested: 'tests',
	blocked: 'blockers',
	health: 'build health',
	changes: 'changes',
	ask_code: 'the code reader'
};

/** What a probe tool looks up, as the short view says it ("tests", "the code reader"). */
export function toolWords(tool: string): string {
	return TOOLS[tool] ?? tool.replaceAll('_', ' ');
}

/** Why an answer waits for an approver. */
/** What an output check's flag on a sentence means, for the approver who reads it. */
const FLAGS: Record<string, string> = {
	sentence_contains_code_or_identifier: 'It may name code or an identifier from the repository.',
	sentence_contains_path_or_secret: 'It may show a file path or a secret.',
	sentence_mentions_client: 'It may name a client.',
	sentence_evaluates_people: 'It may judge a person.',
	sentence_commits_to_client: 'It may promise something to a client.',
	grounded: 'It may not rest on the facts Mercury found.'
};

export function flagWords(check: string): string {
	return FLAGS[check] ?? `A check (${check.replaceAll('_', ' ')}) was not sure about it.`;
}

export function signoffWords(reason: PendingSignoff['reason']): string {
	return reason === 'class'
		? 'Answers of this kind are always read by an approver first.'
		: 'A check was not sure about part of the answer, so an approver reads it first.';
}

/** How fresh a project's data is, when it is not as fresh as it should be. */
export function probeNotice(probe: ProbeState | null, asOf: string): string | null {
	if (probe === null) return "Mercury has not heard from this project's repository yet.";
	if (probe.status === 'stale') {
		return `The data may be out of date: the repository was last read ${asOf}.`;
	}
	if (probe.status === 'quarantined') {
		return "This project's repository reader is switched off until an admin looks at it.";
	}
	return null;
}

const SOURCES = { rule: 'rule', jev: 'Jev', chat: 'chat model (fallback)' } as const;

/** What decided a check: a rule, or the judge that answered. */
export function sourceWords(source: keyof typeof SOURCES): string {
	return SOURCES[source] ?? source;
}

/** The look of each tone, as classes: a dot on the timeline, a chip. */
export const DOT: Record<Tone, string> = {
	neutral: 'bg-border-strong',
	good: 'bg-success',
	warn: 'bg-warning',
	bad: 'bg-danger',
	info: 'bg-info'
};

/** A chip: a dot in the tone's colour on a tint of it, the words in the text colour so they read in both looks. */
export const CHIP: Record<Tone, string> = {
	neutral: 'bg-surface-2 text-text-2',
	good: 'bg-success/12 text-text',
	warn: 'bg-warning/12 text-text',
	bad: 'bg-danger/12 text-text',
	info: 'bg-info/12 text-text'
};

/**
 * One row of the gateway timeline per event of a question's stream. A row is built from the
 * event's numbers, names and codes and the fixed words around them, never from model text: only
 * the `answer` event carries that, and it is shown by the answer card, nowhere else.
 */

import type { CheckResult, JudgeMode, Role, StreamEvent, Verdict } from '$lib/api';
import { describeCost } from '$lib/format';
import { accessWords, classWords, judgeName, toolWords, type Tone } from './words';

/**
 * Whether a role sees the whole trail, folded under the answer, rather than the short view. This is
 * what the screen shows, not an access rule: the core sends every role the same events. The smaller
 * view is the default for any other role, or none known.
 */
export function seesFullTrail(role: Role | undefined): boolean {
	return role === 'tech_lead' || role === 'admin';
}

/** The probe tool that reads the project's code with a model, whose calls the screen marks. */
const CODE_READER = 'ask_code';

/** A verdict as words and a tone, in the full trail and in the short view alike. */
export const VERDICTS: Record<Verdict, { label: string; tone: Tone }> = {
	pass: { label: 'pass', tone: 'good' },
	review: { label: 'needs review', tone: 'warn' },
	block: { label: 'blocked', tone: 'bad' }
};

export interface CheckLine {
	/** The check's id with spaces ("asks for code"). */
	label: string;
	/** What decided it: a rule, or the judge that answered. */
	source: CheckResult['source'];
	p: number | null;
	verdict: Verdict;
}

export interface Row {
	title: string;
	detail: string | null;
	/** The probability the row's decision rests on, when it has one. */
	p: number | null;
	/** Who chose, when the event says. */
	source: string | null;
	checks: CheckLine[];
	tone: Tone;
}

/** A check's id as words: `asks_for_code` → "asks for code". */
export function checkLabel(id: string): string {
	return id.replaceAll('_', ' ');
}

function checkLine(check: CheckResult): CheckLine {
	return {
		label: checkLabel(check.id),
		source: check.source,
		p: check.p ?? null,
		verdict: check.verdict
	};
}

/** The worst verdict among checks, as the tone of the row they sit in. */
function toneOf(checks: CheckLine[]): Tone {
	if (checks.some((c) => c.verdict === 'block')) return 'bad';
	if (checks.some((c) => c.verdict === 'review')) return 'warn';
	return 'neutral';
}

function row(partial: Partial<Row> & Pick<Row, 'title'>): Row {
	return { detail: null, p: null, source: null, checks: [], tone: 'neutral', ...partial };
}

function plural(count: number, one: string, many: string): string {
	return `${count} ${count === 1 ? one : many}`;
}

/** The row for one event. */
export function describeEvent(event: StreamEvent): Row {
	switch (event.event) {
		case 'accepted':
			return row({ title: 'Question received' });
		case 'project':
			return row({
				title: 'Project',
				detail: event.data.project,
				p: event.data.p,
				source: event.data.source === 'explicit' ? 'you chose it' : 'picked by the judge'
			});
		case 'classified': {
			const checks = event.data.checks.map(checkLine);
			return row({
				title: 'Kind of question',
				detail: classWords(event.data.class),
				p: event.data.p,
				checks,
				tone: toneOf(checks)
			});
		}
		case 'plan': {
			const calls = event.data.calls.map((call) => {
				const tool: string = call.tool;
				// what the code reader is asked is the person's own question: never shown
				if (tool === CODE_READER) return `${tool} (your question)`;
				const args = Object.entries(call.args)
					.filter(([, value]) => value !== null && value !== undefined)
					.map(([name, value]) => `${name} ${String(value)}`);
				return args.length ? `${tool} (${args.join(', ')})` : tool;
			});
			return row({ title: 'Plan', detail: calls.join(' · ') || 'nothing to ask' });
		}
		case 'tool_checked': {
			const checks = event.data.checks.map(checkLine);
			return row({ title: `Checked call ${event.data.call + 1}`, checks, tone: toneOf(checks) });
		}
		case 'tool_result': {
			const withheld = event.data.withheld_fields.length;
			const parts = [
				plural(event.data.items, 'item', 'items'),
				withheld > 0 ? `${plural(withheld, 'detail', 'details')} withheld` : null,
				`read as ${accessWords(event.data.access)}`
			];
			return row({
				title: `Repository answered call ${event.data.call + 1}`,
				detail: parts.filter(Boolean).join(' · '),
				p: lowestOfChecks(event.data),
				tone: withheld > 0 ? 'warn' : 'neutral'
			});
		}
		case 'composing':
			return row({ title: 'Writing the answer' });
		case 'judged': {
			const { sentences, stripped, held } = event.data;
			const parts = [
				plural(sentences, 'sentence', 'sentences') + ' checked',
				stripped > 0 ? `${stripped} removed` : null,
				held > 0 ? `${held} held for sign-off` : null
			];
			return row({
				title: 'Answer checked',
				detail: parts.filter(Boolean).join(' · '),
				p: lowestOfChecks(event.data),
				tone: stripped > 0 || held > 0 ? 'warn' : 'good'
			});
		}
		case 'pending_signoff':
			return row({
				title: 'Held for sign-off',
				detail: event.data.reason === 'class' ? 'always read first' : 'a check was not sure',
				tone: 'warn'
			});
		case 'answer':
			// the text is the answer card's, not the timeline's
			return row({ title: 'Answer ready', tone: 'good' });
		case 'done': {
			const { judge_mode, composer, cost_usd } = event.data;
			// a weaker judge or the template is the degraded mode: visible, never silent
			const degraded = judge_mode !== 'jev' || composer === 'template';
			return row({
				title: 'Done',
				detail: [
					`judge: ${judgeName(judge_mode)}`,
					`composer: ${composer}`,
					`cost ${describeCost(cost_usd)}`
				].join(' · '),
				tone: degraded ? 'warn' : 'good'
			});
		}
		case 'refused':
			return row({
				title: 'Refused',
				detail: ending(checkLabel(event.data.code), event.data.cost_usd, event.data.judge_mode),
				tone: 'bad'
			});
		case 'clarify':
			return row({
				title: 'Needs your choice',
				detail: ending(
					event.data.kind === 'project' ? 'which project' : 'which kind of question',
					event.data.cost_usd,
					event.data.judge_mode
				),
				tone: 'warn'
			});
		case 'error':
			return row({
				title: 'Failed',
				detail: ending(checkLabel(event.data.code), event.data.cost_usd, event.data.judge_mode),
				tone: 'bad'
			});
	}
}

/**
 * An ending's detail: what it was, what the question cost up to it, and which judge answered, or
 * that none was asked. Only the core's own events reach here: the transport's report of a lost
 * stream is no row, since it can't know either.
 */
function ending(what: string, cost: number, judge: JudgeMode | null): string {
	return [
		what,
		`cost ${describeCost(cost)}`,
		judge ? `judge: ${judgeName(judge)}` : 'no judge asked'
	].join(' · ');
}

/** Whether a question's plan called the code reader, so part of its answer comes from the code. */
export function callsTheCodeReader(events: StreamEvent[]): boolean {
	return events.some(
		(event) =>
			event.event === 'plan' &&
			event.data.calls.some((call) => (call.tool as string) === CODE_READER)
	);
}

/** The stages of a question the short view has a row for, in the order it runs them. */
export type Stage = 'question' | 'lookup' | 'data' | 'answer';

/** One stage of a question as the short view shows it. */
export interface StageRow {
	stage: Stage;
	/** Whether the question got as far as this stage. */
	reached: boolean;
	/** The strictest verdict of the stage's checks; none for a stage not reached. */
	verdict: Verdict | null;
	/** The lowest probability that the stage was fine; none where a rule decided, or not reached. */
	p: number | null;
	/** What the stage did, in fixed words and numbers; never anything an event says. */
	detail: string | null;
}

const STRICTNESS: Verdict[] = ['pass', 'review', 'block'];

function strictest(verdicts: Verdict[]): Verdict {
	return verdicts.reduce<Verdict>(
		(worst, verdict) => (STRICTNESS.indexOf(verdict) > STRICTNESS.indexOf(worst) ? verdict : worst),
		'pass'
	);
}

function lowest(values: (number | null | undefined)[]): number | null {
	const present = values.filter((value): value is number => typeof value === 'number');
	return present.length > 0 ? Math.min(...present) : null;
}

/**
 * The checks of the lookup whose probability is the chance the call is fine (high is good), the two
 * Jev answers about its arguments. The input checks' own probability is a risk (high is bad), and a
 * check added later may be either, so the row names the ones it reads rather than all.
 */
const LEGIT_CHECKS = ['args_match_question', 'args_free_of_instructions'];

/** The lowest probability of a stage's checks that a judge answered; left out where rules decided. */
function lowestOfChecks(data: object): number | null {
	return 'p_min' in data && typeof data.p_min === 'number' ? data.p_min : null;
}

/**
 * The short view of a question: always four rows, one per stage (the question, the lookup, the data,
 * the answer), each with its strictest verdict and its lowest probability. Built from event names,
 * codes and numbers only, never from an event's text, so it is as safe to show as the full trail.
 */
export function summarizeStages(events: StreamEvent[]): StageRow[] {
	const only = <N extends StreamEvent['event']>(name: N) =>
		events.filter((event): event is Extract<StreamEvent, { event: N }> => event.event === name);

	const [project] = only('project');
	const [classified] = only('classified');
	const [clarify] = only('clarify');
	const [plan] = only('plan');
	const [judged] = only('judged');
	const results = only('tool_result');
	const checked = only('tool_checked').flatMap((event) => event.data.checks);
	const held = only('pending_signoff').length > 0;

	const planned = plan !== undefined;
	const gotData = results.length > 0;
	const gotAnswer = judged !== undefined || held || only('answer').length > 0;

	// a refusal ended the stage the question had reached
	const refusedAt: Stage | null =
		only('refused').length === 0
			? null
			: gotAnswer
				? 'answer'
				: gotData
					? 'data'
					: planned
						? 'lookup'
						: 'question';

	const notReached = (stage: Stage): StageRow => ({
		stage,
		reached: false,
		verdict: null,
		p: null,
		detail: null
	});

	let question = notReached('question');
	if (project || classified || clarify || refusedAt === 'question') {
		const verdicts = classified?.data.checks.map((check) => check.verdict) ?? [];
		question = {
			stage: 'question',
			reached: true,
			verdict:
				refusedAt === 'question'
					? 'block'
					: strictest(clarify ? [...verdicts, 'review'] : verdicts),
			// both are the chance the pick is right (high is good); the input checks' own are not read
			p: lowest([project?.data.p, classified?.data.p]),
			detail: clarify ? 'needs your choice' : null
		};
	}

	let lookup = notReached('lookup');
	if (planned) {
		const tools = [...new Set(plan.data.calls.map((call) => toolWords(call.tool)))];
		lookup = {
			stage: 'lookup',
			reached: true,
			verdict: refusedAt === 'lookup' ? 'block' : strictest(checked.map((check) => check.verdict)),
			p: lowest(checked.filter((check) => LEGIT_CHECKS.includes(check.id)).map((check) => check.p)),
			detail: tools.length > 0 ? tools.join(' · ') : 'nothing to look up'
		};
	}

	let data = notReached('data');
	if (gotData) {
		const withheld = results.reduce((sum, result) => sum + result.data.withheld_fields.length, 0);
		const items = results.reduce((sum, result) => sum + result.data.items, 0);
		const answered = new Set(results.map((result) => result.data.call)).size;
		const asked = plan?.data.calls.length ?? 0;
		data = {
			stage: 'data',
			reached: true,
			verdict: refusedAt === 'data' ? 'block' : withheld > 0 ? 'review' : 'pass',
			p: lowest(results.map((result) => lowestOfChecks(result.data))),
			detail: [
				plural(items, 'item', 'items'),
				withheld > 0 ? `${withheld} withheld` : null,
				answered < asked ? `${answered} of ${asked} calls answered` : null
			]
				.filter(Boolean)
				.join(' · ')
		};
	}

	let answer = notReached('answer');
	if (gotAnswer) {
		const kept = judged ? judged.data.sentences - judged.data.stripped : 0;
		const unsettled = judged ? judged.data.stripped > 0 || judged.data.held > 0 : false;
		answer = {
			stage: 'answer',
			reached: true,
			verdict: refusedAt === 'answer' ? 'block' : unsettled || held ? 'review' : 'pass',
			p: judged ? lowestOfChecks(judged.data) : null,
			detail:
				[
					judged
						? `${kept} of ${plural(judged.data.sentences, 'sentence', 'sentences')} kept`
						: null,
					held ? 'held for sign-off' : null
				]
					.filter(Boolean)
					.join(' · ') || null
		};
	}

	return [question, lookup, data, answer];
}

const STAGE_WORDS: Record<Stage, string> = {
	question: 'the question',
	lookup: 'the lookup',
	data: 'the data',
	answer: 'the answer'
};

/**
 * How the four stages went, in one line over the short view and on the folded trail: the stage that
 * stopped the question, how many need a look, or that all passed.
 */
export function stagesInOneLine(rows: StageRow[]): { tone: Tone; words: string } {
	const blocked = rows.find((row) => row.verdict === 'block');
	if (blocked) return { tone: 'bad', words: `Blocked at ${STAGE_WORDS[blocked.stage]}` };
	const review = rows.filter((row) => row.verdict === 'review').length;
	if (review > 0) {
		return {
			tone: 'warn',
			words: `${review} ${review === 1 ? 'stage needs' : 'stages need'} review`
		};
	}
	const passed = rows.filter((row) => row.verdict === 'pass').length;
	return passed === rows.length
		? { tone: 'good', words: 'All four stages passed' }
		: { tone: 'neutral', words: `${passed} of ${rows.length} stages passed` };
}

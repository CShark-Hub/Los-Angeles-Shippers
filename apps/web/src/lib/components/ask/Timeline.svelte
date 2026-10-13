<script lang="ts">
	import type { StreamEvent } from '$lib/api';
	import type { Entry } from '$lib/ask/conversation.svelte';
	import { describeEvent, VERDICTS, type Row } from '$lib/ask/timeline';
	import { DOT, probability, sourceWords, type Tone } from '$lib/ask/words';
	import Icon from '$lib/components/Icon.svelte';

	import Chip from './Chip.svelte';

	let { entries, running }: { entries: Entry[]; running: boolean } = $props();

	/** The stage an event belongs to, named as the short view names it; the ending stands apart. */
	type Part = 'question' | 'lookup' | 'data' | 'answer' | 'end';
	const PART: Record<StreamEvent['event'], Part> = {
		accepted: 'question',
		project: 'question',
		classified: 'question',
		plan: 'lookup',
		tool_checked: 'lookup',
		tool_result: 'data',
		composing: 'answer',
		judged: 'answer',
		pending_signoff: 'answer',
		answer: 'answer',
		refused: 'end',
		clarify: 'end',
		done: 'end',
		error: 'end'
	};
	const NAMES: Record<Part, string> = {
		question: 'The question',
		lookup: 'The lookup',
		data: 'The data',
		answer: 'The answer',
		end: ''
	};

	interface Group {
		part: Part;
		/** The index of its first event: a group keeps its place as events arrive. */
		start: number;
		rows: (Row & { at: number | null })[];
	}

	// Events in the order they came, gathered by stage: a run of events of one stage is one group.
	const groups = $derived.by(() => {
		const out: Group[] = [];
		entries.forEach((entry, index) => {
			const part = PART[entry.event.event] ?? 'end';
			const row = { ...describeEvent(entry.event), at: entry.at };
			const last = out.at(-1);
			if (last && last.part === part) last.rows.push(row);
			else out.push({ part, start: index, rows: [row] });
		});
		return out;
	});

	// The ending's mark: a disc with the sign of how it ended.
	const END: Record<Tone, { disc: string; icon: 'check' | 'exclamation' | 'close' | 'info-mark' }> =
		{
			good: { disc: 'bg-success text-surface', icon: 'check' },
			warn: { disc: 'bg-warning text-surface', icon: 'exclamation' },
			bad: { disc: 'bg-danger text-surface', icon: 'close' },
			info: { disc: 'bg-info text-surface', icon: 'info-mark' },
			neutral: { disc: 'bg-border-strong text-surface', icon: 'check' }
		};

	function seconds(ms: number): string {
		return `+${(ms / 1000).toFixed(1)} s`;
	}

	// Every line of the trail, an event's or a check's, on the same columns: the mark, what happened
	// (on a measure that keeps the figures near it), a check's verdict, the probability, and, at the far
	// edge, the time from the question's start.
	const COLUMNS =
		'grid grid-cols-[1rem_minmax(0,26rem)_6rem_3.25rem_minmax(0,1fr)_3.25rem] items-baseline gap-x-3 max-sm:grid-cols-[1rem_minmax(0,1fr)_auto_auto_0_auto]';
</script>

{#snippet probabilityOf(p: number | null)}
	{#if p !== null}
		<span class="text-right font-mono text-2xs text-text-3 tabular-nums" title="probability"
			>p {probability(p)}</span
		>
	{:else}
		<span></span>
	{/if}
{/snippet}

<!-- One row per event as it arrives, gathered under the stage it belongs to; a screen reader hears each
     addition once. Each row reads what happened, its detail and probability, and when, from the
     question's start; a check reads its verdict in a column of its own. -->
<ol
	class="flex flex-col"
	aria-label="What Mercury did with the question"
	aria-live="polite"
	aria-relevant="additions"
>
	{#each groups as group (group.start)}
		<li
			class="grid gap-x-4 gap-y-1 border-t border-border-subtle py-2.5 first:border-t-0 first:pt-1 last:pb-1 sm:grid-cols-[6.5rem_minmax(0,1fr)]"
		>
			{#if group.part === 'end'}
				<span class="hidden sm:block"></span>
			{:else}
				<p class="pt-px text-xs font-medium text-text-3">{NAMES[group.part]}</p>
			{/if}
			<ol class="flex min-w-0 flex-col gap-1.5">
				{#each group.rows as row, i (i)}
					{@const ending = group.part === 'end'}
					<li class="flex flex-col gap-1">
						<div class="{COLUMNS} text-compact">
							<!-- the mark: a hollow ring for a step, a filled dot where it says how it went, a
							     disc with its sign for the ending -->
							<span aria-hidden="true" class="flex h-[1lh] items-center justify-center self-start">
								{#if ending}
									<span class="grid size-4 place-items-center rounded-full {END[row.tone].disc}"
										><Icon name={END[row.tone].icon} size={10} weight={3.2} /></span
									>
								{:else if row.tone === 'neutral'}
									<span class="size-2 rounded-full ring-[1.5px] ring-border-strong ring-inset"
									></span>
								{:else}
									<span class="size-2 rounded-full {DOT[row.tone]}"></span>
								{/if}
							</span>
							<p class="min-w-0">
								<span class="{ending ? 'font-semibold' : 'font-medium'} text-text">{row.title}</span
								>
								{#if row.detail}<span class="ml-1.5 text-text-2">{row.detail}</span>{/if}
								{#if row.source}<span class="ml-1.5 text-xs text-text-3">{row.source}</span>{/if}
							</p>
							<span></span>
							{@render probabilityOf(row.p)}
							{#if row.at !== null}
								<span class="col-start-6 text-right text-2xs text-text-3 tabular-nums"
									>{seconds(row.at)}</span
								>
							{/if}
						</div>
						{#if row.checks.length}
							<ul class="flex flex-col">
								{#each row.checks as check, c (c)}
									{@const verdict = VERDICTS[check.verdict]}
									<li class="{COLUMNS} text-xs">
										<span></span>
										<span class="min-w-0 border-l border-border-control py-0.5 pl-3">
											<span class="text-text-2">{check.label}</span>
											<span class="ml-1.5 text-text-3">{sourceWords(check.source)}</span>
										</span>
										<span class="justify-self-end">
											{#if check.verdict === 'pass'}
												<span
													class="inline-flex items-center gap-1.5 text-2xs font-medium text-text-3"
													><span
														class="size-1.5 rounded-full {DOT[verdict.tone]}"
														aria-hidden="true"
													></span>{verdict.label}</span
												>
											{:else}
												<Chip tone={verdict.tone}>{verdict.label}</Chip>
											{/if}
										</span>
										{@render probabilityOf(check.p)}
									</li>
								{/each}
							</ul>
						{/if}
					</li>
				{/each}
			</ol>
		</li>
	{/each}
	{#if running}
		<li
			class="grid gap-x-4 border-t border-border-subtle py-2.5 first:border-t-0 first:pt-1 sm:grid-cols-[6.5rem_minmax(0,1fr)]"
		>
			<span class="hidden sm:block"></span>
			<div class="{COLUMNS} text-compact">
				<span aria-hidden="true" class="flex h-[1lh] items-center justify-center">
					<span
						class="size-2 rounded-full bg-accent motion-safe:animate-pulse motion-reduce:ring-3 motion-reduce:ring-accent/30"
					></span>
				</span>
				<span class="text-text-3">Working…</span>
			</div>
		</li>
	{/if}
</ol>

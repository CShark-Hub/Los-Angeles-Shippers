<script lang="ts">
	import type { Entry } from '$lib/ask/conversation.svelte';
	import { stagesInOneLine, summarizeStages, VERDICTS, type Stage } from '$lib/ask/timeline';
	import { probability } from '$lib/ask/words';
	import Icon from '$lib/components/Icon.svelte';

	import Chip from './Chip.svelte';

	/**
	 * What the gateway did with a question, for the person who doesn't read the whole trail: one row
	 * per stage with its verdict and its lowest probability. Open, with nothing in it to operate.
	 */
	let { entries }: { entries: Entry[] } = $props();

	const NAMES: Record<Stage, string> = {
		question: 'The question',
		lookup: 'The lookup',
		data: 'The data',
		answer: 'The answer'
	};

	const rows = $derived(summarizeStages(entries.map((entry) => entry.event)));

	// Each step's mark: the verdict's colour and sign on a disc, or an empty ring where it wasn't reached.
	const MARK = {
		pass: { disc: 'bg-success text-surface', icon: 'check' },
		review: { disc: 'bg-warning text-surface', icon: 'exclamation' },
		block: { disc: 'bg-danger text-surface', icon: 'close' }
	} as const;

	// A stage that held the question up is washed in its tone, so the eye lands on it first.
	const WASH = {
		pass: 'bg-surface',
		review: 'bg-[color-mix(in_oklab,var(--warning)_8%,var(--surface))]',
		block: 'bg-[color-mix(in_oklab,var(--danger)_8%,var(--surface))]'
	} as const;

	// One line over the four, from the same rows: what stopped it, what needs a look, or that all passed.
	const summary = $derived(stagesInOneLine(rows));
</script>

<!-- A strip of four under the answer (or the notice) it describes: each stage with its verdict, the
     probability it rests on, and what it did; the stage that held the question up is washed in its tone. -->
<section
	aria-labelledby="short-view-heading"
	class="overflow-hidden rounded-card border border-border bg-surface"
>
	<div class="flex items-center justify-between gap-3 border-b border-border-subtle px-4 py-2">
		<h2 id="short-view-heading" class="flex items-center gap-2 text-xs font-medium text-text-2">
			<span class="text-text-3"><Icon name="shield" size={14} /></span>How Mercury checked this
		</h2>
		<Chip tone={summary.tone}>{summary.words}</Chip>
	</div>
	<ol
		aria-label="How each stage went"
		class="grid gap-px bg-border-subtle sm:grid-cols-2 xl:grid-cols-4"
	>
		{#each rows as row (row.stage)}
			<!-- read in order: the stage, its verdict, the probability it rests on, what it did; drawn as
			     the stage with its figure at the right, and the verdict and what it did under them -->
			<li
				class="grid min-w-0 grid-cols-[1.75rem_auto_minmax(0,1fr)_auto] content-start gap-y-0.5 px-4 py-2.5 {row.verdict
					? WASH[row.verdict]
					: 'bg-surface'}"
			>
				<span class="row-span-2 flex h-[1lh] items-center text-compact" aria-hidden="true">
					{#if row.verdict}
						<span class="grid size-4.5 place-items-center rounded-full {MARK[row.verdict].disc}"
							><Icon name={MARK[row.verdict].icon} size={11} weight={3} /></span
						>
					{:else}
						<span class="size-4.5 rounded-full border-[1.5px] border-dashed border-border-strong"
						></span>
					{/if}
				</span>
				<span
					class="col-span-2 col-start-2 row-start-1 truncate text-compact font-medium {row.reached
						? 'text-text'
						: 'text-text-3'}">{NAMES[row.stage]}</span
				>
				<span
					class="col-start-2 row-start-2 text-xs whitespace-nowrap {row.verdict
						? 'font-medium text-text-2'
						: 'text-text-3'}">{row.verdict ? VERDICTS[row.verdict].label : 'not reached'}</span
				>
				{#if row.p !== null}
					<span
						class="col-start-4 row-start-1 self-center pl-3 font-mono text-xs text-text-2 tabular-nums"
						title="probability"><span class="text-text-3">p</span> {probability(row.p)}</span
					>
				{/if}
				{#if row.detail}
					<span
						class="relative col-span-2 col-start-3 row-start-2 min-w-0 pl-3.5 text-xs text-text-3 before:absolute before:left-1.5 before:content-['·']"
						>{row.detail}</span
					>
				{/if}
			</li>
		{/each}
	</ol>
</section>

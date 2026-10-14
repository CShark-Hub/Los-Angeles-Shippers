<script lang="ts">
	import type { OutcomeCounts } from '$lib/api';
	import { percentOf } from '$lib/admin/stats';
	import { DOT, type Tone } from '$lib/ask/words';
	import OutcomeBar from './OutcomeBar.svelte';

	// A ranking: each row's label, its count and share, and a slim bar of its size against the
	// largest. One colour for every bar: the length is the message, not the hue. Every list and
	// meter of the overview is drawn from this one row, so they read as one family: a row may carry
	// a tone (a dot before its label, the bar in the tone's colour), its questions by outcome (the bar
	// split in the chart's colours), or another figure in place of the share (a cost).
	type Row = {
		key: string;
		label: string;
		count: number;
		hint?: string;
		/** a dot before the label and the bar in the tone's colour */
		tone?: Tone;
		/** the row's questions by outcome: the bar is split in the chart's colours */
		counts?: OutcomeCounts;
		/** a figure shown in place of the share, such as a cost */
		aside?: string;
		/** the label is a name from the configuration (a project): set in monospace */
		mono?: boolean;
		/** the label stands for "none of them": quieter than the others */
		muted?: boolean;
	};

	let {
		rows,
		total,
		empty = 'Nothing in this window.',
		color = 'var(--bar)',
		max: scale,
		wide = false
	}: {
		rows: readonly Row[];
		total: number;
		empty?: string;
		color?: string;
		/** what a full bar stands for; the largest row by default. Given, the rows show even at 0. */
		max?: number;
		/** two columns, ranked down then across, when the card is wide enough for them */
		wide?: boolean;
	} = $props();

	const max = $derived(scale ?? rows.reduce((m, r) => Math.max(m, r.count), 0));
	const blank = $derived(rows.length === 0 || (scale === undefined && max === 0));
	// two columns are filled down first, so the ranking still reads from the top
	const half = $derived(Math.ceil(rows.length / 2));
</script>

{#if blank}
	<p class="py-2 text-xs/normal text-text-3">{empty}</p>
{:else}
	<ul
		class="grid gap-x-6 gap-y-2.5 {wide && rows.length > 3
			? '@sm:grid-flow-col @sm:grid-cols-2 @sm:grid-rows-(--rows)'
			: ''}"
		style="--rows: repeat({half}, auto)"
	>
		{#each rows as row (row.key)}
			<li class="flex min-w-0 flex-col gap-1.5" title={row.hint}>
				<div class="flex items-baseline justify-between gap-3 text-xs/normal">
					<span class="flex min-w-0 items-center gap-1.5">
						{#if row.tone}<span
								class="size-1.5 shrink-0 rounded-full {DOT[row.tone]}"
								aria-hidden="true"
							></span>{/if}
						<span
							class="truncate {row.mono ? 'font-mono text-2xs' : ''} {row.muted
								? 'text-text-2'
								: 'text-text'}"
							title={row.hint ? undefined : row.label}>{row.label}</span
						>
					</span>
					<span class="flex shrink-0 items-baseline gap-2 tabular-nums">
						<span class="font-semibold text-text">{row.count}</span>
						<span class="min-w-8 text-right text-text-3"
							>{row.aside ?? percentOf(row.count, total)}</span
						>
					</span>
				</div>
				{#if row.counts}
					<OutcomeBar counts={row.counts} {max} />
				{:else}
					<div class="h-1.5 w-full overflow-hidden rounded-full bg-surface-3/70" aria-hidden="true">
						<div
							class="h-full rounded-full {row.tone ? DOT[row.tone] : ''}"
							style="width: {max > 0 ? (row.count / max) * 100 : 0}%; min-width: {row.count > 0
								? 4
								: 0}px{row.tone ? '' : `; background: ${color}`}"
						></div>
					</div>
				{/if}
			</li>
		{/each}
	</ul>
{/if}

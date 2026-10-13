<script lang="ts">
	import type { OutcomeCounts } from '$lib/api';
	import { percentOf, total } from '$lib/admin/stats';
	import { OUTCOME_COLOR, OUTCOME_WORDS, OUTCOMES } from '$lib/admin/words';

	// One row's questions as a single bar split by outcome, in the chart's colours and order, its
	// length the row's share of the largest row; the words say the same for a reader.
	let { counts, max }: { counts: OutcomeCounts; max: number } = $props();

	const all = $derived(total(counts));
	const parts = $derived(OUTCOMES.filter((o) => counts[o] > 0));
	const words = $derived(
		parts.map((o) => `${counts[o]} ${OUTCOME_WORDS[o].label.toLowerCase()}`).join(', ')
	);
</script>

<div
	class="h-1.5 w-full rounded-full bg-surface-3/70"
	role="img"
	aria-label={all === 0 ? 'No questions' : words}
>
	<div
		class="flex h-full gap-px overflow-hidden rounded-full"
		style="width: {max > 0 ? (all / max) * 100 : 0}%"
	>
		{#each parts as outcome (outcome)}
			<span
				class="h-full"
				style="flex: {counts[outcome]} 0 0; background: {OUTCOME_COLOR[outcome]}; min-width: 3px"
				title="{OUTCOME_WORDS[outcome].label}: {counts[outcome]} ({percentOf(
					counts[outcome],
					all
				)})"
			></span>
		{/each}
	</div>
</div>

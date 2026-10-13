<script lang="ts">
	import { percentOf } from '$lib/admin/stats';
	import { DOT, type Tone } from '$lib/ask/words';
	import { describeCost } from '$lib/format';

	// What was spent against a cap, as a bar that turns amber at 80% and red at the cap: the core's
	// own spend alert fires at 80%, marked on the track. Without a cap (the file can't be read) it
	// says so. The lead meter of a card (`primary`) states the spend as a figure, with its share of
	// the cap, over a thicker bar; the others are rows of the same family as the overview's lists.
	let {
		spent,
		cap,
		label,
		primary = false
	}: { spent: number; cap: number | null; label?: string; primary?: boolean } = $props();

	const ratio = $derived(cap && cap > 0 ? spent / cap : 0);
	const tone = $derived<Tone>(ratio >= 1 ? 'bad' : ratio >= 0.8 ? 'warn' : 'info');
</script>

<div class="flex flex-col {primary ? 'gap-2' : 'gap-1.5'}">
	{#if primary}
		<div class="flex flex-col gap-0.5">
			<div class="flex items-baseline justify-between gap-3 text-xs/normal">
				<span class="truncate font-medium text-text-2">{label ?? 'Spent today'}</span>
				{#if cap !== null && cap > 0}<span class="shrink-0 text-text-3 tabular-nums"
						>{percentOf(spent, cap)}</span
					>{/if}
			</div>
			<p class="flex items-baseline gap-1.5 tabular-nums">
				<span class="text-figure font-semibold tracking-tight text-text">{describeCost(spent)}</span
				>
				{#if cap !== null}<span class="text-xs text-text-3">of {describeCost(cap)}</span>{/if}
			</p>
		</div>
	{:else}
		<div class="flex items-baseline justify-between gap-3 text-xs/normal">
			<span class="truncate text-text">{label ?? 'Spent today'}</span>
			<span class="shrink-0 text-text-3 tabular-nums"
				><span class="font-semibold text-text">{describeCost(spent)}</span>
				{#if cap !== null}of {describeCost(cap)}{/if}</span
			>
		</div>
	{/if}
	<div
		class="relative w-full rounded-full bg-surface-3/70 {primary ? 'h-2' : 'h-1.5'}"
		role="meter"
		aria-label={label ?? 'Spent today'}
		aria-valuemin="0"
		aria-valuemax={cap ?? 0}
		aria-valuenow={spent}
	>
		<div
			class="h-full rounded-full {DOT[tone]}"
			style="width: {Math.min(100, ratio * 100)}%; min-width: {spent > 0 ? 4 : 0}px"
		></div>
		{#if cap !== null && cap > 0}
			<!-- where the core's spend alert fires -->
			<span
				class="absolute -inset-y-0.5 left-4/5 w-px rounded-full bg-border-strong/60"
				aria-hidden="true"
			></span>
		{/if}
	</div>
</div>

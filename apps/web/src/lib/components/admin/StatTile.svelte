<script lang="ts">
	import type { Snippet } from 'svelte';

	import { DOT, type Tone } from '$lib/ask/words';
	import Sparkline from './Sparkline.svelte';

	// One figure of the overview: what it counts, the number, and a line that puts it in context. A
	// tone marks a figure that needs a look: a dot beside the label and a faint wash of the tone over
	// the tile, never the number's colour. A trend runs beside the number, filling the room the figure
	// leaves. The lead figure (`primary`) is set larger, with a taller trend, so the row reads from it.
	// The tile is one cell of a band of figures; the band draws the card around them.
	let {
		label,
		value,
		detail,
		tone,
		trend,
		primary = false,
		children
	}: {
		label: string;
		value: string;
		detail?: string;
		tone?: Tone;
		trend?: readonly number[];
		primary?: boolean;
		children?: Snippet;
	} = $props();

	const marked = $derived(tone !== undefined && tone !== 'neutral');
	const WASH: Record<Tone, string> = {
		neutral: '',
		good: 'bg-linear-to-b from-success/8 to-transparent to-70%',
		warn: 'bg-linear-to-b from-warning/9 to-transparent to-70%',
		bad: 'bg-linear-to-b from-danger/9 to-transparent to-70%',
		info: 'bg-linear-to-b from-info/8 to-transparent to-70%'
	};
</script>

<div class="flex min-w-0 flex-col bg-surface px-4 pt-3.5 pb-3.5 {tone ? WASH[tone] : ''}">
	<div class="flex items-center gap-1.5 text-xs font-medium text-text-2">
		{#if marked && tone}<span class="size-1.5 shrink-0 rounded-full {DOT[tone]}" aria-hidden="true"
			></span>{/if}
		<span class="truncate">{label}</span>
	</div>
	<!-- the figures of a band share one baseline, whatever their size -->
	<div class="mt-2 flex min-h-9 items-end gap-4">
		<span
			class="shrink-0 font-semibold tracking-tight text-text tabular-nums {primary
				? 'text-3xl leading-9'
				: 'text-figure'}">{value}</span
		>
		{#if trend && trend.length > 1}<div class="mb-1 min-w-0 flex-1">
				<Sparkline values={trend} height={primary ? 34 : 26} />
			</div>{/if}
	</div>
	{#if detail}<p class="mt-1 text-2xs leading-4 {marked ? 'text-text-2' : 'text-text-3'}">
			{detail}
		</p>{/if}
	{@render children?.()}
</div>

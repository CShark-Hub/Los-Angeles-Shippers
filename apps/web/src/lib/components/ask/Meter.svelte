<script lang="ts">
	import { DOT, percent, type Tone } from '$lib/ask/words';

	// How sure something is, as a short bar and its number: low reads at a glance, before the text.
	let { label = 'Confidence', value }: { label?: string; value: number } = $props();

	const tone = $derived<Tone>(value >= 0.8 ? 'good' : value >= 0.6 ? 'warn' : 'bad');
</script>

<span class="inline-flex items-center gap-2 text-xs text-text-2">
	{label}
	<span class="h-1.5 w-12 overflow-hidden rounded-full bg-surface-3" aria-hidden="true">
		<span class="block h-full rounded-full {DOT[tone]}" style="width: {Math.round(value * 100)}%"
		></span>
	</span>
	<span class="font-semibold text-text tabular-nums">{percent(value)}</span>
</span>

<script lang="ts">
	import type { Snippet } from 'svelte';

	import { CHIP, DOT, type Tone } from '$lib/ask/words';

	let { tone = 'neutral', children }: { tone?: Tone; children: Snippet } = $props();

	// A hairline in the tone's colour inside the tint, so a chip keeps its edge on an inset too.
	const EDGE: Record<Tone, string> = {
		neutral: 'ring-border',
		good: 'ring-success/25',
		warn: 'ring-warning/30',
		bad: 'ring-danger/30',
		info: 'ring-info/30'
	};
</script>

<span
	class="inline-flex h-5 items-center gap-1.5 rounded-chip px-1.5 text-2xs leading-none font-medium whitespace-nowrap ring-1 ring-inset {CHIP[
		tone
	]} {EDGE[tone]}"
>
	{#if tone !== 'neutral'}<span
			class="size-1.5 shrink-0 rounded-full {DOT[tone]}"
			aria-hidden="true"
		></span>{/if}
	{@render children()}
</span>

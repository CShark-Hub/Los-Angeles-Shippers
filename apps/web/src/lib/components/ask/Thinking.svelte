<script lang="ts">
	import type { Entry, Phase } from '$lib/ask/conversation.svelte';
	import { stageOf } from '$lib/ask/loader';
	import Icon from '$lib/components/Icon.svelte';

	let { entries, phase }: { entries: Entry[]; phase: Phase } = $props();

	// One line on how far it got, from the last event's name: never anything an event says.
	const stage = $derived(stageOf(entries, phase));

	// The four stages the short view and the trail name, and the one the last event's name belongs to.
	const STEPS = ['The question', 'The lookup', 'The data', 'The answer'];
	const STEP_OF: Record<string, number> = {
		accepted: 0,
		project: 0,
		classified: 0,
		plan: 1,
		tool_checked: 1,
		tool_result: 2,
		composing: 3,
		judged: 3,
		pending_signoff: 3,
		answer: 3
	};
	const current = $derived(STEP_OF[entries.at(-1)?.event.event ?? ''] ?? 0);
</script>

<!-- One line while the question runs: where it got to in words, and among the four stages. The
     stages are drawn for the eye; the words are what a screen reader hears. -->
<div
	class="flex min-h-11 items-center gap-x-6 gap-y-2 rounded-card border border-border bg-surface py-2.5 pr-4 pl-4 shadow-card"
>
	<p role="status" aria-live="polite" class="shimmer min-w-0 flex-1 text-compact font-medium">
		{stage}…
	</p>
	<div aria-hidden="true" class="hidden shrink-0 items-center gap-2 md:flex">
		{#each STEPS as name, i (name)}
			{#if i > 0}<span
					class="h-px w-4 transition-colors duration-300 {i <= current
						? 'bg-accent/50'
						: 'bg-border-control'}"
				></span>{/if}
			<span
				class="flex items-center gap-1.5 text-2xs transition-colors duration-300 {i < current
					? 'text-text-2'
					: i === current
						? 'font-medium text-text'
						: 'text-text-3'}"
			>
				{#if i < current}
					<span class="grid size-3.5 place-items-center rounded-full bg-accent text-on-accent"
						><Icon name="check" size={9} weight={3.4} /></span
					>
				{:else if i === current}
					<span
						class="size-3.5 rounded-full border-2 border-accent/25 border-t-accent motion-safe:animate-spin"
					></span>
				{:else}
					<span class="size-3.5 rounded-full border-[1.5px] border-border-control"></span>
				{/if}
				{name}
			</span>
		{/each}
	</div>
</div>

<style>
	.shimmer {
		color: var(--text-2);
		background: linear-gradient(
			90deg,
			var(--text-2) 0%,
			var(--text) 40%,
			var(--accent-text) 50%,
			var(--text) 60%,
			var(--text-2) 100%
		);
		background-size: 200% 100%;
		-webkit-background-clip: text;
		background-clip: text;
		-webkit-text-fill-color: transparent;
		animation: shimmer 2.4s linear infinite;
	}
	@keyframes shimmer {
		from {
			background-position: 200% 0;
		}
		to {
			background-position: -200% 0;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.shimmer {
			animation: none;
			-webkit-text-fill-color: var(--text);
		}
	}
</style>

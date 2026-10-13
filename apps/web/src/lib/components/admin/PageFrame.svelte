<script lang="ts">
	import type { ComponentProps, Snippet } from 'svelte';

	import Icon from '$lib/components/Icon.svelte';

	// A whole admin screen in one panel: its name, one line about it, what it counts at a glance and
	// its controls in the panel's head, level with the name; everything the screen shows in its body,
	// on a ground a step quieter than the cards it holds. In a page's `main` that is a flex column at
	// least the window's height, a short screen's panel still reaches the bottom of the window.
	let {
		title,
		description,
		icon,
		summary,
		actions,
		children
	}: {
		title: string;
		description: string;
		icon?: ComponentProps<typeof Icon>['name'];
		summary?: Snippet;
		actions?: Snippet;
		children: Snippet;
	} = $props();
</script>

<section
	class="flex min-w-0 flex-1 flex-col overflow-clip rounded-panel border border-border bg-surface shadow-card"
>
	<header
		class="flex flex-wrap items-start justify-between gap-x-6 gap-y-3 border-b border-border-subtle px-5 pt-5 pb-4"
	>
		<div class="flex min-w-0 flex-1 basis-96 gap-3.5">
			{#if icon}
				<span
					aria-hidden="true"
					class="mt-0.5 grid size-9 shrink-0 place-items-center rounded-control bg-accent-soft text-accent-text ring-1 ring-accent/20 ring-inset"
					><Icon name={icon} size={18} /></span
				>
			{/if}
			<div class="flex min-w-0 flex-col gap-1">
				<h1 class="text-xl font-semibold tracking-tight text-text">
					{title}
				</h1>
				<p class="max-w-3xl text-compact leading-5 text-text-2">{description}</p>
				{#if summary}<div class="mt-1.5 flex flex-wrap items-center gap-1.5">
						{@render summary()}
					</div>{/if}
			</div>
		</div>
		{#if actions}<div class="flex flex-wrap items-center gap-2">{@render actions()}</div>{/if}
	</header>
	<div class="body flex min-w-0 flex-1 flex-col gap-4 p-5">
		{@render children()}
	</div>
</section>

<style>
	/* The body's ground: most of the way to the page from a card, so the cards in it stand forward. */
	.body {
		background: color-mix(in oklab, var(--bg) 70%, var(--surface));
	}
</style>

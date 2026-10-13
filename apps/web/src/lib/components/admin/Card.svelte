<script lang="ts">
	import type { Snippet } from 'svelte';

	// A panel's section: a title with an optional line under it and actions on the right, then what it
	// shows. Dense on purpose: the admin reads many of these side by side, so every card keeps the same
	// header and the same inner rhythm. Cards in a row stretch to the tallest; what they show fills the
	// height (a chart, figure tiles) or ends in a footnote held at the foot.
	let {
		title,
		subtitle,
		actions,
		flush = false,
		class: extra = '',
		children
	}: {
		title: string;
		subtitle?: string;
		actions?: Snippet;
		/** The content reaches the card's edges (a table), instead of sitting in its padding. */
		flush?: boolean;
		class?: string;
		children: Snippet;
	} = $props();
</script>

<section
	class="flex min-w-0 flex-col rounded-card border border-border bg-surface shadow-card {extra}"
>
	<header class="flex items-start justify-between gap-3 px-4 pt-3.5 pb-3">
		<div class="flex min-w-0 flex-col gap-0.5">
			<h2 class="text-sm leading-5 font-semibold tracking-tight text-text">{title}</h2>
			{#if subtitle}<p class="text-xs/normal text-text-3">{subtitle}</p>{/if}
		</div>
		{#if actions}<div class="flex shrink-0 items-center gap-1.5">{@render actions()}</div>{/if}
	</header>
	<!-- a column, so a footnote can sit at the card's foot (`mt-auto`) when its row is taller; a
	     container, so a list can take two columns when the card is wide enough for them -->
	<div class="@container flex min-w-0 flex-1 flex-col {flush ? '' : 'px-4 pb-4'}">
		{@render children()}
	</div>
</section>

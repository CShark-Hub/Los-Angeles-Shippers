<script lang="ts">
	import type { Snippet } from 'svelte';

	import type { Tone } from '$lib/ask/words';
	import Icon from '$lib/components/Icon.svelte';

	let {
		title,
		tone = 'neutral',
		alert = false,
		children
	}: {
		title: string;
		tone?: Tone;
		/** Read out at once: for something that went wrong, not for something that is happening. */
		alert?: boolean;
		children?: Snippet;
	} = $props();

	// A callout: a tint of the tone with a hairline of it all round, and the tone's mark on a disc.
	const LOOK: Record<
		Tone,
		{ box: string; disc: string; icon: 'check' | 'exclamation' | 'close' | 'info-mark' }
	> = {
		neutral: {
			box: 'border-border bg-surface',
			disc: 'bg-surface-3 text-text-2',
			icon: 'info-mark'
		},
		good: { box: 'border-success/35 bg-success/6', disc: 'bg-success text-surface', icon: 'check' },
		warn: {
			box: 'border-warning/40 bg-warning/6',
			disc: 'bg-warning text-surface',
			icon: 'exclamation'
		},
		bad: { box: 'border-danger/40 bg-danger/6', disc: 'bg-danger text-surface', icon: 'close' },
		info: { box: 'border-info/35 bg-info/6', disc: 'bg-info text-surface', icon: 'info-mark' }
	};
	const look = $derived(LOOK[tone]);
</script>

<section
	class="flex gap-3 rounded-card border px-4 py-3 text-sm {look.box}"
	role={alert ? 'alert' : 'status'}
>
	<span
		aria-hidden="true"
		class="mt-px grid size-5 shrink-0 place-items-center rounded-full {look.disc}"
		><Icon name={look.icon} size={12} weight={2.75} /></span
	>
	<div class="flex min-w-0 flex-1 flex-col gap-1">
		<h2
			class="w-fit rounded-sm text-sm font-semibold text-text focus-ring"
			tabindex="-1"
			data-focus-target
		>
			{title}
		</h2>
		{@render children?.()}
	</div>
</section>

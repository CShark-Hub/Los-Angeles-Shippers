<script lang="ts">
	import { Button } from 'bits-ui';
	import type { Snippet } from 'svelte';

	let {
		variant = 'primary',
		type = 'button',
		disabled = false,
		wide = false,
		full = false,
		onclick,
		children
	}: {
		variant?: 'primary' | 'quiet';
		type?: 'button' | 'submit';
		disabled?: boolean;
		/** The full width of its column, taller: the one action of a page, like Sign in. */
		wide?: boolean;
		/** The full width of its column at the usual size: a stack of choices, like a decision. */
		full?: boolean;
		onclick?: () => void;
		children: Snippet;
	} = $props();

	// One shape for every button: a rounded control 32px tall (40px for the one action of a page), its
	// label (and an icon, if it has one) in the middle. The page's main action is filled and lit along
	// its top edge; the others sit on the surface with a light edge and a little lift.
	const looks = {
		primary: 'border-transparent bg-accent text-on-accent shadow-accent hover:bg-accent-hover',
		quiet:
			'border-border-control bg-surface text-text shadow-control hover:border-border-strong hover:bg-surface-2'
	};
</script>

<Button.Root
	{type}
	{disabled}
	{onclick}
	class="inline-flex items-center justify-center gap-1.5 rounded-control border font-medium focus-ring transition-[background-color,border-color,color,transform] duration-150 disabled:opacity-50 disabled:shadow-none motion-safe:active:translate-y-px {wide
		? 'min-h-10 w-full px-5 py-2 text-sm'
		: 'min-h-8 px-3 py-1 text-compact'} {full ? 'w-full' : ''} {looks[variant]}"
>
	{@render children()}
</Button.Root>

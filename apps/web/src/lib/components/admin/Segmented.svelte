<script lang="ts" generics="T extends string">
	// A choice of a few, side by side, one of them on: the overview's window. Radio buttons under the
	// hood, so the arrows move between them and a reader hears which one is picked. Drawn like a quiet
	// button holding its options; the one picked is lit the way the side panel lights its page.
	let {
		label,
		options,
		value,
		onchange
	}: {
		label: string;
		options: readonly { value: T; label: string }[];
		value: T;
		onchange: (value: T) => void;
	} = $props();

	const name = $props.id();
</script>

<div
	role="radiogroup"
	aria-label={label}
	class="inline-flex h-8 items-center gap-0.5 rounded-control border border-border-control bg-surface p-0.5 shadow-control"
>
	{#each options as option (option.value)}
		<label
			class="relative inline-flex h-full items-center rounded-chip px-2.5 text-compact font-medium whitespace-nowrap text-text-2 transition-colors duration-150 not-has-checked:hover:bg-surface-2 not-has-checked:hover:text-text has-checked:bg-accent-soft has-checked:text-accent-text has-focus-visible:outline-3 has-focus-visible:outline-offset-2 has-focus-visible:outline-ring"
		>
			<input
				type="radio"
				{name}
				value={option.value}
				checked={option.value === value}
				onchange={() => onchange(option.value)}
				class="sr-only"
			/>
			{option.label}
		</label>
	{/each}
</div>

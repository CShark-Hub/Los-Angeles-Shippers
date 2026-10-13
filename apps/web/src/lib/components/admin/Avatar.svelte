<script lang="ts">
	// A person's initials in a round badge, beside their name: decoration, so hidden from readers.
	// Sized in rem, like the type, so the badge grows with the person's text size. The person picked
	// on the screen wears the accent filled, wherever they show; a disabled person is greyed.
	let {
		name,
		size = 28,
		muted = false,
		selected = false
	}: { name: string; size?: number; muted?: boolean; selected?: boolean } = $props();

	const letters = $derived(
		name
			.split(/\s+/)
			.filter(Boolean)
			.slice(0, 2)
			.map((word) => word[0].toUpperCase())
			.join('')
	);

	const look = $derived(
		muted
			? `bg-surface-2 text-text-3 ${selected ? 'ring-accent' : 'ring-border'}`
			: selected
				? 'bg-accent text-on-accent ring-accent shadow-accent'
				: 'bg-accent-soft text-accent-text ring-accent/20'
	);
</script>

<span
	aria-hidden="true"
	class="grid shrink-0 place-items-center rounded-full font-semibold tracking-tight ring-1 transition-colors select-none ring-inset {look}"
	style="width: {size / 16}rem; height: {size / 16}rem; font-size: {(size * 0.38) / 16}rem"
	>{letters}</span
>

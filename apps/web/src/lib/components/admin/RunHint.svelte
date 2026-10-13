<script lang="ts">
	import { MCTL } from '$lib/admin/commands';
	import Icon from '$lib/components/Icon.svelte';

	// Says once per screen how the commands beside things are run, and why nothing here changes
	// anything: a stolen session in a browser must not be able to grant, issue or loosen.
	let { what = 'Nothing here changes anything.' }: { what?: string } = $props();

	// a command named in the sentence, as a small code chip that keeps its shape when it wraps
	const chip =
		'rounded-chip border border-border-subtle bg-surface-2 px-1.5 py-px font-mono text-2xs leading-5 text-text box-decoration-clone';
</script>

<div
	class="flex items-start gap-3 rounded-card border border-border bg-surface px-4 py-3 text-xs leading-5 text-text-2 shadow-card"
>
	<span
		class="grid size-6 shrink-0 place-items-center rounded-control bg-surface-2 text-text-2"
		aria-hidden="true"><Icon name="terminal" size={15} /></span
	>
	<p class="min-w-0 pt-0.5">
		<span class="font-semibold text-text">Read-only.</span>
		{what} Changes are made on the server over SSH with the command shown beside each thing, where
		<code class={chip}>mctl</code>
		stands for
		<code class={chip}>{MCTL}</code>.
	</p>
</div>

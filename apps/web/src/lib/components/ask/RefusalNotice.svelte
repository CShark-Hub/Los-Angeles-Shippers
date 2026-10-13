<script lang="ts">
	import type { Snippet } from 'svelte';

	import type { Refusal } from '$lib/api';
	import AppButton from '$lib/components/AppButton.svelte';
	import Icon from '$lib/components/Icon.svelte';

	let {
		refusal,
		live = true,
		onaskagain,
		askingAgain = false
	}: {
		refusal: Refusal;
		/** The refusal of the question on screen, where the focus goes; an earlier one in the conversation isn't. */
		live?: boolean;
		/**
		 * Asks again a question an approver rejected: its words go back in the box, and the next
		 * question names the rejected one. Offered only with an approver's rejection on screen.
		 */
		onaskagain?: () => void;
		/** The person chose to ask it again: the box holds the question, waiting to be sent. */
		askingAgain?: boolean;
	} = $props();

	const again = $derived(live && refusal.code === 'rejected_by_approver' && onaskagain);
	// A confidential ask is refused for what it is about, not for how it was put: its mark is a lock.
	const mark = $derived<'lock' | 'exclamation'>(
		refusal.code === 'confidential' ? 'lock' : 'exclamation'
	);
</script>

<!-- A reply like the answer, in the warning's tone: the same card, edge, header row and inset footer, so a
     conversation reads as one column of replies. What the core said, whenever it said it: why, what to do
     instead (for an answer an approver rejected, their note), and the reason's code. -->
{#snippet reply(title: Snippet)}
	<span aria-hidden="true" class="absolute inset-x-0 top-0 h-0.5 bg-warning"></span>
	<div
		class="flex min-h-12 items-center gap-2.5 border-b border-border-subtle bg-linear-to-b from-warning/8 to-transparent px-5 py-2.5"
	>
		<span
			aria-hidden="true"
			class="grid size-6 shrink-0 place-items-center rounded-full bg-warning/15 text-warning ring-1 ring-warning/25 ring-inset"
			><Icon
				name={mark}
				size={mark === 'lock' ? 14 : 13}
				weight={mark === 'lock' ? 2.25 : 2.75}
			/></span
		>
		{@render title()}
	</div>
	<div class="flex max-w-measure flex-col gap-1.5 px-5 pt-4 pb-4.5">
		<p class="text-title leading-relaxed text-text">{refusal.message}</p>
		{#if refusal.hint}<p class="text-sm leading-relaxed text-text-2">{refusal.hint}</p>{/if}
		{#if again && askingAgain}
			<p class="text-sm leading-relaxed text-text-2">
				Your question is back in the box below: send it as it is or change it first. Mercury checks
				it again, and the record links it to this one.
			</p>
		{/if}
	</div>
	<div
		class="flex min-h-10 flex-wrap items-center gap-x-3 gap-y-2 border-t border-border-subtle bg-surface-2/60 px-5 py-2"
	>
		<p class="text-xs text-text-3">
			Reason: <code
				class="rounded-chip bg-surface px-1.5 py-px font-mono text-2xs text-text-2 ring-1 ring-border ring-inset"
				>{refusal.code}</code
			>
		</p>
		{#if again && !askingAgain}
			<span class="ml-auto"
				><AppButton variant="quiet" onclick={onaskagain}>Ask again</AppButton></span
			>
		{/if}
	</div>
{/snippet}

{#if live}
	{#snippet heading()}
		<h2
			class="w-fit rounded-sm text-sm font-semibold text-text focus-ring"
			tabindex="-1"
			data-focus-target
		>
			Mercury can't answer that
		</h2>
	{/snippet}
	<section
		role="status"
		class="relative overflow-hidden rounded-card border border-border bg-surface shadow-card"
	>
		{@render reply(heading)}
	</section>
{:else}
	<!-- the same reply, an earlier question's: not news, so not read out and not where the focus goes -->
	{#snippet label()}
		<p class="text-sm font-semibold text-text">Mercury can't answer that.</p>
	{/snippet}
	<div class="relative overflow-hidden rounded-card border border-border bg-surface shadow-card">
		{@render reply(label)}
	</div>
{/if}

<script lang="ts">
	import Icon from '$lib/components/Icon.svelte';
	import { describe, type Described } from '$lib/errors';
	import { session } from '$lib/session.svelte';

	let busy = $state(false);
	let problem = $state<Described | null>(null);

	// Ends the session at the core; the layout then takes the person to Sign in.
	async function disconnect() {
		busy = true;
		problem = null;
		try {
			await session.signOut();
		} catch (error) {
			problem = describe(error);
		} finally {
			busy = false;
		}
	}
</script>

<!-- a quiet icon at the end of the person's row; its name is said, and shown on hover -->
<div class="relative shrink-0">
	<button
		type="button"
		aria-label="Disconnect"
		title="Disconnect"
		disabled={busy}
		onclick={disconnect}
		class="grid size-8 place-items-center rounded-control text-text-3 focus-ring transition-colors hover:bg-surface-2 hover:text-text disabled:opacity-50"
	>
		<Icon name="logout" size={17} />
	</button>
	{#if problem}
		<p
			role="alert"
			class="pointer-events-none absolute right-0 bottom-full z-10 mb-2 w-60 rounded-control border border-border bg-surface-raised px-3 py-2 text-compact text-danger shadow-raised"
		>
			{problem.message}
			{#if problem.hint}<span class="text-text-2">{problem.hint}</span>{/if}
		</p>
	{/if}
</div>

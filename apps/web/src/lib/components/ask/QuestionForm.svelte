<script lang="ts">
	import type { ProjectSummary } from '$lib/api';
	import { describeAge } from '$lib/format';
	import Icon from '$lib/components/Icon.svelte';

	let {
		projects,
		busy,
		text = $bindable(''),
		project = $bindable(''),
		onask
	}: {
		projects: ProjectSummary[];
		busy: boolean;
		text?: string;
		/** The project picked, or '' to let Mercury work it out from the question. */
		project?: string;
		onask: () => void;
	} = $props();

	let box = $state<HTMLTextAreaElement | null>(null);
	const picked = $derived(projects.find((p) => p.project === project));
	const ready = $derived(!busy && text.trim() !== '');

	// What the person should know about the repository behind the project picked, in a few words.
	const probe = $derived.by(() => {
		if (!picked) return null;
		if (picked.probe === null) return 'not read yet';
		if (picked.probe.status === 'stale') return `last read ${describeAge(picked.probe.as_of)}`;
		if (picked.probe.status === 'quarantined') return 'switched off by an admin';
		return null;
	});

	// The box grows with what is typed, up to a few lines, then scrolls.
	$effect(() => {
		void text;
		if (!box) return;
		box.style.height = 'auto';
		box.style.height = `${Math.min(box.scrollHeight, 160)}px`;
	});

	function submit(event: SubmitEvent) {
		event.preventDefault();
		if (ready) onask();
	}

	// Enter asks, Shift+Enter starts a new line; a key that composes a character is left alone.
	function keydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
			event.preventDefault();
			if (ready) onask();
		}
	}
</script>

<!-- An elevated panel: the box itself draws no outline, the whole composer shows where the focus is.
     Below the box, the project it asks about, a word on that project's data, the keys, and Ask. -->
<form
	onsubmit={submit}
	class="rounded-panel border border-border-strong bg-surface-raised shadow-raised transition-[border-color] duration-150 focus-within:border-accent focus-within:outline-3 focus-within:outline-offset-2 focus-within:outline-ring"
>
	<label for="question" class="sr-only">Your question</label>
	<textarea
		id="question"
		name="question"
		bind:this={box}
		bind:value={text}
		onkeydown={keydown}
		rows="2"
		maxlength="2000"
		required
		disabled={busy}
		placeholder={busy ? 'Mercury is answering…' : 'Ask anything about the project…'}
		aria-describedby="question-help"
		class="block max-h-40 w-full resize-none bg-transparent px-4 pt-3.5 pb-2 text-title leading-6 text-text outline-none placeholder:text-text-3 disabled:cursor-not-allowed"
	></textarea>

	<div class="flex items-center gap-2.5 px-3 pt-0.5 pb-3">
		<label for="project" class="sr-only">Project</label>
		<span class="relative inline-flex min-w-0 items-center">
			<span class="pointer-events-none absolute left-2.5 text-text-3" aria-hidden="true"
				><Icon name="folder" size={15} /></span
			>
			<select
				id="project"
				bind:value={project}
				disabled={busy}
				class="h-8 max-w-64 min-w-0 appearance-none truncate rounded-control border border-border-control bg-surface-2 pr-8 pl-8 text-compact font-medium text-text focus-ring transition-colors hover:border-border-strong hover:bg-surface-3 disabled:opacity-50"
			>
				{#if projects.length !== 1}
					<option value="">Any project</option>
				{/if}
				{#each projects as option (option.project)}
					<option value={option.project}>{option.display_name}</option>
				{/each}
			</select>
			<span class="pointer-events-none absolute right-2.5 text-text-3" aria-hidden="true"
				><Icon name="chevron-down" size={14} /></span
			>
		</span>
		{#if probe}<span class="flex min-w-0 items-center gap-1 text-xs text-warning"
				><span class="shrink-0" aria-hidden="true"><Icon name="alert" size={13} /></span><span
					class="truncate">{probe}</span
				></span
			>{/if}
		<span class="flex-1"></span>
		<p
			id="question-help"
			class="hidden truncate text-xs text-text-3 transition-opacity md:block {busy
				? 'opacity-0'
				: ''}"
		>
			<kbd
				class="rounded-chip border border-border bg-surface-2 px-1.5 py-px font-sans text-2xs font-medium text-text-2"
				>Enter</kbd
			>
			to ask ·
			<kbd
				class="rounded-chip border border-border bg-surface-2 px-1.5 py-px font-sans text-2xs font-medium text-text-2"
				>Shift+Enter</kbd
			> for a new line
		</p>
		<button
			type="submit"
			aria-label="Ask"
			title="Ask"
			disabled={!ready}
			class="grid size-8 shrink-0 place-items-center rounded-control bg-accent text-on-accent shadow-accent focus-ring transition-[background-color,color,translate] duration-150 hover:bg-accent-hover disabled:cursor-not-allowed disabled:bg-surface-3 disabled:text-text-3 disabled:shadow-none motion-safe:active:translate-y-px"
		>
			<Icon name="send" size={16} weight={2.3} />
		</button>
	</div>
</form>

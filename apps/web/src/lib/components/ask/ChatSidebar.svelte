<script lang="ts">
	import { resolve } from '$app/paths';
	import type { ProjectSummary } from '$lib/api';
	import type { Thread } from '$lib/ask/history.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { LOCALE } from '$lib/format';

	let {
		items,
		current,
		projects,
		onnew
	}: {
		items: Thread[];
		/** The key of the conversation on screen. */
		current: string | null;
		projects: ProjectSummary[];
		onnew: () => void;
	} = $props();

	// How its last question ended, as a dot and a word for whoever doesn't see the colour. An answer is
	// the usual ending, so its word stays with the screen reader; any other ending is said on screen too.
	type Status = { dot: string; word: string; shown?: false };
	const STATUS: Record<string, Status> = {
		delivered: { dot: 'bg-success', word: 'Answered', shown: false },
		refused: { dot: 'bg-warning', word: 'Refused' },
		clarify: { dot: 'bg-info', word: 'Needs your choice' },
		pending_signoff: { dot: 'bg-info', word: 'Waiting for an approver' },
		failed: { dot: 'bg-danger', word: 'Failed' },
		unavailable: { dot: 'bg-danger', word: 'Not available' },
		disconnected: { dot: 'bg-warning', word: 'Connection lost' },
		expired: { dot: 'bg-border-strong', word: 'No longer kept' }
	};
	// Live: it pulses, or, for whoever asked for less motion, holds still inside a soft halo.
	const RUNNING: Status = {
		dot: 'bg-accent motion-safe:animate-pulse motion-reduce:ring-2 motion-reduce:ring-accent/30',
		word: 'Answering'
	};

	function nameOf(project: string | null): string | null {
		if (!project) return null;
		return projects.find((p) => p.project === project)?.display_name ?? project;
	}

	function when(at: number): string {
		const minutes = Math.floor((Date.now() - at) / 60_000);
		if (minutes < 1) return 'now';
		if (minutes < 60) return `${minutes} min`;
		return new Intl.DateTimeFormat(LOCALE, { timeStyle: 'short' }).format(new Date(at));
	}
</script>

<nav aria-label="Your conversations" class="flex h-full min-h-0 flex-col">
	<button
		type="button"
		onclick={onnew}
		class="flex h-8 items-center gap-2 rounded-control border border-border-control bg-surface px-2.5 text-compact font-medium text-text shadow-control focus-ring transition-colors hover:border-border-strong hover:bg-surface-2"
	>
		<span class="text-accent-text"><Icon name="plus" size={15} /></span>New conversation
	</button>

	<p class="mt-4 mb-1.5 flex items-center justify-between px-2 text-xs font-medium text-text-3">
		Conversations
		{#if items.length > 0}<span aria-hidden="true" class="text-2xs tabular-nums"
				>{items.length}</span
			>{/if}
	</p>

	{#if items.length === 0}
		<!-- a quiet frame for the place the list will be, and why it starts empty -->
		<div
			class="flex items-start gap-2.5 rounded-control border border-dashed border-border-control px-2.5 py-2.5"
		>
			<span
				aria-hidden="true"
				class="grid size-6 shrink-0 place-items-center rounded-chip bg-surface-2 text-text-3"
				><Icon name="chat" size={14} /></span
			>
			<p class="flex min-w-0 flex-col gap-0.5 text-xs leading-snug text-text-3">
				<span class="text-compact font-medium text-text-2">None yet.</span>
				<span>They stay in this tab only; nothing is kept in the browser.</span>
			</p>
		</div>
	{:else}
		<!-- room on every side for a row's focus ring, which the scrolling list would clip -->
		<ul
			class="-mt-1.5 -mr-2 -ml-1.5 flex min-h-0 flex-col gap-0.5 overflow-y-auto pt-1.5 pr-2 pb-2 pl-1.5"
		>
			{#each items as thread (thread.key)}
				{@const first = thread.turns[0]}
				{@const last = thread.turns.at(-1)}
				{@const status =
					!last || ['running', 'sending', 'loading'].includes(last.phase)
						? RUNNING
						: (STATUS[last.phase] ?? RUNNING)}
				{@const selected = thread.key === current}
				{@const project = last ? nameOf(last.project) : null}
				{#if last}
					<li>
						<!-- a link to its last question: the Ask page opens the question every navigation names,
						     the one to the address the router went to last as well; two lines, the question,
						     then how it stands, where it was asked and how many questions it holds, and when;
						     how it ended, in a line, on pointing at it, and in the link's name after the
						     question for whoever can't point (a screen reader, a finger) -->
						<a
							href={resolve('/ask/[id]', { id: last.id })}
							aria-current={selected ? 'page' : undefined}
							title={last.preview || undefined}
							class="grid grid-cols-[0.5rem_minmax(0,1fr)_auto] gap-x-2.5 rounded-control px-2.5 py-2 focus-ring transition-[background-color,box-shadow] {selected
								? 'bg-surface-raised shadow-control ring-1 ring-border ring-inset'
								: 'hover:bg-surface-2/70'}"
						>
							<span class="flex h-[1lh] items-center text-compact" aria-hidden="true">
								<span class="size-2 rounded-full {status.dot}"></span>
							</span>
							<span class="sr-only">{status.word}:</span>
							<span
								class="col-span-2 truncate text-compact font-medium {first?.text
									? 'text-text'
									: 'text-text-2 italic'}">{first?.text || 'A question from before'}</span
							>
							{#if last.preview}<span class="sr-only">{last.preview}</span>{/if}
							<!-- one line of whole pieces, most telling first: a piece that doesn't fit drops out
							     of sight (onto a hidden second line) rather than leave a stub; only the first
							     may shorten -->
							<span
								class="col-start-2 flex h-[1lh] min-w-0 flex-wrap items-center overflow-hidden text-xs text-text-3 [&>*]:shrink-0 [&>*+*]:before:mx-1 [&>*+*]:before:content-['·'] [&>:first-child]:min-w-0 [&>:first-child]:shrink [&>:first-child]:truncate"
							>
								{#if status.shown !== false}<span aria-hidden="true" class="font-medium text-text-2"
										>{status.word}</span
									>{/if}
								{#if project}<span>{project}</span>{/if}
								{#if thread.turns.length > 1}<span class="tabular-nums"
										>{thread.turns.length} questions</span
									>{/if}
							</span>
							<span class="self-center text-2xs text-text-3 tabular-nums"
								>{when(thread.updated)}</span
							>
						</a>
					</li>
				{/if}
			{/each}
		</ul>
	{/if}
</nav>

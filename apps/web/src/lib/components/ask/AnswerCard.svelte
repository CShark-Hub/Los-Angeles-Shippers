<script lang="ts">
	import type { Answer, Ending } from '$lib/api';
	import { describeDay, describeDuration } from '$lib/format';
	import { evidenceWords, statusChip, testedChip, waitingWords } from '$lib/ask/words';

	import { answerCard } from '$lib/ask/answer-text';

	import Icon from '$lib/components/Icon.svelte';

	import Chip from './Chip.svelte';
	import Meter from './Meter.svelte';

	let {
		answer,
		done,
		notes = [],
		withheld = 0,
		leftOut = 0,
		fromCode = false,
		live = true
	}: {
		answer: Answer;
		done: Ending | null;
		/** What the person should know about the data the answer rests on: a stale repository, say. */
		notes?: string[];
		/** Text values the data checks withheld. */
		withheld?: number;
		/** Sentences the output checks left out. */
		leftOut?: number;
		/** Part of the answer comes from the project's code reader. */
		fromCode?: boolean;
		/** The answer to the question on screen, where the focus goes; an earlier one in the conversation isn't. */
		live?: boolean;
	} = $props();

	// The text's blocks and the notes on what the checks kept back, each thing said once
	// (`answerCard`); a note the page gives twice is said once too.
	const card = $derived(answerCard(answer.text, { withheld, leftOut }));
	const blocks = $derived(card.blocks);
	const said = $derived([...new Set([...notes, ...card.notes])]);

	// How long it took; what it cost and which judge checked it are in the trail's `Done` row, folded under the answer.
	const meta = $derived(done ? describeDuration(done.duration_ms) : '');
</script>

<!-- The answer, and the pattern every reply follows: a tone edge on top, a header row (what it
     is, how sure, where it came from, how long it took), the prose at a reading measure, then the facts
     it rests on as an aligned table on an inset. -->
<section
	aria-labelledby={live ? 'answer-heading' : undefined}
	aria-label={live ? undefined : 'Earlier in this conversation'}
	class="relative overflow-hidden rounded-card border border-border bg-surface shadow-card"
>
	<span aria-hidden="true" class="absolute inset-x-0 top-0 h-0.5 bg-accent-2"></span>
	<div
		class="flex min-h-12 flex-wrap items-center gap-x-3 gap-y-1.5 border-b border-border-subtle bg-linear-to-b from-accent-2/7 to-transparent px-5 py-2.5"
	>
		<span class="flex items-center gap-2.5">
			<span
				aria-hidden="true"
				class="grid size-6 shrink-0 place-items-center rounded-full bg-accent-2/15 text-accent-2 ring-1 ring-accent-2/25 ring-inset"
				><Icon name="check" size={13} weight={2.75} /></span
			>
			{#if live}
				<h2
					id="answer-heading"
					class="rounded-sm text-sm font-semibold text-text focus-ring"
					tabindex="-1"
					data-focus-target
				>
					Answer
				</h2>
			{:else}
				<h2 class="text-sm font-semibold text-text">Answer</h2>
			{/if}
		</span>
		<span class="h-4 w-px bg-border" aria-hidden="true"></span>
		<Meter value={answer.confidence} />
		{#if fromCode}
			<span
				title="Part of this answer comes from the project's code reader, checked like every answer."
				><Chip tone="info">Read from the code</Chip></span
			>
		{/if}
		{#if meta}
			<span class="ml-auto flex items-center gap-1.5 text-xs text-text-3 tabular-nums"
				><Icon name="clock" size={13} />{meta}</span
			>
		{/if}
	</div>

	<div class="flex flex-col gap-3.5 px-5 pt-4 pb-5">
		<!-- The one place a model's text is shown: the core sent it in the `answer` event, after its checks.
		     Bold runs and list items are read from its marks; every piece is still text, never markup. -->
		<div class="flex max-w-measure flex-col gap-2.5 text-title leading-relaxed text-text">
			{#each blocks as block, i (i)}
				{#if block.kind === 'paragraph'}
					<p>
						{#each block.parts as part, j (j)}{#if part.strong}<strong class="font-semibold"
									>{part.text}</strong
								>{:else}{part.text}{/if}{/each}
					</p>
				{:else if block.kind === 'list'}
					<ul class="flex list-disc flex-col gap-1 pl-5 marker:text-accent-text">
						{#each block.items as item, j (j)}
							<li class="pl-0.5">
								{#each item as part, k (k)}{#if part.strong}<strong class="font-semibold"
											>{part.text}</strong
										>{:else}{part.text}{/if}{/each}
							</li>
						{/each}
					</ul>
				{:else}
					<p class="text-compact leading-normal text-text-3">{block.text}</p>
				{/if}
			{/each}
		</div>

		{#if said.length}
			<!-- what the checks kept back, set apart from the prose: a mark in the warning's tone, then the note -->
			<ul class="flex max-w-measure flex-col gap-1.5 text-compact text-text-2">
				{#each said as note (note)}
					<li class="flex items-start gap-2">
						<span
							aria-hidden="true"
							class="mt-0.5 grid size-4 shrink-0 place-items-center rounded-full bg-warning/15 text-warning"
							><Icon name="exclamation" size={10} weight={3} /></span
						>{note}
					</li>
				{/each}
			</ul>
		{/if}
	</div>

	{#if answer.facts.length}
		<div class="border-t border-border-subtle bg-surface-2/60 px-5 pt-3 pb-1.5">
			<div class="flex items-center gap-2">
				<h3 class="text-xs font-semibold text-text-2">What it rests on</h3>
				<span
					class="grid h-4.5 min-w-4.5 place-items-center rounded-full bg-surface-3 px-1 text-2xs font-semibold text-text-2 tabular-nums"
					>{answer.facts.length}</span
				>
			</div>
			<!-- One row per fact. On a wide screen the rows share three columns, each as wide as it needs:
			     what it is (its title over its module), how it stands (the chips), and what shows it (the
			     evidence over the day it was read). The free space falls after the last column, not between. -->
			<ul
				class="mt-1.5 divide-y divide-border-subtle lg:grid lg:grid-cols-[fit-content(22rem)_auto_minmax(0,1fr)] lg:gap-x-8"
			>
				{#each answer.facts as fact (fact.id)}
					{@const status = statusChip(fact.status)}
					{@const tested = testedChip(fact.tested)}
					{@const shows = fact.evidence.map(evidenceWords).join(' · ')}
					<li
						class="flex flex-wrap items-center gap-x-4 gap-y-1.5 py-2.5 lg:col-span-3 lg:grid lg:grid-cols-subgrid"
					>
						<span class="flex min-w-0 flex-1 basis-60 flex-col lg:basis-auto">
							<span
								class="text-compact leading-snug font-medium break-words {fact.title_withheld
									? 'text-text-3 italic'
									: 'text-text'}">{fact.title_withheld ? 'Title withheld' : fact.title}</span
							>
							<span class="text-2xs text-text-3">{fact.module}</span>
						</span>
						<ul class="flex flex-wrap gap-1" aria-label="About this item">
							<li><Chip tone={status.tone}>{status.label}</Chip></li>
							<li><Chip tone={tested.tone}>{tested.label}</Chip></li>
							{#if fact.waiting_on}<li>
									<Chip tone="warn">{waitingWords(fact.waiting_on)}</Chip>
								</li>{/if}
						</ul>
						<span class="flex min-w-0 basis-full flex-col tabular-nums lg:basis-auto">
							{#if shows}<span class="text-xs text-text-2">{shows}</span>{/if}
							<span class="text-2xs text-text-3"
								>{[`As of ${describeDay(fact.updated_at)}`, ...fact.people].join(' · ')}</span
							>
						</span>
					</li>
				{/each}
			</ul>
		</div>
	{/if}
</section>

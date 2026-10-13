<script lang="ts">
	import { commands } from '$lib/admin/commands';
	import { approves } from '$lib/admin/people';
	import { limitWords, PROBE_WORDS, probeCondition, type ProbeCondition } from '$lib/admin/words';
	import type { AccessMode, AdminRepository } from '$lib/api';
	import Chip from '$lib/components/ask/Chip.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { describeAge, describeMoment, roleLabel } from '$lib/format';
	import Avatar from './Avatar.svelte';
	import CopyCommand from './CopyCommand.svelte';

	// One repository: its probe's state and how fresh its facts are, how much of the code it may read
	// against what the server allows, who may ask about it, and the commands that change any of it.
	// The card's six parts are rows of the page's grid (a subgrid), so the same part of every card in
	// a row starts at the same height and the cards compare at a glance.
	let { repository }: { repository: AdminRepository } = $props();

	const MODES: readonly { mode: AccessMode; label: string; meaning: string }[] = [
		{
			mode: 'metadata',
			label: 'Metadata',
			meaning: 'Issues, pull requests and builds only: never the code.'
		},
		{
			mode: 'code',
			label: 'Code',
			meaning: 'The probe reads the code for its figures; the code is never shown.'
		},
		{
			mode: 'code_model',
			label: 'Code + model',
			meaning: 'The code is read with a model too; the code is never shown.'
		}
	];

	// a state that needs a look washes the card's head faintly in its tone, as the figures above do,
	// and tints its icon; the chip names it
	const WASH: Partial<Record<ProbeCondition, string>> = {
		quarantined: 'bg-linear-to-b from-danger/7 to-transparent',
		stale: 'bg-linear-to-b from-warning/8 to-transparent'
	};
	const MARK: Record<ProbeCondition, string> = {
		quarantined: 'bg-danger/10 text-danger ring-1 ring-danger/20 ring-inset',
		stale: 'bg-warning/12 text-warning ring-1 ring-warning/25 ring-inset',
		unregistered: 'bg-surface-2 text-text-3 ring-1 ring-border ring-inset',
		healthy: 'bg-surface-2 text-text-2 ring-1 ring-border ring-inset'
	};

	/** A list longer than this by two or more shows its first ones and a "+N more". */
	const PEOPLE_SHOWN = 4;
	const MODULES_SHOWN = 6;

	const probe = $derived(repository.probe);
	const condition = $derived(probeCondition(probe));
	const words = $derived(PROBE_WORDS[condition]);
	const reads = $derived(MODES.findIndex((m) => m.mode === probe?.access));
	const allowed = $derived(MODES.findIndex((m) => m.mode === repository.access_max));

	// how fresh the facts are, in the tone of the probe: a silent one's last word stands out
	const heard = $derived(
		condition === 'stale' ? 'text-warning' : probe === null ? 'text-text-3' : 'text-text'
	);

	let reason = $state('Investigating');
	let more = $state(false);
	let everyone = $state(false);
	let allModules = $state(false);

	const modules = $derived(probe?.modules ?? []);
	const foldModules = $derived(modules.length > MODULES_SHOWN + 1);
	const shownModules = $derived(
		foldModules && !allModules ? modules.slice(0, MODULES_SHOWN) : modules
	);
	const foldPeople = $derived(repository.people.length > PEOPLE_SHOWN + 1);
	const shownPeople = $derived(
		foldPeople && !everyone ? repository.people.slice(0, PEOPLE_SHOWN) : repository.people
	);

	// The ladder of what a probe may read, from least to most, as one meter of three segments: what
	// it reads is filled (the step it reads now the strongest), what the server also allows is a
	// tint of the accent, and past the server's limit the track is empty.
	function segment(i: number): string {
		if (i === reads) return 'bg-accent';
		if (i < reads) return 'bg-accent/55';
		if (i <= allowed) return 'bg-accent/20';
		return 'bg-surface-3';
	}
	function tone(i: number): string {
		if (i === reads) return 'text-text';
		if (i <= allowed || i < reads) return 'text-text-2';
		return 'text-text-3';
	}
	function status(i: number): string {
		if (i === reads) return 'Reads now';
		if (i === allowed) return 'Server allows';
		if (i < allowed) return 'Allowed';
		return 'Not allowed';
	}

	// the side label of a part, level with the first line of what it shows
	const label = 'text-xs leading-5 font-medium text-text-3';
	const part = 'grid grid-cols-[6rem_minmax(0,1fr)] items-start gap-x-3 px-4';
	const unfold =
		'-mx-1 inline-flex h-5 items-center rounded-chip px-1 text-2xs font-medium text-text-2 focus-ring transition-colors hover:bg-surface-2 hover:text-text';
</script>

<article
	class="relative row-span-6 grid grid-rows-subgrid gap-y-0 overflow-hidden rounded-card border border-border bg-surface shadow-card"
	aria-labelledby="repo-{repository.project}"
>
	<header class="flex flex-col gap-2.5 px-4 pt-4 pb-3.5 {WASH[condition] ?? ''}">
		<div class="flex items-start gap-3">
			<span
				aria-hidden="true"
				class="grid size-9 shrink-0 place-items-center rounded-control {MARK[condition]}"
				><Icon name="repo" size={17} /></span
			>
			<div class="flex min-w-0 flex-1 flex-col gap-0.5">
				<div class="flex min-w-0 items-start justify-between gap-3">
					<h2
						id="repo-{repository.project}"
						class="truncate text-title/6 font-semibold tracking-tight text-text"
					>
						{repository.display_name ?? repository.project}
					</h2>
					<span class="shrink-0 pt-0.5" title={words.meaning}
						><Chip tone={words.tone}>{words.label}</Chip></span
					>
				</div>
				<p class="truncate font-mono text-2xs text-text-3">
					{repository.project}{repository.display_name === null ? ' · not in projects.yaml' : ''}
				</p>
			</div>
		</div>
		<!-- what the state means for this card; a quarantine says why, in its own words -->
		{#if condition === 'quarantined' && probe?.quarantine_reason}
			<p
				class="ml-12 flex items-start gap-2 rounded-control bg-danger/8 px-2.5 py-1.5 text-compact leading-5 text-text ring-1 ring-danger/20 ring-inset"
			>
				<span
					aria-hidden="true"
					class="mt-0.5 grid size-4 shrink-0 place-items-center rounded-full bg-danger text-surface"
					><Icon name="exclamation" size={11} weight={2.75} /></span
				>
				<span><span class="font-semibold">Quarantined:</span> {probe.quarantine_reason}</span>
			</p>
		{:else}
			<p class="ml-12 text-xs leading-4.5 text-text-3">{words.meaning}</p>
		{/if}
	</header>

	<!-- the figures at a glance, in a band of their own: how fresh, how much asked, how lately -->
	<dl
		class="grid grid-cols-3 divide-x divide-border-subtle border-y border-border-subtle bg-surface-2/50"
	>
		<div class="flex min-w-0 flex-col gap-0.5 px-4 py-2.5">
			<dt class="truncate text-2xs text-text-3">Last heard from</dt>
			<dd
				class="truncate text-sm font-semibold tabular-nums {heard}"
				title={probe ? describeMoment(probe.last_seen_at) : undefined}
			>
				{probe ? describeAge(probe.last_seen_at) : 'Never'}
			</dd>
		</div>
		<div class="flex min-w-0 flex-col gap-0.5 px-4 py-2.5">
			<dt class="truncate text-2xs text-text-3">Questions, 30 days</dt>
			<dd class="text-sm font-semibold text-text tabular-nums">
				{repository.questions_30d}
			</dd>
		</div>
		<div class="flex min-w-0 flex-col gap-0.5 px-4 py-2.5">
			<dt class="truncate text-2xs text-text-3">Last question</dt>
			<dd
				class="truncate text-sm font-semibold tabular-nums {repository.last_asked_at
					? 'text-text'
					: 'text-text-3'}"
			>
				{repository.last_asked_at ? describeAge(repository.last_asked_at) : 'None yet'}
			</dd>
		</div>
	</dl>

	<section aria-label="What the probe reads" class="{part} pt-3.5 pb-2">
		<h3 class={label}>What it reads</h3>
		<div class="flex min-w-0 flex-col gap-2">
			<!-- the three steps as one meter: the words over each segment, the segments end to end -->
			<ol class="grid grid-cols-3 gap-x-1" aria-label="Access modes, from least to most">
				{#each MODES as m, i (m.mode)}
					<li title={m.meaning} class="flex min-w-0 flex-col">
						<span class="truncate text-xs leading-5 font-medium {tone(i)}">{m.label}</span>
						<span
							class="truncate text-2xs leading-4 {i === reads
								? 'font-medium text-accent-text'
								: 'text-text-3'}">{status(i)}</span
						>
						<span
							aria-hidden="true"
							class="mt-1.5 h-1.5 {i === 0 ? 'rounded-l-full' : ''} {i === MODES.length - 1
								? 'rounded-r-full'
								: ''} {segment(i)}"
						></span>
					</li>
				{/each}
			</ol>
			{#if probe?.access_limited_by}
				<p class="text-2xs leading-4 text-text-3">
					limited by {limitWords(probe.access_limited_by)}
				</p>
			{/if}
			{#if probe === null}
				<p class="text-2xs leading-4 text-text-3">
					Its probe hasn't registered, so nothing is read yet.
				</p>
			{/if}
		</div>
	</section>

	{#if probe}
		<section aria-label="What it answers" class="{part} py-2">
			<h3 class={label}>Modules</h3>
			<ul class="flex min-w-0 flex-wrap items-center gap-1">
				{#each shownModules as module (module)}
					<li><Chip>{module}</Chip></li>
				{:else}
					<li class="text-2xs leading-5 text-text-3">None described.</li>
				{/each}
				{#if foldModules}
					<li>
						<button
							type="button"
							class={unfold}
							aria-expanded={allModules}
							onclick={() => (allModules = !allModules)}
							>{allModules ? 'Show fewer' : `+${modules.length - MODULES_SHOWN} more`}</button
						>
					</li>
				{/if}
			</ul>
		</section>
	{:else}
		<!-- no probe, no modules: the row stays, so the parts below line up with the cards beside -->
		<div class="{part} py-2" aria-hidden="true">
			<span class={label}>Modules</span><span class="text-xs leading-5 text-text-3">—</span>
		</div>
	{/if}

	<section aria-label="Who may ask" class="{part} pt-2 pb-4">
		<div class="flex flex-col">
			<h3 class={label}>Who may ask</h3>
			<span class="text-2xs text-text-3 tabular-nums">{repository.people.length} granted</span>
		</div>
		{#if repository.people.length === 0}
			<p class="text-xs leading-5 text-text-3">Nobody is granted this project.</p>
		{:else}
			<!-- one row per person on shared columns, so the roles and the approver marks line up -->
			<ul
				class="grid min-w-0 grid-cols-[minmax(0,max-content)_max-content_minmax(min-content,1fr)] gap-x-3 gap-y-1"
			>
				{#each shownPeople as person (person.email)}
					<li class="col-span-3 grid min-w-0 grid-cols-subgrid items-center text-compact leading-5">
						<span class="flex min-w-0 items-center gap-2">
							<Avatar name={person.display_name} size={20} muted={person.disabled_at !== null} />
							<span
								class="truncate font-medium {person.disabled_at
									? 'text-text-3 line-through'
									: 'text-text'}">{person.display_name}</span
							>
						</span>
						<span class="text-xs text-text-3">{roleLabel(person.role)}</span>
						<span class="flex min-w-0"
							>{#if approves(person, person)}<Chip tone="info">Approver</Chip>
							{:else if person.approver}<Chip tone="warn">Approver flag, no effect</Chip>{/if}</span
						>
					</li>
				{/each}
				{#if foldPeople}
					<li class="col-span-3">
						<button
							type="button"
							class={unfold}
							aria-expanded={everyone}
							onclick={() => (everyone = !everyone)}
							>{everyone
								? 'Show fewer'
								: `+${repository.people.length - PEOPLE_SHOWN} more`}</button
						>
					</li>
				{/if}
			</ul>
		{/if}
	</section>

	<!-- the way from what the card shows to a change: the commands to run on the server, at the
	     foot, and the one that switches the probe off at the end of the last row, set apart -->
	<footer class="flex flex-col gap-3 border-t border-border-subtle px-4 pt-3.5 pb-4">
		{#if condition === 'quarantined'}
			<CopyCommand
				label="Release it (it is checked again on its next registration)"
				command={commands.release(repository.project)}
			/>
		{/if}
		<div class="flex min-w-0 items-end gap-2.5">
			<div class="min-w-0 flex-1">
				<CopyCommand
					label="Who may ask, in the terminal"
					command={commands.accessTo(repository.project)}
				/>
			</div>
			{#if condition !== 'quarantined' && probe}
				<button
					type="button"
					aria-expanded={more}
					onclick={() => (more = !more)}
					class="inline-flex h-8.5 shrink-0 items-center gap-1.5 rounded-control border px-2.5 text-xs font-medium shadow-control focus-ring transition-colors hover:border-danger/40 hover:bg-danger/8 hover:text-danger {more
						? 'border-danger/40 bg-danger/8 text-danger'
						: 'border-border-control bg-surface text-text-2'}"
				>
					<span
						class="motion-safe:transition-transform motion-safe:duration-150 {more
							? 'rotate-90'
							: ''}"><Icon name="chevron" size={13} weight={2.25} /></span
					>{more ? 'Hide' : 'Quarantine…'}</button
				>
			{/if}
		</div>
		{#if more && condition !== 'quarantined' && probe}
			<div
				class="flex flex-col gap-2.5 rounded-control border border-danger/25 bg-danger/5 px-3 pt-2.5 pb-3"
			>
				<label class="flex items-center gap-2.5 text-xs font-medium text-text-2">
					Reason
					<input
						bind:value={reason}
						maxlength="120"
						class="h-8 min-w-0 flex-1 rounded-control border border-border-strong bg-surface px-2.5 text-compact font-normal text-text shadow-control focus-ring"
					/>
				</label>
				<CopyCommand
					command={commands.quarantine(repository.project, reason.trim() || 'Investigating')}
				/>
			</div>
		{/if}
	</footer>
</article>

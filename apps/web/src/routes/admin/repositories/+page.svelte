<script lang="ts">
	import { AdminRead, REPOSITORIES_PATH } from '$lib/admin/admin.svelte';
	import { commands } from '$lib/admin/commands';
	import { PROBE_WORDS, probeCondition, type ProbeCondition } from '$lib/admin/words';
	import type { AdminRepositories } from '$lib/api';
	import AppButton from '$lib/components/AppButton.svelte';
	import Notice from '$lib/components/ask/Notice.svelte';
	import CopyCommand from '$lib/components/admin/CopyCommand.svelte';
	import PageFrame from '$lib/components/admin/PageFrame.svelte';
	import RepositoryCard from '$lib/components/admin/RepositoryCard.svelte';
	import RunHint from '$lib/components/admin/RunHint.svelte';
	import StatTile from '$lib/components/admin/StatTile.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { describeMoment } from '$lib/format';
	import { session } from '$lib/session.svelte';

	const read = new AdminRead<AdminRepositories>();
	const admin = $derived(session.me?.user.role === 'admin');

	$effect(() => {
		if (admin) void read.load(REPOSITORIES_PATH);
	});

	const repositories = $derived(read.data?.repositories ?? []);
	// what needs a look first: a quarantine, then a silent probe, then one never registered
	const ORDER: readonly ProbeCondition[] = ['quarantined', 'stale', 'unregistered', 'healthy'];
	const sorted = $derived(
		[...repositories].sort(
			(a, b) =>
				ORDER.indexOf(probeCondition(a.probe)) - ORDER.indexOf(probeCondition(b.probe)) ||
				a.project.localeCompare(b.project)
		)
	);
	// the projects in each state, in the order the cards come: the strip names them, so it reads as
	// the cards' table of contents
	const named = $derived(
		Object.fromEntries(
			ORDER.map((c) => [
				c,
				sorted.filter((r) => probeCondition(r.probe) === c).map((r) => r.display_name ?? r.project)
			])
		) as Record<ProbeCondition, string[]>
	);

	/** The first few names of a state, and how many more there are. */
	function nameList(names: readonly string[]): string | undefined {
		if (names.length === 0) return undefined;
		const shown = names.slice(0, 3).join(', ');
		return names.length > 3 ? `${shown} +${names.length - 3}` : shown;
	}
</script>

<svelte:head>
	<title>Repositories · Admin · Mercury</title>
</svelte:head>

<main
	id="main"
	tabindex="-1"
	class="flex flex-col gap-4 px-2 py-2.5 focus:outline-none sm:px-3 lg:min-h-dvh"
>
	<PageFrame
		title="Repositories"
		description="Each project Mercury can answer about: whether its probe is healthy and fresh, how much of the repository it may read, and who may ask about it."
		icon="repo"
	>
		{#snippet summary()}
			{#if read.data}
				<span class="text-2xs font-medium text-text-2 tabular-nums"
					>{repositories.length}
					{repositories.length === 1 ? 'repository' : 'repositories'}</span
				>
				{#if read.readAt}<span aria-hidden="true" class="size-0.5 rounded-full bg-border-strong"
					></span><span class="flex items-center gap-1 text-2xs text-text-3 tabular-nums"
						><Icon name="clock" size={12} />Read {describeMoment(read.readAt)}</span
					>{/if}
			{/if}
		{/snippet}
		{#snippet actions()}
			<AppButton
				variant="quiet"
				onclick={() => void read.load(REPOSITORIES_PATH)}
				disabled={read.loading || !admin}
			>
				<span
					class="text-text-2 {read.loading
						? 'motion-safe:animate-spin motion-reduce:text-accent-text'
						: ''}"><Icon name="refresh" size={14} /></span
				>
				Refresh
			</AppButton>
		{/snippet}

		{#if !admin}
			<Notice title="Only an admin sees the admin panel" tone="info">
				<p class="text-text-2">Ask an admin about people, access or repositories.</p>
			</Notice>
		{:else}
			{#if read.failure}
				<Notice title={read.failure.message} tone="bad" alert>
					{#if read.failure.hint}<p class="text-text-2">{read.failure.hint}</p>{/if}
				</Notice>
			{/if}

			{#if read.data}
				<!-- the fleet at a glance, as one band in the order the cards come (what needs a look
				     first), each state naming its projects, and the command that lists every probe at once -->
				<div
					class="grid grid-cols-2 gap-px overflow-hidden rounded-card border border-border bg-border-subtle shadow-card sm:grid-cols-4 xl:grid-cols-[repeat(4,minmax(0,1fr))_minmax(0,1.55fr)]"
				>
					{#each ORDER as c (c)}
						<StatTile
							label={PROBE_WORDS[c].label}
							value={String(named[c].length)}
							tone={named[c].length > 0 ? PROBE_WORDS[c].tone : undefined}
							detail={nameList(named[c])}
						/>
					{/each}
					<div
						class="col-span-2 flex min-w-0 flex-col gap-2.5 bg-surface px-4 pt-3.5 pb-3.5 sm:col-span-4 xl:col-span-1"
					>
						<span class="flex items-center gap-1.5 text-xs font-medium text-text-2"
							><span class="text-text-3" aria-hidden="true"><Icon name="terminal" size={13} /></span
							>Every probe at once, in the terminal</span
						>
						<div class="mt-auto"><CopyCommand command={commands.probes()} /></div>
					</div>
				</div>

				{#if sorted.length === 0}
					<Notice title="No repository yet" tone="info">
						<p class="text-text-2">
							A project appears here once projects.yaml lists it or its probe registers.
						</p>
					</Notice>
				{:else}
					<!-- each card spans six rows of this grid and lays its parts on them (a subgrid), so the
					     cards of a row are as tall as each other and their parts line up -->
					<div
						class="grid items-stretch gap-4 transition-opacity md:grid-cols-2 2xl:grid-cols-3 {read.loading
							? 'opacity-60'
							: ''}"
						aria-busy={read.loading}
					>
						{#each sorted as repository (repository.project)}
							<RepositoryCard {repository} />
						{/each}
					</div>
				{/if}
			{:else if read.loading}
				<p class="text-sm text-text-3" role="status">Reading the repositories…</p>
			{/if}

			<RunHint
				what="This page shows each repository's probe and access, and changes nothing. What a probe may read is set in projects.yaml and the repository's manifest, on the server, and changes by pull request."
			/>
		{/if}
	</PageFrame>
</main>

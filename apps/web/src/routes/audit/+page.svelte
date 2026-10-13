<script lang="ts">
	import { onMount } from 'svelte';

	import type { AuditItem } from '$lib/api';
	import type { Tone } from '$lib/ask/words';
	import { AuditView } from '$lib/audit/audit.svelte';
	import { groupRows } from '$lib/audit/groups';
	import { CLASSES, exportPath, OUTCOMES } from '$lib/audit/query';
	import AppButton from '$lib/components/AppButton.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Chip from '$lib/components/ask/Chip.svelte';
	import Notice from '$lib/components/ask/Notice.svelte';
	import PageFrame from '$lib/components/admin/PageFrame.svelte';
	import { describeMoment, LOCALE } from '$lib/format';
	import { session } from '$lib/session.svelte';

	const view = new AuditView();
	const admin = $derived(session.me?.user.role === 'admin');
	const projects = $derived(session.me?.projects.map((g) => g.project) ?? []);

	// The audit is an admin's: nobody else's page asks the core for a row.
	onMount(() => {
		if (admin) void view.search();
	});

	function search(event: SubmitEvent) {
		event.preventDefault();
		void view.search();
	}

	let exportLink = $state<HTMLAnchorElement>();
	/** True for the one click a passed check makes: the browser follows that one and downloads. */
	let checked = false;

	// The export is the browser's download, which can't say why it failed: the filters are tried
	// first, and a refusal is put in words on this page instead of a failed download.
	async function exportRows(event: MouseEvent) {
		if (checked) return;
		event.preventDefault();
		if (!(await view.checkExport())) return;
		checked = true;
		exportLink?.click();
		checked = false;
	}

	// what a chain that holds is said as, in words that agree with its numbers: one row is "the one
	// row shown", never "each of the 1 rows"
	const holds = $derived.by(() => {
		if (view.chain.state !== 'holds') return '';
		const { rows, linked } = view.chain;
		const hashes =
			rows === 1
				? 'The one row shown matches its hash'
				: `Each of the ${rows} rows shown matches its hash`;
		if (linked === 0) return hashes;
		return linked === 1
			? `${hashes}, and the one that follows another row here links to it`
			: `${hashes}, and each of the ${linked} that follow another row here links to it`;
	});
	/** The row the chain breaks at, marked in the log. */
	const brokenAt = $derived(view.chain.state === 'broken' ? view.chain.seq : null);

	// The filters most used stay in view; the question's id and its kind open on request, and the
	// button says how many of them are set while they are folded away.
	let more = $state(false);
	const folded = $derived(
		[view.filters.question, view.filters.class].filter((value) => value.trim() !== '').length
	);

	// Each field named above it, one height for all; a select draws its own chevron, as the
	// composer's does.
	const label = 'flex min-w-0 flex-col gap-1.5';
	const name = 'text-xs font-medium text-text-2';
	const field =
		'h-8 w-full min-w-0 rounded-control border border-border-strong bg-surface px-2.5 text-compact text-text shadow-control transition-colors hover:border-text-3 focus-ring';
	const select = `${field} appearance-none truncate pr-8`;

	/**
	 * An event's badge tone, from its name alone: the judge's checks, an answer delivered, a question
	 * waiting on a person, one stopped. Every other row (received, routed, an admin's action) stays
	 * neutral.
	 */
	const TONES: Record<string, Tone> = {
		classified: 'info',
		tool_checked: 'info',
		judged: 'info',
		probe_judge: 'info',
		answer: 'good',
		done: 'good',
		pending_signoff: 'warn',
		clarify: 'warn',
		approval_expired: 'warn',
		budget_exceeded: 'warn',
		probe_quarantined: 'warn',
		refused: 'bad',
		error: 'bad',
		csrf_refused: 'bad'
	};
	const toneOf = (type: string): Tone => TONES[type] ?? 'neutral';

	// The rows that follow each other with one question under one header, the rows of no question
	// likewise, in the chain's order: a question the chain interleaves with another is headed again,
	// and the rows of the next page land after these.
	const groups = $derived(groupRows(view.items));

	function plural(count: number, one: string, many: string): string {
		return `${count} ${count === 1 ? one : many}`;
	}

	/** What the rows on screen add up to, for the line under the title. */
	const tally = $derived.by(() => {
		const items = view.items;
		const count = (type: string) => items.filter((item) => item.type === type).length;
		return {
			rows: items.length,
			questions: new Set(items.map((item) => item.question_id).filter(Boolean)).size,
			people: new Set(items.map((item) => item.user).filter(Boolean)).size,
			refused: count('refused'),
			failed: count('error'),
			held: count('pending_signoff')
		};
	});

	// A row's time to the second; the day too when it isn't the day its group started on.
	const clock = new Intl.DateTimeFormat(LOCALE, { timeStyle: 'medium' });
	const stamp = new Intl.DateTimeFormat(LOCALE, { dateStyle: 'medium', timeStyle: 'medium' });
	function timeOf(item: AuditItem, first: AuditItem): string {
		const at = new Date(item.at);
		return at.toDateString() === new Date(first.at).toDateString()
			? clock.format(at)
			: stamp.format(at);
	}

	/** The names a payload holds, for a glance before it is opened. */
	function fields(payload: object): string {
		const keys = Object.keys(payload);
		const shown = keys.slice(0, 4).join(' · ');
		return keys.length > 4 ? `${shown} +${keys.length - 4}` : shown;
	}

	// the log's columns, the same in every row so the badges and the times line up down the page:
	// number, the timeline's mark, event, what it recorded, time (on a narrow screen what it recorded
	// goes under the event); a question's header uses them too, its mark on the timeline's line
	const grid =
		'grid grid-cols-[2.25rem_0.75rem_auto_minmax(0,1fr)_auto] gap-x-3 gap-y-1 sm:grid-cols-[2.5rem_0.75rem_minmax(9rem,max-content)_minmax(0,1fr)_auto]';
	/** The timeline's line, the same in a question's header and its rows. */
	const line = 'absolute left-1/2 w-px -translate-x-1/2 bg-border-control';
	/** A question header's ground: the inset's tint, opaque, as it sticks over the rows going by. */
	const questionGround = 'bg-[color-mix(in_oklab,var(--surface-2)_55%,var(--surface))]';
</script>

<svelte:head>
	<title>Audit · Mercury</title>
</svelte:head>

{#snippet chevron()}
	<span class="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-text-3"
		><Icon name="chevron-down" size={14} /></span
	>
{/snippet}

<!-- what the rows on screen add up to, under the title: the counts in words, and a tone chip only for
     what waits on a person or went wrong -->
{#snippet tallies()}
	<span class="text-xs font-medium text-text-2 tabular-nums"
		>{plural(tally.rows, 'row', 'rows')}</span
	>
	{#if tally.questions > 0}<span aria-hidden="true" class="size-0.5 rounded-full bg-border-strong"
		></span><span class="text-xs text-text-2 tabular-nums"
			>{plural(tally.questions, 'question', 'questions')}</span
		>{/if}
	{#if tally.people > 0}<span aria-hidden="true" class="size-0.5 rounded-full bg-border-strong"
		></span><span class="text-xs text-text-2 tabular-nums"
			>{plural(tally.people, 'person', 'people')}</span
		>{/if}
	{#if tally.held + tally.refused + tally.failed > 0}
		<span class="ml-1.5 flex flex-wrap items-center gap-1.5">
			{#if tally.held > 0}<Chip tone="warn">{tally.held} held for sign-off</Chip>{/if}
			{#if tally.refused > 0}<Chip tone="bad">{tally.refused} refused</Chip>{/if}
			{#if tally.failed > 0}<Chip tone="bad">{tally.failed} failed</Chip>{/if}
		</span>
	{/if}
{/snippet}

<!-- the page's download, level with its title: every row the filters on screen match -->
{#snippet download()}
	{#if view.exporting}<span class="text-compact text-text-2">Checking the export…</span>{/if}
	<!-- eslint-disable svelte/no-navigation-without-resolve -- a download the core serves, not a page of the app -->
	<a
		bind:this={exportLink}
		href={exportPath(view.applied)}
		download
		onclick={exportRows}
		aria-disabled={view.exporting}
		class="group/export inline-flex min-h-8 items-center gap-1.5 rounded-control border border-border-control bg-surface px-3 py-1 text-compact font-medium text-text shadow-control focus-ring transition-colors hover:border-border-strong hover:bg-surface-2 aria-disabled:cursor-wait aria-disabled:opacity-60"
		><svg
			width="15"
			height="15"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="1.9"
			stroke-linecap="round"
			stroke-linejoin="round"
			aria-hidden="true"
			focusable="false"
			class="text-text-2 transition-colors group-hover/export:text-text"
			><path d="M12 4v11M7.5 10.5 12 15l4.5-4.5M5 19.5h14" /></svg
		>Export every matching row
		<span class="font-normal text-text-2">(JSON lines)</span></a
	>
	<!-- eslint-enable svelte/no-navigation-without-resolve -->
{/snippet}

<!-- the frame reaches the window's foot, however few the rows -->
<main
	id="main"
	tabindex="-1"
	class="flex flex-col px-2 py-2.5 focus:outline-none sm:px-3 lg:min-h-dvh"
>
	<PageFrame
		title="Audit"
		description="Every step of every question is a row of the audit, written before anyone saw it, each chained to the one before."
		icon="audit"
		summary={admin && view.loaded && view.items.length > 0 ? tallies : undefined}
		actions={admin && view.loaded ? download : undefined}
	>
		{#if !admin}
			<Notice title="Only an admin reads the audit" tone="info">
				<p class="text-text-2">What Mercury did with your question is on its page in Ask.</p>
			</Notice>
		{:else}
			{#if view.failure}
				<Notice title={view.failure.message} tone="bad" alert>
					{#if view.failure.hint}<p class="text-text-2">{view.failure.hint}</p>{/if}
				</Notice>
			{/if}

			<!-- why the browser downloaded nothing: the export's filters were refused before it started -->
			{#if view.exportFailure}
				<Notice title="The export didn't start" tone="bad" alert>
					<p class="text-text-2">
						{view.exportFailure.message}{#if view.exportFailure.hint}
							{view.exportFailure.hint}{/if}
					</p>
				</Notice>
			{/if}

			<!-- one panel for the log, as tall as the frame leaves it: the filters across its top, then the
			     chain's state, then the rows, question by question, and the log's foot at its bottom.
			     It clips without scrolling, so a question's header can stick while its rows go by. -->
			<div
				class="flex flex-1 flex-col overflow-clip rounded-card border border-border bg-surface shadow-card"
			>
				<!-- one toolbar: the filters most used in a row, the dates side by side, the actions at its
				     end; the question's id and its kind open beneath on request -->
				<form
					onsubmit={search}
					aria-label="Filters"
					class="grid grid-cols-1 gap-3 px-4 pt-3 pb-3.5 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-[7.5rem_minmax(9rem,1fr)_9rem_7.75rem_7.75rem_auto] xl:items-end"
				>
					<label class={label}>
						<span class={name}>Project</span>
						<span class="relative flex">
							<select bind:value={view.filters.project} class={select}>
								<option value="">Any</option>
								{#each projects as project (project)}<option value={project}>{project}</option
									>{/each}
							</select>
							{@render chevron()}
						</span>
					</label>
					<label class={label}>
						<span class={name}>Person (e-mail)</span>
						<input type="email" autocomplete="off" bind:value={view.filters.user} class={field} />
					</label>
					<label class={label}>
						<span class={name}>How it ended</span>
						<span class="relative flex">
							<select bind:value={view.filters.outcome} class={select}>
								<option value="">Any</option>
								{#each OUTCOMES as o (o)}<option value={o}>{o.replaceAll('_', ' ')}</option>{/each}
							</select>
							{@render chevron()}
						</span>
					</label>
					<label class={label}>
						<span class={name}>From</span>
						<input type="date" bind:value={view.filters.from} class="{field} tabular-nums" />
					</label>
					<label class={label}>
						<span class={name}>Before</span>
						<input type="date" bind:value={view.filters.to} class="{field} tabular-nums" />
					</label>
					<div
						class="flex flex-col items-start gap-1.5 sm:col-span-2 lg:col-span-1 lg:items-end xl:col-span-1"
					>
						<button
							type="button"
							aria-expanded={more}
							aria-controls="audit-more-filters"
							onclick={() => (more = !more)}
							class="-mx-1 inline-flex h-4.5 items-center gap-1 rounded-chip px-1 text-xs font-medium text-text-2 focus-ring transition-colors hover:text-text"
						>
							More filters
							{#if folded > 0 && !more}<span
									class="grid h-4 min-w-4 place-items-center rounded-full bg-accent-soft px-1 text-2xs leading-none text-accent-text tabular-nums"
									>{folded}<span class="sr-only">&nbsp;set</span></span
								>{/if}
							<span
								class="text-text-3 motion-safe:transition-transform motion-safe:duration-150 {more
									? 'rotate-180'
									: ''}"><Icon name="chevron-down" size={13} weight={2.25} /></span
							>
						</button>
						<div class="flex flex-wrap items-center gap-2">
							<AppButton variant="quiet" disabled={view.loading} onclick={() => view.clear()}
								>Clear filters</AppButton
							>
							<AppButton type="submit" disabled={view.loading}>Show rows</AppButton>
						</div>
					</div>
					<!-- under the first three filters, in their columns: the id under Project and Person -->
					<div
						id="audit-more-filters"
						hidden={!more}
						class="grid grid-cols-1 gap-3 sm:col-span-2 sm:grid-cols-2 lg:col-span-3 lg:grid-cols-subgrid"
					>
						<label class="{label} lg:col-span-2">
							<span class={name}>Question id</span>
							<input
								autocomplete="off"
								bind:value={view.filters.question}
								class="{field} font-mono text-xs"
							/>
						</label>
						<label class={label}>
							<span class={name}>Kind of question</span>
							<span class="relative flex">
								<select bind:value={view.filters.class} class={select}>
									<option value="">Any</option>
									{#each CLASSES as c (c)}<option value={c}>{c}</option>{/each}
								</select>
								{@render chevron()}
							</span>
						</label>
					</div>
				</form>

				{#if view.loaded}
					<!-- the log: the chain's state on top, then the rows, question by question -->
					<section aria-label="Audit log" class="flex flex-1 flex-col border-t border-border">
						{#if view.chain.state === 'holds'}
							<div
								role="status"
								class="flex items-start gap-3 border-b border-border-subtle bg-success/5 px-4 py-3"
							>
								<!-- the verified mark: a shield with a check -->
								<svg
									width="22"
									height="22"
									viewBox="0 0 24 24"
									aria-hidden="true"
									focusable="false"
									class="mt-px shrink-0"
									><path
										class="fill-success"
										d="M12 2.5 19.5 5.6v5.9c0 4.6-3.1 8.6-7.5 9.9-4.4-1.3-7.5-5.3-7.5-9.9V5.6L12 2.5Z"
									/><path
										class="stroke-surface"
										d="m8.6 12.1 2.3 2.3 4.5-4.6"
										fill="none"
										stroke-width="2.1"
										stroke-linecap="round"
										stroke-linejoin="round"
									/></svg
								>
								<div class="flex min-w-0 flex-col gap-0.5">
									<h2 class="text-compact font-semibold text-text">
										The chain holds for these rows
									</h2>
									<p class="text-xs/5 text-pretty text-text-2">
										{holds}. The whole chain is checked by an admin with
										<code
											class="rounded-chip bg-surface-2 px-1.5 py-px font-mono text-2xs whitespace-nowrap text-text ring-1 ring-border-subtle ring-inset"
											>mercury-core admin audit verify</code
										>.
									</p>
								</div>
							</div>
						{:else if view.chain.state === 'broken'}
							<div
								role="alert"
								class="flex items-start gap-3 border-b border-border-subtle bg-danger/6 px-4 py-3"
							>
								<span
									aria-hidden="true"
									class="mt-px grid size-5.5 shrink-0 place-items-center rounded-full bg-danger text-surface"
									><Icon name="close" size={12} weight={2.75} /></span
								>
								<div class="flex min-w-0 flex-col gap-0.5">
									<h2 class="text-compact font-semibold text-text">
										The chain is broken at row {view.chain.seq}
									</h2>
									<p class="text-xs/5 text-pretty text-text-2">
										{view.chain.what === 'hash'
											? "That row doesn't match its hash: it was changed after it was written."
											: "That row doesn't follow the row before it: a row between them is missing or was changed."}
										Tell your admin.
									</p>
								</div>
							</div>
						{:else}
							<!-- nothing matched: said in the middle of where the rows would be -->
							<div
								class="flex flex-1 flex-col items-center justify-center gap-3 px-4 py-12 text-center"
							>
								<span
									aria-hidden="true"
									class="grid size-10 place-items-center rounded-full bg-surface-2 text-text-3 ring-1 ring-border ring-inset"
									><Icon name="search" size={18} /></span
								>
								<p class="text-sm text-text-2">No rows match these filters.</p>
							</div>
						{/if}

						{#if groups.length > 0}
							<!-- the columns of every group, named once -->
							<div
								aria-hidden="true"
								class="hidden border-b border-border-subtle px-4 py-1.5 text-2xs font-medium text-text-3 sm:grid {grid}"
							>
								<span class="text-right">#</span><span></span><span>Event</span><span></span><span
									class="text-right">Time</span
								>
							</div>
							<ol class="flex flex-col" aria-label="Audit rows">
								{#each groups as group (group.key)}
									{@const first = group.items[0]}
									<li class="border-b border-border-subtle last:border-b-0">
										<!-- the question once, its mark at the head of its rows' line: its id, project
										     and person, how many rows, when it began; it stays in view while its rows
										     scroll by (on an opaque ground, under the phone's top bar) -->
										<div
											id="audit-{group.key}"
											class="{grid} sticky top-14 z-10 items-center border-b border-border-subtle px-4 py-2 lg:top-0 {questionGround}"
										>
											<span
												aria-hidden="true"
												class="relative col-start-2 row-start-1 flex justify-center self-stretch"
											>
												<span class="{line} top-2.5 -bottom-2"></span>
												<span
													class="relative grid size-5 shrink-0 place-items-center rounded-full bg-surface text-text-2 ring-1 ring-border-control ring-inset"
													><Icon
														name={group.question ? 'chat' : 'audit'}
														size={11}
														weight={2.1}
													/></span
												>
											</span>
											<div
												class="col-span-2 col-start-3 row-start-1 flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1"
											>
												{#if group.question}
													<span class="min-w-0 font-mono text-xs font-semibold break-all text-text"
														><span class="sr-only">Question id </span>{group.question}</span
													>
												{:else}
													<span class="text-xs font-medium text-text-2">Not part of a question</span
													>
												{/if}
												{#if group.project}<span
														class="rounded-chip bg-surface px-1.5 font-mono text-2xs leading-5 text-text-2 ring-1 ring-border-control ring-inset"
														><span class="sr-only">Project </span>{group.project}</span
													>{/if}
												{#if group.user}<span
														class="inline-flex min-w-0 items-center gap-1.5 text-xs break-all text-text-2"
														><span aria-hidden="true" class="shrink-0 text-text-3"
															><Icon name="user" size={13} /></span
														><span class="sr-only">Person </span>{group.user}</span
													>{/if}
											</div>
											<span
												class="col-start-5 row-start-1 flex items-center justify-end gap-x-2 text-xs whitespace-nowrap tabular-nums"
												><span class="text-2xs text-text-3"
													>{plural(group.items.length, 'row', 'rows')}</span
												><span aria-hidden="true" class="size-0.5 rounded-full bg-border-strong"
												></span><time datetime={first.at} class="text-text-2"
													>{describeMoment(first.at)}</time
												></span
											>
										</div>
										<!-- its rows as a timeline: the line comes down from the question's mark, joins
										     the rows' marks and stops at the last; each row its number and event -->
										<ol aria-labelledby="audit-{group.key}" class="flex flex-col py-1">
											{#each group.items as item, index (item.seq)}
												{@const broken = item.seq === brokenAt}
												{@const head = index === 0}
												{@const tail = index === group.items.length - 1}
												<li
													class="{grid} px-4 py-1.5 text-xs/normal transition-colors {broken
														? 'bg-danger/6'
														: 'hover:bg-surface-2/60'}"
												>
													<span
														class="col-start-1 row-start-1 text-right font-mono text-2xs leading-5 text-text-3 tabular-nums"
														>#{item.seq}</span
													>
													<!-- the mark sits level with the badge's middle; the line runs through the
													     row's padding (and the list's, above the first) to the marks around it -->
													<span
														aria-hidden="true"
														class="relative col-start-2 row-span-2 row-start-1 flex justify-center sm:row-span-1"
													>
														<span
															class="{line} {head ? '-top-2.75' : '-top-1.5'} {tail
																? head
																	? 'h-5.25'
																	: 'h-4'
																: '-bottom-1.5'}"
														></span>
														<span
															class="relative mt-1.75 size-1.5 rounded-full {broken
																? 'bg-danger ring-2 ring-danger/25'
																: 'bg-border-strong'}"
														></span>
													</span>
													<span class="col-start-3 row-start-1 flex min-w-0"
														><Chip tone={toneOf(item.type)}
															><span class="font-mono">{item.type}</span></Chip
														></span
													>
													<div
														class="col-span-3 col-start-3 row-start-2 flex min-w-0 flex-col gap-1 sm:col-span-1 sm:col-start-4 sm:row-start-1"
													>
														{#if (item.project && item.project !== group.project) || (item.user && item.user !== group.user)}
															<p class="flex flex-wrap items-center gap-x-2 leading-5 text-text-2">
																{#if item.project && item.project !== group.project}<span
																		class="rounded-chip bg-surface-2 px-1.5 font-mono text-2xs text-text-2 ring-1 ring-border-subtle ring-inset"
																		><span class="sr-only">Project </span>{item.project}</span
																	>{/if}
																{#if item.user && item.user !== group.user}<span class="break-all"
																		><span class="sr-only">Person </span>{item.user}</span
																	>{/if}
															</p>
														{/if}
														{#if item.payload === null}
															<p class="flex items-center gap-1.5 leading-5 text-text-3">
																<span aria-hidden="true" class="shrink-0"
																	><Icon name="clock" size={13} /></span
																>Payload expired: retention removed it; its digest stays.
															</p>
														{:else}
															<details class="group min-w-0">
																<!-- 24px to the pointer, 20px in the row: level with the badge -->
																<summary
																	class="-mx-1.5 -my-0.5 flex h-6 w-fit max-w-[calc(100%+0.75rem)] min-w-0 cursor-pointer list-none items-center gap-1 rounded-chip px-1.5 font-medium text-text-2 focus-ring transition-colors group-open:text-text hover:bg-surface-2 hover:text-text [&::-webkit-details-marker]:hidden"
																	><span
																		class="shrink-0 text-text-3 group-open:rotate-90 motion-safe:transition-transform motion-safe:duration-150"
																		><Icon name="chevron" size={12} weight={2.25} /></span
																	><span class="shrink-0">What it recorded</span
																	>{#if fields(item.payload)}<span
																			class="ml-1.5 truncate font-mono text-2xs font-normal text-text-3 group-open:hidden"
																			>{fields(item.payload)}</span
																		>{/if}</summary
																>
																<!-- a long line scrolls; a browser that lets the keyboard into it shows the app's ring -->
																<pre
																	class="mt-1.5 mb-1 max-h-80 overflow-auto rounded-control border border-border-subtle bg-surface-2 px-3 py-2 font-mono text-2xs leading-relaxed text-text focus-ring">{JSON.stringify(
																		item.payload,
																		null,
																		2
																	)}</pre>
															</details>
														{/if}
													</div>
													<time
														datetime={item.at}
														title={describeMoment(item.at)}
														class="col-start-5 row-start-1 text-right text-xs leading-5 whitespace-nowrap text-text-3 tabular-nums"
														>{timeOf(item, first)}</time
													>
												</li>
											{/each}
										</ol>
									</li>
								{/each}
							</ol>

							<!-- the log's foot: the next page on request, or word that there is none -->
							<div
								class="mt-auto flex items-center border-t border-border-subtle bg-surface-2/40 px-4 {view.next
									? 'py-2.5'
									: 'py-2'}"
							>
								{#if view.next}
									<AppButton variant="quiet" disabled={view.loading} onclick={() => view.more()}
										>Show more rows</AppButton
									>
								{:else}
									<p class="flex items-center gap-1.5 text-2xs text-text-3">
										<span aria-hidden="true"><Icon name="check" size={12} weight={2.25} /></span
										>Every matching row is shown
									</p>
								{/if}
							</div>
						{/if}
					</section>
				{:else if view.loading}
					<p class="border-t border-border-subtle px-4 py-4 text-sm text-text-2">
						Reading the audit…
					</p>
				{/if}
			</div>
		{/if}
	</PageFrame>
</main>

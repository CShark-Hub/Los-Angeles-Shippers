<script lang="ts">
	import { tick } from 'svelte';

	import { AdminRead, PEOPLE_PATH } from '$lib/admin/admin.svelte';
	import { ADMIN_SHELL, commands } from '$lib/admin/commands';
	import { approves, filterPeople, noFilter, summarize } from '$lib/admin/people';
	import { tokenChip } from '$lib/admin/words';
	import type { AdminPeople, AdminPerson, Role } from '$lib/api';
	import { DOT, type Tone } from '$lib/ask/words';
	import AppButton from '$lib/components/AppButton.svelte';
	import Notice from '$lib/components/ask/Notice.svelte';
	import Avatar from '$lib/components/admin/Avatar.svelte';
	import Card from '$lib/components/admin/Card.svelte';
	import CopyCommand from '$lib/components/admin/CopyCommand.svelte';
	import PageFrame from '$lib/components/admin/PageFrame.svelte';
	import PersonDetail from '$lib/components/admin/PersonDetail.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { describeAge, describeMoment, roleLabel } from '$lib/format';
	import { session } from '$lib/session.svelte';

	const read = new AdminRead<AdminPeople>();
	const admin = $derived(session.me?.user.role === 'admin');

	// one read when the screen opens; Refresh reads again (each read is an audit row)
	$effect(() => {
		if (admin) void read.load(PEOPLE_PATH);
	});

	const filter = $state(noFilter());
	const people = $derived(read.data?.people ?? []);
	const shown = $derived(filterPeople(people, filter));
	const tally = $derived(summarize(people));
	const projects = $derived(read.data?.projects ?? []);
	// the busiest person's questions, the full length of the bar beside each count
	const busiest = $derived(Math.max(1, ...people.map((p) => p.questions_30d)));
	let selectedId = $state<string | null>(null);
	const selected = $derived(people.find((p) => p.id === selectedId) ?? shown[0] ?? null);

	const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

	// what each figure comes to, in a few words under it: who asked lately, the tokens that run out
	// within a week, the projects someone signs off on
	const current = $derived(people.filter((p) => p.disabled_at === null));
	const asked = $derived(people.filter((p) => p.questions_30d > 0).length);
	const expiring = $derived(
		current.reduce(
			(n, p) =>
				n + p.tokens.filter((t) => t.state === 'active' && tokenChip(t).tone === 'warn').length,
			0
		)
	);
	const holding = $derived(
		current.filter((p) => p.tokens.some((t) => t.state === 'active')).length
	);
	const approved = $derived([
		...new Set(current.flatMap((p) => p.grants.filter((g) => approves(p, g)).map((g) => g.project)))
	]);

	// the five figures over the list: who is here, what they hold, and who needs a look (a dot of the
	// tone beside the words, as on the overview's tiles, and a faint wash on the one to look at)
	interface Figure {
		label: string;
		value: number;
		tone?: Tone;
		detail?: string;
		/** The detail is the part to look at: a dot of the warning's colour before it. */
		heed?: boolean;
	}
	const figures = $derived<Figure[]>([
		{ label: 'People', value: tally.people, detail: `${asked} asked in 30 days` },
		{
			label: 'Active tokens',
			value: tally.activeTokens,
			tone: 'good',
			detail:
				expiring > 0
					? `${expiring} ${expiring === 1 ? 'expires' : 'expire'} soon`
					: holding > 0
						? `held by ${plural(holding, 'person', 'people')}`
						: undefined,
			heed: expiring > 0
		},
		{
			label: 'Approvers',
			value: tally.approvers,
			tone: 'info',
			detail: approved.length > 0 ? `on ${approved.join(', ')}` : undefined
		},
		{
			label: 'Without a project',
			value: tally.withoutProject,
			tone: tally.withoutProject > 0 ? 'warn' : undefined,
			detail: tally.withoutProject > 0 ? 'questions refused' : undefined
		},
		{
			label: 'Disabled',
			value: tally.disabled,
			detail: tally.disabled > 0 ? "can't sign in" : undefined
		}
	]);

	// The other way round from the list: each project and the active people who may ask about it,
	// its approvers marked. A press shows only its people in the list.
	interface Holder {
		person: AdminPerson;
		approver: boolean;
	}
	const byProject = $derived(
		[
			...new Set([
				...projects,
				'portfolio',
				...people.flatMap((p) => p.grants.map((g) => g.project))
			])
		].map((project) => {
			const holders: Holder[] = [];
			let unread = false;
			for (const person of people) {
				const grant = person.grants.find((g) => g.project === project);
				if (!grant || person.disabled_at !== null) continue;
				holders.push({ person, approver: approves(person, grant) });
				unread ||= grant.classes === null;
			}
			return {
				project,
				holders,
				approvers: holders.filter((h) => h.approver).length,
				noManifest: (read.data?.classes_known ?? false) && unread
			};
		})
	);
	// how many of a project's people show as badges; the rest are counted beside them
	const SHOWN_HOLDERS = 6;
	// a command named in the read-only line, as a small code chip that keeps its shape when it wraps
	const codeChip =
		'rounded-chip border border-border-subtle bg-surface px-1 py-px font-mono text-2xs text-text-2 box-decoration-clone';

	let detail = $state<HTMLElement>();
	async function pick(id: string) {
		selectedId = id;
		// on a narrow screen the detail is under the list: take the person there
		if (window.matchMedia('(max-width: 1279px)').matches) {
			await tick();
			const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
			detail?.scrollIntoView({ behavior: still ? 'auto' : 'smooth', block: 'start' });
		}
	}

	let adding = $state(false);
	let newEmail = $state('');
	let newName = $state('');
	let newRole = $state<Role>('pm');

	const ROLES: readonly Role[] = ['pm', 'tech_lead', 'board', 'admin'];
	const SHORT_ROLE: Record<Role, string> = {
		pm: 'PM',
		tech_lead: 'Tech lead',
		board: 'Board',
		admin: 'Admin'
	};
	// a field: 32px, the strong edge every field keeps, its value at the size of a dense row
	const field =
		'h-8 rounded-control border border-border-strong bg-surface px-2.5 text-compact text-text focus-ring transition-colors hover:bg-surface-2';
	// a filter: the same field with the app's chevron; one that narrows the list wears the accent
	function filterField(narrows: boolean): string {
		return `h-8 appearance-none rounded-control border pr-8 pl-2.5 text-compact focus-ring transition-colors ${
			narrows
				? 'border-accent bg-accent-soft font-medium text-accent-text'
				: 'border-border-strong bg-surface text-text hover:bg-surface-2'
		}`;
	}
</script>

<svelte:head>
	<title>People & access · Admin · Mercury</title>
</svelte:head>

{#snippet chevron(narrows: boolean)}<span
		class="pointer-events-none absolute right-2.5 {narrows ? 'text-accent-text' : 'text-text-3'}"
		aria-hidden="true"><Icon name="chevron-down" size={14} /></span
	>{/snippet}

<main
	id="main"
	tabindex="-1"
	class="flex flex-col px-2 py-2.5 focus:outline-none sm:px-3 lg:min-h-dvh"
>
	<PageFrame
		title="People & access"
		description="Who may ask Mercury about what: each person's role, the projects they are granted, what each grant lets them ask, and their tokens."
		icon="users"
	>
		{#snippet summary()}
			{#if read.data && read.readAt}<span
					class="inline-flex items-center gap-1.5 text-xs text-text-3 tabular-nums"
					><Icon name="clock" size={13} />Read {describeMoment(read.readAt)}</span
				>{/if}
		{/snippet}
		{#snippet actions()}
			<!-- the page's one action: opens the three commands that add someone; its mark turns to a close -->
			<button
				type="button"
				aria-expanded={adding}
				onclick={() => (adding = !adding)}
				disabled={!admin}
				class="group inline-flex min-h-8 items-center gap-1.5 rounded-control border border-transparent bg-accent px-3 py-1 text-compact font-medium text-on-accent shadow-accent focus-ring transition-[background-color,transform] duration-150 hover:bg-accent-hover disabled:opacity-50 disabled:shadow-none aria-expanded:bg-accent-hover motion-safe:active:translate-y-px"
			>
				<span
					class="grid place-items-center group-aria-expanded:rotate-45 motion-safe:transition-transform motion-safe:duration-200"
					><Icon name="plus" size={15} weight={2.2} /></span
				> Add a person
			</button>
			<AppButton
				variant="quiet"
				onclick={() => void read.load(PEOPLE_PATH)}
				disabled={read.loading || !admin}
			>
				<span
					class="text-text-3 {read.loading
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
			{#if adding}
				<section
					aria-label="Add a person"
					class="flex flex-col gap-3 rounded-card border border-border bg-surface px-4 pt-3.5 pb-4 shadow-card"
				>
					<h2 class="text-sm font-semibold text-text">Add a person</h2>
					<div class="flex flex-wrap items-end gap-3">
						<label class="flex flex-col gap-1.5 text-xs font-medium text-text-2">
							E-mail address
							<input
								type="email"
								bind:value={newEmail}
								placeholder="name@example.com"
								class="{field} w-64 font-normal"
							/>
						</label>
						<label class="flex flex-col gap-1.5 text-xs font-medium text-text-2">
							Name
							<input
								bind:value={newName}
								placeholder="Their name"
								class="{field} w-52 font-normal"
							/>
						</label>
						<label class="flex flex-col gap-1.5 text-xs font-medium text-text-2">
							Role
							<span class="relative inline-flex items-center">
								<select bind:value={newRole} class="{field} appearance-none pr-8 font-normal">
									{#each ROLES as role (role)}<option value={role}>{roleLabel(role)}</option>{/each}
								</select>
								{@render chevron(false)}
							</span>
						</label>
					</div>
					<div class="grid gap-3 border-t border-border-subtle pt-3 lg:grid-cols-3">
						<CopyCommand
							label="1. Add them"
							command={commands.addPerson(
								newEmail.trim() || 'name@example.com',
								newName.trim() || 'Their name',
								newRole
							)}
						/>
						<CopyCommand
							label="2. Grant a project"
							command={commands.grant(newEmail.trim() || 'name@example.com', [
								projects[0] ?? 'delta'
							])}
						/>
						<CopyCommand
							label="3. Give them a token"
							command={commands.createToken(newEmail.trim() || 'name@example.com', 'laptop')}
						/>
					</div>
				</section>
			{/if}

			{#if read.failure}
				<Notice title={read.failure.message} tone="bad" alert>
					{#if read.failure.hint}<p class="text-text-2">{read.failure.hint}</p>{/if}
				</Notice>
			{/if}

			{#if read.data}
				{#if !read.data.classes_known}
					<Notice title="What each grant lets a person ask can't be shown" tone="warn">
						<p class="text-text-2">The core can't read policies.yaml or projects.yaml right now.</p>
					</Notice>
				{/if}

				<!-- on the left the figures, the list and the projects; on the right the person picked from the
				     list, in a raised panel as tall as the left column (never taller than the window), held in
				     view while the page scrolls, and scrolling on its own when it holds more -->
				<div
					class="grid gap-4 transition-opacity xl:grid-cols-[minmax(0,1fr)_360px] min-[87.5rem]:grid-cols-[minmax(0,1fr)_380px] 2xl:grid-cols-[minmax(0,1fr)_420px] {read.loading
						? 'opacity-60'
						: ''}"
					aria-busy={read.loading}
				>
					<div class="flex min-w-0 flex-col gap-4 xl:min-h-[34rem]">
						<!-- a band of five cells; the line under each figure shows when the band has room for it -->
						<dl
							class="@container grid grid-cols-2 gap-px overflow-hidden rounded-card border border-border bg-border-subtle shadow-card sm:grid-cols-[repeat(5,minmax(max-content,1fr))]"
						>
							{#each figures as figure (figure.label)}
								<div
									class="flex min-w-0 flex-col bg-surface px-4 pt-3 pb-3 {figure.tone === 'warn'
										? 'bg-linear-to-b from-warning/9 to-transparent to-70%'
										: ''}"
								>
									<dt class="flex items-center gap-1.5 text-xs font-medium text-text-2">
										{#if figure.tone}<span
												class="size-1.5 shrink-0 rounded-full {DOT[figure.tone]}"
												aria-hidden="true"
											></span>{/if}
										<span class="truncate">{figure.label}</span>
									</dt>
									<dd
										class="mt-1.5 text-figure leading-7 font-semibold tracking-tight text-text tabular-nums"
									>
										{figure.value}
									</dd>
									<!-- as wide as the cell, never wider: the words never widen the band -->
									{#if figure.detail}<dd
											class="hidden w-0 min-w-full items-center gap-1.5 pt-0.5 text-2xs leading-4 @[40rem]:flex {figure.heed
												? 'text-text-2'
												: 'text-text-3'}"
										>
											{#if figure.heed}<span
													class="size-1.5 shrink-0 rounded-full bg-warning"
													aria-hidden="true"
												></span>{/if}
											<span class="truncate">{figure.detail}</span>
										</dd>{/if}
								</div>
							{/each}
						</dl>

						<section
							class="@container flex min-w-0 flex-col overflow-hidden rounded-card border border-border bg-surface shadow-card"
							aria-label="People"
						>
							<div class="flex flex-wrap items-center gap-2 px-4 py-3">
								<label
									class="relative flex w-full min-w-48 items-center @[40rem]:w-auto @[40rem]:flex-1"
								>
									<span class="pointer-events-none absolute left-2.5 text-text-3"
										><Icon name="search" size={15} /></span
									>
									<input
										type="search"
										bind:value={filter.query}
										placeholder="Search by name or e-mail"
										aria-label="Search people"
										class="{field} w-full pl-8"
									/>
								</label>
								<span class="relative inline-flex items-center">
									<select
										bind:value={filter.role}
										aria-label="Role"
										class={filterField(filter.role !== 'all')}
									>
										<option value="all">Every role</option>
										{#each ROLES as role (role)}<option value={role}>{roleLabel(role)}</option
											>{/each}
									</select>
									{@render chevron(filter.role !== 'all')}
								</span>
								<span class="relative inline-flex items-center">
									<select
										bind:value={filter.project}
										aria-label="Project"
										class={filterField(filter.project !== 'all')}
									>
										<option value="all">Any project</option>
										{#each [...projects, 'portfolio'] as p (p)}<option value={p}>{p}</option>{/each}
									</select>
									{@render chevron(filter.project !== 'all')}
								</span>
								<span class="relative inline-flex items-center">
									<select
										bind:value={filter.status}
										aria-label="Status"
										class={filterField(filter.status !== 'all')}
									>
										<option value="all">Active and disabled</option>
										<option value="active">Active</option>
										<option value="disabled">Disabled</option>
									</select>
									{@render chevron(filter.status !== 'all')}
								</span>
								{#if shown.length !== people.length}<span
										class="inline-flex h-6 items-center rounded-chip bg-accent-soft px-2 text-xs font-medium text-accent-text tabular-nums"
										aria-hidden="true">{shown.length} of {people.length}</span
									>{/if}
							</div>

							<div class="overflow-x-auto">
								<table class="w-full text-left text-compact">
									<caption class="sr-only">People, {shown.length} of {people.length}</caption>
									<thead class="text-xs text-text-3">
										<tr class="border-y border-border-subtle bg-surface-2/60">
											<th scope="col" class="h-9 py-0 pr-2.5 pl-4 font-medium">Person</th>
											<th scope="col" class="h-9 px-2.5 py-0 font-medium">Role</th>
											<th scope="col" class="h-9 px-2.5 py-0 font-medium">Projects</th>
											<th scope="col" class="hidden h-9 px-2.5 py-0 font-medium @[34rem]:table-cell"
												>Tokens</th
											>
											<th scope="col" class="hidden h-9 px-2.5 py-0 font-medium @[42rem]:table-cell"
												>Last asked</th
											>
											<th
												scope="col"
												class="hidden h-9 py-0 pr-4 pl-2.5 text-right font-medium @md:table-cell"
												>30 days</th
											>
										</tr>
									</thead>
									<tbody class="divide-y divide-border-subtle">
										{#each shown as person (person.id)}
											{@const active = person.tokens.filter((t) => t.state === 'active')}
											{@const soon = active.some((t) => tokenChip(t).tone === 'warn')}
											{@const isSelected = selected?.id === person.id}
											{@const share = person.questions_30d / busiest}
											<tr
												class="cursor-pointer transition-colors {isSelected
													? 'bg-accent-soft'
													: 'hover:bg-surface-2/70'} {person.disabled_at ? 'text-text-3' : ''}"
												onclick={() => pick(person.id)}
											>
												<td class="relative py-2.5 pr-2.5 pl-4">
													{#if isSelected}<span
															aria-hidden="true"
															class="absolute inset-y-2 left-0 w-0.75 rounded-r-full bg-accent"
														></span>{/if}
													<button
														type="button"
														onclick={(e) => {
															e.stopPropagation();
															void pick(person.id);
														}}
														aria-pressed={isSelected}
														class="flex max-w-60 min-w-0 items-center gap-3 rounded-control text-left focus-ring"
													>
														<Avatar
															name={person.display_name}
															size={32}
															muted={person.disabled_at !== null}
															selected={isSelected}
														/>
														<span class="flex min-w-0 flex-col">
															<span
																class="truncate leading-5 font-medium {person.disabled_at
																	? 'text-text-2 line-through decoration-text-3'
																	: 'text-text'}">{person.display_name}</span
															>
															<span class="truncate text-xs text-text-3">{person.email}</span>
														</span>
													</button>
												</td>
												<td class="px-2.5 py-2.5 whitespace-nowrap">
													<span class="text-text-2" title={roleLabel(person.role)}
														>{SHORT_ROLE[person.role]}</span
													>
												</td>
												<td class="px-2.5 py-2.5">
													{#if person.grants.length === 0}
														<span
															class="inline-flex h-5.5 items-center gap-1 rounded-chip bg-warning/10 pr-1.5 pl-1 text-2xs font-medium text-warning ring-1 ring-warning/30 ring-inset"
															><Icon name="alert" size={12} />None</span
														>
													{:else}
														<span class="flex flex-wrap gap-1">
															{#each person.grants as g (g.project)}
																{@const approver = approves(person, g)}
																<!-- a project; an approver's in the approvers' blue led by a shield, a
																     flag that grants nothing in the warning's colour -->
																<span
																	class="inline-flex h-5.5 items-center gap-1 rounded-chip font-mono text-2xs ring-1 ring-inset {approver ||
																	g.approver
																		? 'pr-1.5 pl-1'
																		: 'px-1.5'} {approver
																		? 'bg-info/10 text-text ring-info/30'
																		: g.approver
																			? 'bg-warning/10 text-text ring-warning/30'
																			: 'bg-surface-2 text-text-2 ring-border'}"
																	>{#if approver}<span class="text-info" title="Approver"
																			><Icon name="shield" size={11} weight={2.2} /></span
																		>{:else if g.approver}<span
																			class="text-warning"
																			title="Approver flag, no effect"
																			><Icon name="shield" size={11} weight={2.2} /></span
																		>{/if}{g.project}{#if approver}<span class="sr-only"
																			>(approver)</span
																		>{:else if g.approver}<span class="sr-only"
																			>(approver flag, no effect)</span
																		>{/if}</span
																>
															{/each}
														</span>
													{/if}
												</td>
												<td class="hidden px-2.5 py-2.5 whitespace-nowrap @[34rem]:table-cell">
													{#if active.length === 0}
														<span class="text-text-3">None active</span>
													{:else}
														<span class="inline-flex items-center gap-2 text-text-2 tabular-nums">
															<span
																class="size-1.5 rounded-full {soon ? 'bg-warning' : 'bg-success'}"
																aria-hidden="true"
															></span>
															<span title={soon ? 'One of them expires within a week' : undefined}
																>{active.length} active</span
															>
														</span>
													{/if}
												</td>
												<td
													class="hidden px-2.5 py-2.5 whitespace-nowrap text-text-2 tabular-nums @[42rem]:table-cell"
													title={person.last_asked_at
														? describeMoment(person.last_asked_at)
														: undefined}
												>
													{person.last_asked_at ? describeAge(person.last_asked_at) : 'Never'}
												</td>
												<td class="hidden py-2.5 pr-4 pl-2.5 @md:table-cell">
													<!-- the count over a bar against the busiest person's (a sliver for any at
													     all), so the column reads as one small chart and costs no width -->
													<span class="ml-auto flex w-12 flex-col items-end gap-1.5">
														<span
															class="leading-4 font-semibold tabular-nums {person.disabled_at
																? 'text-text-2'
																: 'text-text'}">{person.questions_30d}</span
														>
														<span
															class="block h-1 w-full overflow-hidden rounded-full bg-surface-3"
															aria-hidden="true"
															><span
																class="block h-full rounded-full {isSelected
																	? 'bg-accent'
																	: person.disabled_at
																		? 'bg-border-control'
																		: 'bg-bar'}"
																style="width: {share > 0
																	? Math.max(8, Math.round(share * 100))
																	: 0}%"
															></span></span
														>
													</span>
												</td>
											</tr>
										{:else}
											<tr>
												<td colspan="6" class="px-4 py-10">
													<span class="flex flex-col items-center gap-2.5 text-compact text-text-3">
														<span
															class="grid size-9 place-items-center rounded-full bg-surface-2 text-text-3 ring-1 ring-border-subtle ring-inset"
															aria-hidden="true"><Icon name="search" size={16} /></span
														>
														Nobody matches these filters.
													</span>
												</td>
											</tr>
										{/each}
									</tbody>
								</table>
							</div>
						</section>

						<Card
							title="By project"
							subtitle="Who may ask about each project"
							flush
							class="xl:flex-1"
						>
							<!-- one row a project, in four aligned columns: the project, how many may ask, who,
							     and its approvers; the rows share what height the card gains, up to a roomy row each -->
							<ul
								class="flex flex-1 flex-col divide-y divide-border-subtle border-t border-border-subtle"
							>
								{#each byProject as row (row.project)}
									{@const on = filter.project === row.project}
									{@const more = row.holders.length - SHOWN_HOLDERS}
									<li class="flex max-h-16 flex-1">
										<button
											type="button"
											aria-pressed={on}
											onclick={() => (filter.project = on ? 'all' : row.project)}
											class="relative grid w-full grid-cols-[minmax(0,10rem)_4.5rem_minmax(0,1fr)_auto] items-center gap-x-4 py-2 pr-4 pl-4 text-left focus-ring transition-colors {on
												? 'bg-accent-soft'
												: 'hover:bg-surface-2/70'}"
										>
											{#if on}<span
													aria-hidden="true"
													class="absolute inset-y-1.5 left-0 w-0.75 rounded-r-full bg-accent"
												></span>{/if}
											<span class="flex min-w-0 items-center gap-2">
												<span class="truncate font-mono text-compact font-medium text-text"
													>{row.project}</span
												>
												{#if row.noManifest}<span
														class="inline-flex shrink-0 items-center gap-1 text-2xs font-medium text-warning"
														><Icon name="alert" size={12} />No manifest</span
													>{/if}
											</span>
											<span class="text-xs whitespace-nowrap text-text-2 tabular-nums"
												>{plural(row.holders.length, 'person', 'people')}</span
											>
											<span
												class="flex min-w-0 items-center gap-1 overflow-hidden py-1 pr-1"
												aria-hidden="true"
											>
												{#each row.holders.slice(0, SHOWN_HOLDERS) as holder (holder.person.id)}
													<span
														class="relative shrink-0"
														title="{holder.person.display_name}{holder.approver
															? ', approver'
															: ''}"
													>
														<Avatar
															name={holder.person.display_name}
															size={24}
															selected={selected?.id === holder.person.id}
														/>
														{#if holder.approver}<span
																class="absolute -right-1 -bottom-1 grid size-3.5 place-items-center rounded-full bg-surface text-info ring-1 ring-border"
																><Icon name="shield" size={9} weight={2.6} /></span
															>{/if}
													</span>
												{/each}
												{#if more > 0}<span
														class="shrink-0 pl-0.5 text-2xs font-medium text-text-3 tabular-nums"
														>+{more}</span
													>{/if}
											</span>
											<span class="flex justify-end">
												{#if row.approvers > 0}<span
														class="inline-flex h-5.5 items-center gap-1 rounded-chip bg-info/10 pr-1.5 pl-1 text-2xs font-medium whitespace-nowrap text-text tabular-nums ring-1 ring-info/30 ring-inset"
														><span class="text-info" aria-hidden="true"
															><Icon name="shield" size={11} weight={2.2} /></span
														>{plural(row.approvers, 'approver', 'approvers')}</span
													>{/if}
											</span>
										</button>
									</li>
								{/each}
							</ul>
						</Card>
					</div>

					<!-- the row is as tall as the taller side: the projects' card takes up what the panel adds,
					     the panel what the list adds, up to the window's height -->
					<div bind:this={detail} class="scroll-mt-2.5">
						<div
							class="flex flex-col xl:sticky xl:top-2.5 xl:h-full xl:max-h-[calc(100dvh-1.25rem)]"
						>
							{#if selected}
								<PersonDetail person={selected} {projects} />
							{/if}
						</div>
					</div>
				</div>

				<!-- why nothing on this page changes anything, and how the commands beside things are run: the
				     frame's foot -->
				<p
					class="flex items-start gap-2.5 border-t border-border-subtle px-0.5 pt-3.5 text-xs leading-5 text-text-3"
				>
					<span
						class="grid size-5 shrink-0 place-items-center rounded-chip bg-surface-2 text-text-3 ring-1 ring-border-subtle ring-inset"
						aria-hidden="true"><Icon name="terminal" size={13} /></span
					>
					<span class="min-w-0">
						<span class="font-semibold text-text-2">Read-only.</span>
						This page shows people and their access, and changes nothing. Changes are made on the server
						over SSH: paste the command shown beside each thing into the admin shell, which
						<code class={codeChip}>{ADMIN_SHELL}</code>
						opens.
					</span>
				</p>
			{:else if read.loading}
				<p class="text-sm text-text-3" role="status">Reading who has access…</p>
			{/if}
		{/if}
	</PageFrame>
</main>

<script lang="ts">
	import { onMount, type Snippet } from 'svelte';

	import { resolve } from '$app/paths';
	import { PROBE_WORDS, classLabel } from '$lib/admin/words';
	import { approvals } from '$lib/approvals/approvals.svelte';
	import { projects } from '$lib/ask/projects.svelte';
	import { DOT, type Tone } from '$lib/ask/words';
	import Avatar from '$lib/components/admin/Avatar.svelte';
	import PageFrame from '$lib/components/admin/PageFrame.svelte';
	import Chip from '$lib/components/ask/Chip.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { describeAge, describeExpiry, roleLabel } from '$lib/format';
	import { applyTheme, loadTheme, saveTheme, type ThemeChoice } from '$lib/preferences';
	import { session } from '$lib/session.svelte';

	const me = $derived(session.me);
	const expiry = $derived(me ? describeExpiry(me.token.expires_at) : null);

	// The token's runway, as a row of days from today: one mark per day of the next 30, lit while the
	// token still works on it. A token that runs longer fills the row; one that has run out leaves it
	// dark. Drawn from the expiry and the clock only, so it says what the words beside it say.
	const HORIZON_DAYS = 30;
	const DAY_MS = 24 * 60 * 60 * 1000;
	const DAYS = Array.from({ length: HORIZON_DAYS }, (_, day) => day);
	const runway = $derived.by(() => {
		if (!me) return 0;
		const left = (new Date(me.token.expires_at).getTime() - Date.now()) / DAY_MS;
		// whole days, as the chip counts them; the last day keeps one mark lit until the token runs out
		return left <= 0 ? 0 : Math.min(HORIZON_DAYS, Math.max(1, Math.floor(left)));
	});
	const runwayTone = $derived<Tone>(!expiry ? 'neutral' : expiry.soon ? 'warn' : 'good');

	// A project's name as the list gives it, once it has come; the grant names a project by its id.
	function nameOf(project: string): string {
		return project === 'portfolio'
			? 'All projects (portfolio)'
			: (projects.find(project)?.display_name ?? project);
	}

	// The projects where the person approves held answers. Only an approver hears of approving: the
	// Projects card says it, names those projects and leads to Approvals, with what waits there once
	// the list the side panel's badge counts from has come.
	const approved = $derived(me ? me.projects.filter((grant) => grant.approver) : []);
	const approverIn = $derived(approved.length);
	const approvedNames = $derived(
		new Intl.ListFormat('en').format(approved.map((grant) => nameOf(grant.project)))
	);
	const waiting = $derived(
		approverIn > 0 && approvals.loaded && !approvals.failure ? approvals.decidable.length : null
	);

	let theme = $state<ThemeChoice>(loadTheme());
	const themes: {
		value: ThemeChoice;
		label: string;
		note: string;
		icon: 'monitor' | 'sun' | 'moon';
	}[] = [
		{
			value: 'default',
			label: 'As set up',
			note: 'The look this Mercury was built with.',
			icon: 'monitor'
		},
		{ value: 'neutral', label: 'Neutral', note: 'Light, plain.', icon: 'sun' },
		{ value: 'demo', label: 'Demo', note: 'Dark, with the accent colours.', icon: 'moon' }
	];

	// The tokens a look's small preview is drawn with.
	const PREVIEW = [
		'bg',
		'sidebar',
		'surface',
		'border',
		'text',
		'text-3',
		'accent',
		'accent-soft',
		'accent-2'
	] as const;
	type Look = 'neutral' | 'demo';

	/**
	 * A look's own token values, so its preview shows that look whichever one the page wears: the
	 * page's root takes the look for an instant, inside this call (nothing is painted in between),
	 * gives back the values, and gets its own look back.
	 */
	function palette(look: Look): string {
		const root = document.documentElement;
		const was = root.dataset.theme;
		root.dataset.theme = look;
		const style = getComputedStyle(root);
		const vars = PREVIEW.map((name) => `--${name}: ${style.getPropertyValue(`--${name}`).trim()}`);
		if (was === undefined) delete root.dataset.theme;
		else root.dataset.theme = was;
		return vars.join('; ');
	}

	const palettes: Record<Look, string> = { neutral: palette('neutral'), demo: palette('demo') };

	// Which look "As set up" is: the look `applyTheme` gives back for it, asked of a stand-in root.
	const built: Look = (() => {
		const stand: { dataset: { theme?: string } } = { dataset: {} };
		applyTheme('default', stand);
		return stand.dataset.theme === 'demo' ? 'demo' : 'neutral';
	})();
	const previewOf = (choice: ThemeChoice): Look => (choice === 'default' ? built : choice);

	// The projects' names, and what each lets the person ask, when the list has come; the grant names
	// a project by its id.
	onMount(() => {
		if (projects.list === null) void projects.load();
	});

	// A look is a preference of this browser, kept in it; nothing else is.
	function pick(choice: ThemeChoice) {
		theme = choice;
		saveTheme(choice);
		applyTheme(choice);
	}

	// A part of the settings: a card with its title and a line about it in a header, the thing itself
	// under it.
	const card =
		'flex min-w-0 flex-col overflow-hidden rounded-card border border-border bg-surface shadow-card';
	// the two cards side by side share their rows on a wide screen, so their headers end on one line
	// whichever line about it wraps
	const paired = 'lg:row-span-2 lg:grid lg:grid-rows-subgrid lg:gap-0';
	// the label of a value
	const term = 'text-xs text-text-3';
	// a fact of the identity or the token as a row: its label in a narrow column, the value beside it,
	// the same label column in both cards, so their values start the same way in from the label
	const fact = 'grid min-h-8 grid-cols-[4rem_minmax(0,1fr)] items-center gap-x-4';
	// the identity's facts beside the name stand apart from it by a hairline on a wide screen
	const apart = 'sm:border-l sm:border-border-subtle sm:pl-5';
	// a link that looks like a quiet button
	const quietLink =
		'inline-flex min-h-8 items-center gap-1.5 rounded-control border border-border-control bg-surface px-3 text-compact font-medium text-text shadow-control focus-ring transition-colors duration-150 hover:border-border-strong hover:bg-surface-2';
	// a kind of question a project lets the person ask, as a small chip
	const classChip =
		'inline-flex h-5 items-center rounded-chip bg-surface-2 px-1.5 text-2xs font-medium text-text-2 ring-1 ring-border-subtle ring-inset';
	// the columns of a project's row on a wide screen: the project, what may be asked, its repository
	const projectColumns = 'lg:grid-cols-[minmax(0,17rem)_minmax(0,1fr)_minmax(0,12rem)]';
</script>

<svelte:head>
	<title>Settings · Mercury</title>
</svelte:head>

<!-- a part's header: its mark and title, the line about it under the title, a count on the right -->
{#snippet head(
	id: string,
	title: string,
	note: string,
	icon: 'user' | 'folder' | 'key' | 'monitor',
	meta?: Snippet
)}
	<header
		class="flex items-start justify-between gap-4 border-b border-border-subtle px-5 pt-3.5 pb-3"
	>
		<div class="flex min-w-0 flex-col gap-0.5">
			<h2
				{id}
				class="flex items-center gap-2 text-sm leading-5 font-semibold tracking-tight text-text"
			>
				<span class="flex text-text-3"><Icon name={icon} size={15} /></span>{title}
			</h2>
			<p class="pl-5.75 text-xs text-pretty text-text-3">{note}</p>
		</div>
		{#if meta}<div class="flex shrink-0 items-center gap-2 pt-0.5">{@render meta()}</div>{/if}
	</header>
{/snippet}

<!-- the whole screen in one panel: its name in the panel's head, the four parts on its quieter ground -->
<main
	id="main"
	tabindex="-1"
	class="flex flex-col px-2 py-2.5 focus:outline-none sm:px-3 lg:min-h-dvh"
>
	<PageFrame
		title="Settings"
		description="Who you are to Mercury, your token, and this browser."
		icon="settings"
	>
		{#if me}
			<!-- who and the token side by side, the projects and the look under them, each the full width -->
			<div class="grid gap-4 lg:grid-cols-[minmax(0,1.75fr)_minmax(0,1fr)]">
				<!-- the identity: the person as the side panel's foot shows them, larger, their name leading
				     and what else Mercury knows of them in rows beside it -->
				<section aria-labelledby="who" class="{card} {paired}">
					{@render head('who', 'Who I am', 'How Mercury knows you. Your admin sets it.', 'user')}
					<div
						class="flex flex-1 flex-col gap-5 bg-linear-to-r from-accent/10 to-transparent to-60% px-5 py-5 sm:flex-row sm:items-center"
					>
						<!-- signed in: the dot on the badge, as the person is present in this browser now -->
						<span class="relative flex w-fit shrink-0">
							<Avatar name={me.user.display_name} size={56} selected />
							<span
								aria-hidden="true"
								class="absolute -right-0.5 bottom-0 size-3.5 rounded-full bg-success ring-3 ring-surface"
							></span>
						</span>
						<!-- the name across both rows on a wide screen, the e-mail and the role one under the other -->
						<dl
							class="grid min-w-0 flex-1 gap-y-3 sm:grid-cols-[minmax(0,0.75fr)_minmax(0,1.25fr)] sm:gap-x-6 sm:gap-y-0"
						>
							<div class="flex min-w-0 flex-col gap-0.5 sm:row-span-2 sm:justify-center">
								<dt class={term}>Name</dt>
								<dd class="truncate text-xl font-semibold tracking-tight text-text">
									{me.user.display_name}
								</dd>
							</div>
							<div class="{fact} {apart} sm:pb-1">
								<dt class={term}>E-mail</dt>
								<dd class="min-w-0 text-sm wrap-anywhere text-text">{me.user.email}</dd>
							</div>
							<div class="{fact} {apart} sm:pt-1">
								<dt class={term}>Role</dt>
								<dd class="flex">
									<Chip tone={me.user.role === 'admin' ? 'info' : 'neutral'}
										>{roleLabel(me.user.role)}</Chip
									>
								</dd>
							</div>
						</dl>
					</div>
				</section>

				<!-- the token: its id, when it runs out, and that runway in days -->
				<section aria-labelledby="token-heading" class="{card} {paired}">
					{@render head(
						'token-heading',
						'My token',
						'Its id is shown here. The token itself is not kept in this browser.',
						'key'
					)}
					<div class="flex flex-1 flex-col gap-4 px-5 pt-3.5 pb-4.5">
						<dl class="flex flex-col gap-1 text-sm">
							<div class={fact}>
								<dt class={term}>Id</dt>
								<dd class="flex">
									<code
										class="rounded-chip bg-surface-2 px-1.5 py-0.5 font-mono text-compact text-text ring-1 ring-border-subtle ring-inset"
										>{me.token.public_id}</code
									>
								</dd>
							</div>
							{#if expiry}
								<div class={fact}>
									<dt class={term}>Expires</dt>
									<dd class="flex flex-wrap items-center gap-x-2.5 gap-y-1.5 text-text">
										<span class="tabular-nums">{expiry.when}</span>
										<Chip tone={expiry.soon ? 'warn' : 'good'}>{expiry.left}</Chip>
									</dd>
								</div>
							{/if}
						</dl>
						{#if expiry}
							<!-- the runway: what the chip says, in days; the words carry it for a reader -->
							<div aria-hidden="true" class="mt-auto flex flex-col gap-1.5">
								<span class="flex h-2 gap-0.5">
									{#each DAYS as day (day)}
										<span
											class="flex-1 rounded-xs {day < runway ? DOT[runwayTone] : 'bg-surface-3'}"
										></span>
									{/each}
								</span>
								<span class="flex justify-between text-2xs text-text-3 tabular-nums">
									<span>Today</span>
									<span>{HORIZON_DAYS} days</span>
								</span>
							</div>
						{/if}
					</div>
				</section>

				<!-- each project: what it is, what may be asked about it, and how fresh its repository is -->
				<section aria-labelledby="projects" class="{card} lg:col-span-2">
					{#snippet granted()}
						<span class="flex items-center gap-1.5 text-xs text-text-3 tabular-nums">
							<span>{me.projects.length} granted</span>
							{#if approverIn > 0}<span aria-hidden="true">·</span>
								<span>approver in {approverIn}</span>{/if}
						</span>
					{/snippet}
					<!-- approving is said only to an approver -->
					{@render head(
						'projects',
						'Projects',
						approverIn > 0
							? 'What you can ask about, and where you approve held answers.'
							: 'What you can ask about.',
						'folder',
						me.projects.length > 0 ? granted : undefined
					)}
					{#if me.projects.length === 0}
						<div class="p-4">
							<div
								class="flex items-center gap-3 rounded-control border border-dashed border-border-control px-4 py-4 text-sm text-text-2"
							>
								<span
									aria-hidden="true"
									class="grid size-9 shrink-0 place-items-center rounded-control bg-surface-2 text-text-3"
									><Icon name="folder" size={16} /></span
								>
								<p>None yet. Ask your admin to give you one.</p>
							</div>
						</div>
					{:else}
						<!-- the columns' names, for the eye; each row's cells say what they are to a reader -->
						<div
							aria-hidden="true"
							class="hidden gap-x-6 border-b border-border-subtle bg-surface-2/50 px-5 py-2 text-xs font-medium text-text-3 lg:grid {projectColumns}"
						>
							<span>Project</span>
							<span>You can ask about</span>
							<span class="text-right">Repository</span>
						</div>
						<ul class="flex flex-col divide-y divide-border-subtle">
							{#each me.projects as grant (grant.project)}
								{@const summary =
									grant.project === 'portfolio' ? undefined : projects.find(grant.project)}
								{@const name = nameOf(grant.project)}
								{@const probe = summary
									? PROBE_WORDS[summary.probe?.status ?? 'unregistered']
									: null}
								<li class="grid items-center gap-x-6 gap-y-2.5 px-5 py-3 text-sm {projectColumns}">
									<div class="flex min-w-0 items-center gap-3">
										<!-- the project's mark: a shield in the approvers' blue where they approve -->
										<span
											aria-hidden="true"
											class="grid size-9 shrink-0 place-items-center rounded-control ring-1 ring-inset {grant.approver
												? 'bg-info/12 text-info ring-info/25'
												: 'bg-surface-2 text-text-2 ring-border-subtle'}"
											><Icon name={grant.approver ? 'shield' : 'folder'} size={16} /></span
										>
										<span class="flex min-w-0 flex-col gap-0.5">
											<span class="flex min-w-0 items-center gap-2">
												<span class="truncate font-medium text-text">{name}</span>
												{#if grant.approver}<Chip tone="info">Approver</Chip>{/if}
											</span>
											<span class="flex min-w-0 items-center gap-1.5 text-2xs text-text-3">
												<span class="shrink-0 font-mono">{grant.project}</span>
												{#if summary && summary.modules.length > 0}<span aria-hidden="true">·</span>
													<span class="truncate">{summary.modules.join(', ')}</span>{/if}
											</span>
										</span>
									</div>

									<div class="min-w-0 pl-12 lg:pl-0">
										{#if summary && summary.classes.length > 0}
											<ul class="flex flex-wrap gap-1" aria-label="What you can ask about {name}">
												{#each summary.classes as question (question)}
													<li class={classChip}>{classLabel(question)}</li>
												{/each}
											</ul>
										{:else}
											<span class="text-text-3" aria-hidden="true">—</span>
										{/if}
									</div>

									<div
										class="flex min-w-0 items-center gap-2 pl-12 lg:flex-col lg:items-end lg:gap-0.5 lg:pl-0"
									>
										{#if summary && probe}
											<span class="sr-only">Repository:</span>
											<span title={probe.meaning}><Chip tone={probe.tone}>{probe.label}</Chip></span
											>
											{#if summary.probe}<span class="text-2xs text-text-3 tabular-nums"
													>Read {describeAge(summary.probe.as_of)}</span
												>{/if}
										{:else}
											<span class="text-text-3" aria-hidden="true">—</span>
										{/if}
									</div>
								</li>
							{/each}
						</ul>
						{#if approverIn > 0}
							<!-- what approving means for this person: where they do it, what waits, the way there -->
							<div
								class="flex flex-wrap items-center gap-x-6 gap-y-2.5 border-t border-border-subtle bg-info/5 px-5 py-2.5"
							>
								<p class="flex min-w-0 flex-1 basis-80 items-center gap-3 text-compact text-text-2">
									<span
										aria-hidden="true"
										class="grid size-7 shrink-0 place-items-center rounded-full bg-info/12 text-info ring-1 ring-info/25 ring-inset"
										><Icon name="shield" size={14} /></span
									>
									<span class="min-w-0"
										>You approve held answers in <span class="font-medium text-text"
											>{approvedNames}</span
										>.</span
									>
								</p>
								<div class="flex shrink-0 items-center gap-3">
									{#if waiting !== null}
										<span class="flex items-center gap-1.5 text-compact text-text-2">
											{#if waiting > 0}<span
													class="grid h-5 min-w-5 place-items-center rounded-full bg-accent px-1.5 text-2xs font-semibold text-on-accent tabular-nums"
													>{waiting}</span
												>
												waiting{:else}None waiting{/if}
										</span>
									{/if}
									<a href={resolve('/approvals')} class={quietLink}
										>Go to Approvals<span aria-hidden="true" class="-mr-1 flex text-text-3"
											><Icon name="chevron" size={14} /></span
										></a
									>
								</div>
							</div>
						{/if}
					{/if}
				</section>

				<!-- the look, set once and seldom: three compact choices, each in miniature in its own colours -->
				<section aria-labelledby="look" class="{card} lg:col-span-2">
					{@render head('look', 'Look', 'Remembered in this browser only.', 'monitor')}
					<fieldset class="px-5 py-4">
						<legend class="sr-only">Look</legend>
						<div class="grid gap-2.5 sm:grid-cols-3">
							{#each themes as option (option.value)}
								<label
									class="group flex items-center gap-3.5 rounded-control border border-border bg-surface p-2 pr-3.5 transition-[border-color,box-shadow,background-color] duration-150 hover:border-border-control hover:bg-surface-2/40 has-[:checked]:border-accent has-[:checked]:bg-accent/4 has-[:checked]:ring-1 has-[:checked]:ring-accent has-[:focus-visible]:outline-3 has-[:focus-visible]:outline-offset-2 has-[:focus-visible]:outline-ring"
								>
									<input
										type="radio"
										name="theme"
										value={option.value}
										checked={theme === option.value}
										onchange={() => pick(option.value)}
										class="sr-only"
									/>
									<!-- the look in miniature, in its own colours: the side panel, a title, a card -->
									<span
										aria-hidden="true"
										style={palettes[previewOf(option.value)]}
										class="flex h-16 w-28 shrink-0 overflow-hidden rounded-chip border border-border bg-bg"
									>
										<span
											class="flex w-[30%] flex-col gap-1 border-r border-border bg-sidebar px-1.5 py-2"
										>
											<span class="flex items-center gap-0.5">
												<span class="size-1.5 rounded-[1px] bg-accent"></span>
												<span class="h-0.75 w-1/2 rounded-full bg-text/70"></span>
											</span>
											<span class="mt-0.5 h-2 rounded-[1px] bg-accent-soft"></span>
											<span class="h-0.75 w-3/4 rounded-full bg-text-3/45"></span>
											<span class="h-0.75 w-1/2 rounded-full bg-text-3/45"></span>
										</span>
										<span class="flex flex-1 flex-col gap-1 p-1.5">
											<span class="h-1 w-2/5 rounded-full bg-text/80"></span>
											<span
												class="flex flex-1 flex-col justify-between rounded-xs border border-border bg-surface p-1"
											>
												<span class="flex flex-col gap-0.75">
													<span class="h-0.75 w-4/5 rounded-full bg-text-3/50"></span>
													<span class="h-0.75 w-3/5 rounded-full bg-text-3/50"></span>
												</span>
												<span class="flex items-center gap-1">
													<span class="h-1.5 w-5 rounded-[1px] bg-accent"></span>
													<span class="size-1 rounded-full bg-accent-2"></span>
												</span>
											</span>
										</span>
									</span>
									<span class="flex min-w-0 flex-1 flex-col gap-0.5">
										<span class="flex items-center gap-1.5 text-sm font-medium text-text">
											<span class="flex text-text-3 group-has-[:checked]:text-accent-text"
												><Icon name={option.icon} size={15} /></span
											>
											{option.label}
										</span>
										<span class="text-xs text-pretty text-text-2">{option.note}</span>
									</span>
									<!-- the radio as it is drawn: a ring, filled with a dot when picked -->
									<span
										aria-hidden="true"
										class="grid size-4 shrink-0 place-items-center rounded-full border border-border-strong bg-surface transition-colors group-has-[:checked]:border-accent group-has-[:checked]:bg-accent"
										><span
											class="size-1.5 rounded-full bg-on-accent opacity-0 group-has-[:checked]:opacity-100"
										></span></span
									>
								</label>
							{/each}
						</div>
					</fieldset>
				</section>
			</div>
		{/if}
	</PageFrame>
</main>

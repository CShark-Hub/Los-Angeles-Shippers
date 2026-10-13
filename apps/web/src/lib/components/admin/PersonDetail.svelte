<script lang="ts">
	import { commands } from '$lib/admin/commands';
	import { approves, flagged, grantable, mayApprove, roleChoices } from '$lib/admin/people';
	import { classLabel, tokenChip } from '$lib/admin/words';
	import type { AdminGrant, AdminPerson, Role } from '$lib/api';
	import Chip from '$lib/components/ask/Chip.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { describeAge, describeCost, describeDay, roleLabel } from '$lib/format';
	import Avatar from './Avatar.svelte';
	import CopyCommand from './CopyCommand.svelte';

	// One person, everything the admin may want to know or change about them: who they are and three
	// figures, each grant and what it lets them ask, the tokens, then the changes. Each change is a
	// command to copy, folded until it is asked for; nothing here runs it, and only a command the core
	// would take is offered: none for a disabled person's access, no approver flag for a role that
	// can't approve, no board or admin role for an approver.
	let { person, projects }: { person: AdminPerson; projects: readonly string[] } = $props();

	type GrantClass = NonNullable<AdminGrant['classes']>[number];

	const disabled = $derived(person.disabled_at !== null);
	const canApprove = $derived(mayApprove(person.role));
	const choices = $derived(roleChoices(person));
	const refused = $derived(choices.filter((c) => c.refused !== null));
	const flags = $derived(flagged(person));
	const options = $derived(grantable(person, projects));

	let newRole = $state<Role>('pm');
	let toGrant = $state('');
	let asApprover = $state(false);
	let tokenLabel = $state('laptop');
	let tokenExpires = $state('90d');
	/**
	 * The one command open in the panel, if any: `grant`, `token`, `role`, `disable`, a grant's
	 * take-back (`ungrant:<project>`) or a token's revoke (`revoke:<public id>`). One at a time, so
	 * the panel grows by one command at most.
	 */
	let open = $state<string | null>(null);
	/** The part under the name that scrolls when the panel is taller than its room. */
	let body = $state<HTMLElement>();

	function toggle(what: string) {
		open = open === what ? null : what;
	}

	// a new person on screen starts the forms over, folded
	$effect(() => {
		void person.id;
		newRole = roleChoices(person).find((c) => c.refused === null)?.role ?? 'pm';
		toGrant = grantable(person, projects)[0] ?? '';
		asApprover = false;
		open = null;
	});

	// and is read from the top
	$effect(() => {
		void person.id;
		body?.scrollTo({ top: 0 });
	});

	const tokens = $derived([...person.tokens].reverse());

	// What a grant lets them ask, as one string, so two grants that allow the same can be told apart
	// from two that don't.
	function asked(grant: AdminGrant): string | null {
		return grant.classes === null
			? null
			: grant.classes.map((c) => `${c.class}${c.signoff ? '+' : ''}`).join(',');
	}
	// When every grant lets them ask the same, the list is shown once under the grants instead of on
	// each (a reader still hears it on each grant).
	const shared = $derived.by(() => {
		const grants = person.grants;
		if (grants.length < 2) return null;
		const first = asked(grants[0]);
		return first !== null && grants.every((g) => asked(g) === first) ? grants[0].classes : null;
	});

	// a field: 32px, the strong edge every field keeps; a select wears the app's chevron
	const field =
		'h-8 rounded-control border border-border-strong bg-surface px-2.5 text-compact text-text focus-ring transition-colors hover:bg-surface-2';
	const select = `${field} appearance-none pr-7`;
	// a section's title, with its mark before it
	const heading = 'flex items-center gap-2 text-compact font-semibold text-text';
	// a list of rows on the panel: grants, tokens, the changes
	const box = 'overflow-hidden rounded-control border border-border bg-surface shadow-control';
	// where a change is put together: a dashed edge, the command it comes to inside
	const compose =
		'flex flex-col gap-2 rounded-control border border-dashed border-border-control bg-surface px-3 pt-2.5 pb-3';
	// a class a grant lets them ask, as a small chip
	const classChip =
		'inline-flex h-5 items-center gap-1 rounded-chip bg-surface-2 px-1.5 text-2xs font-medium text-text-2 ring-1 ring-border-subtle ring-inset';
	// what opens a new grant or token: the accent's words with a plus that turns to a close
	const adder =
		'group -ml-1.5 inline-flex h-7 w-fit items-center gap-1.5 rounded-control px-1.5 text-xs font-medium text-accent-text focus-ring transition-colors hover:bg-accent-soft aria-expanded:bg-accent-soft';
	// what opens a row's command: a small quiet button; a risky one turns red under the pointer
	function opener(risky: boolean): string {
		return `inline-flex h-7 shrink-0 items-center gap-1 rounded-control border border-border-control bg-surface px-2.5 text-xs font-medium text-text-2 shadow-control focus-ring transition-colors ${
			risky
				? 'hover:border-danger/40 hover:bg-danger/8 hover:text-danger aria-expanded:border-danger/40 aria-expanded:bg-danger/8 aria-expanded:text-danger'
				: 'hover:border-border-strong hover:bg-surface-2 hover:text-text aria-expanded:bg-surface-2 aria-expanded:text-text'
		}`;
	}
</script>

{#snippet chevron()}<span
		class="pointer-events-none absolute right-2 text-text-3"
		aria-hidden="true"><Icon name="chevron-down" size={14} /></span
	>{/snippet}

{#snippet classInner(c: GrantClass)}
	{classLabel(c.class)}{#if c.signoff}<span class="text-warning"
			><Icon name="clock" size={11} weight={2.2} /></span
		><span class="sr-only">, needs a sign-off</span>{/if}
{/snippet}

{#snippet plus()}<span
		class="grid place-items-center group-aria-expanded:rotate-45 motion-safe:transition-transform motion-safe:duration-200"
		aria-hidden="true"><Icon name="plus" size={14} weight={2.2} /></span
	>{/snippet}

<!-- A raised side panel: who they are and three figures stay at the top; what they may ask, their
     tokens and what can change about them scroll under it, with the terminal's own view at the foot. -->
<article
	class="flex min-h-0 flex-1 flex-col overflow-hidden rounded-card border border-border bg-surface-raised shadow-raised"
	aria-label={person.display_name}
>
	<header class="shrink-0 border-b border-border-subtle">
		<!-- the person picked in the list, in the accent filled as their row's badge is; their role and
		     whether they may sign in on one line under the address -->
		<div class="flex items-start gap-3 px-4 pt-3.5 pb-3">
			<Avatar name={person.display_name} size={44} muted={disabled} selected={!disabled} />
			<div class="flex min-w-0 flex-1 flex-col">
				<h2 class="truncate text-title leading-6 font-semibold tracking-tight text-text">
					{person.display_name}
				</h2>
				<p class="truncate text-xs text-text-3">{person.email}</p>
				<div class="mt-1.5 flex flex-wrap items-center gap-1.5">
					<Chip tone={person.role === 'admin' ? 'info' : 'neutral'}>{roleLabel(person.role)}</Chip>
					{#if disabled && person.disabled_at}
						<Chip tone="bad">Disabled {describeDay(person.disabled_at)}</Chip>
					{:else}
						<Chip tone="good">Active</Chip>
					{/if}
				</div>
			</div>
		</div>

		<dl
			class="mx-3 mb-3 grid grid-cols-[minmax(0,1.15fr)_minmax(0,1.1fr)_minmax(0,0.75fr)] divide-x divide-border-subtle rounded-control bg-surface-2/70 ring-1 ring-border-subtle ring-inset"
		>
			<div class="flex min-w-0 flex-col gap-0.5 px-2.5 py-2">
				<dt class="truncate text-2xs font-medium text-text-3">Questions, 30 days</dt>
				<dd class="text-title leading-6 font-semibold text-text tabular-nums">
					{person.questions_30d}
				</dd>
			</div>
			<div class="flex min-w-0 flex-col gap-0.5 px-2.5 py-2">
				<dt class="truncate text-2xs font-medium text-text-3">Last asked</dt>
				<dd class="truncate text-compact leading-6 font-semibold text-text tabular-nums">
					{person.last_asked_at ? describeAge(person.last_asked_at) : 'Never'}
				</dd>
			</div>
			<div class="flex min-w-0 flex-col gap-0.5 px-2.5 py-2">
				<dt class="truncate text-2xs font-medium text-text-3">Spent today</dt>
				<dd class="text-title leading-6 font-semibold text-text tabular-nums">
					{describeCost(person.spent_today_usd)}
				</dd>
			</div>
		</dl>
	</header>

	<div
		bind:this={body}
		class="flex min-h-0 flex-1 flex-col divide-y divide-border-subtle overflow-y-auto"
	>
		<section class="flex flex-col gap-2.5 px-4 pt-3.5 pb-4" aria-labelledby="grants-{person.id}">
			<div class="flex items-center justify-between gap-2">
				<h3 id="grants-{person.id}" class={heading}>
					<span class="text-text-3"><Icon name="folder" size={15} /></span>
					Projects and what they may ask
				</h3>
				<span class="text-xs text-text-3 tabular-nums">{person.grants.length} granted</span>
			</div>
			{#if person.grants.length === 0}
				<p
					class="flex items-start gap-2 rounded-control border border-warning/30 bg-warning/6 px-3 py-2 text-xs/5 text-text-2"
				>
					<span class="mt-0.5 shrink-0 text-warning"><Icon name="alert" size={14} /></span>
					<span
						>No project yet: they can sign in, but every question is refused until a project is
						granted.</span
					>
				</p>
			{:else}
				<div class={box}>
					<ul class="flex flex-col divide-y divide-border-subtle">
						{#each person.grants as grant (grant.project)}
							{@const approver = approves(person, grant)}
							<li class="flex flex-col gap-2 px-3 py-2">
								<div class="flex min-h-7 items-center gap-2.5">
									<!-- the grant's mark: a shield in the approvers' blue for an approver, in the
									     warning's colour for a flag that grants nothing -->
									<span
										class="grid size-7 shrink-0 place-items-center rounded-control ring-1 ring-inset {approver
											? 'bg-info/12 text-info ring-info/25'
											: grant.approver
												? 'bg-warning/12 text-warning ring-warning/30'
												: 'bg-surface-2 text-text-3 ring-border-subtle'}"
										aria-hidden="true"
										><Icon name={grant.approver ? 'shield' : 'folder'} size={14} /></span
									>
									<span class="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-1">
										<span class="truncate font-mono text-compact font-semibold text-text"
											>{grant.project}</span
										>
										{#if approver}<Chip tone="info">Approver</Chip>
										{:else if grant.approver}<Chip tone="warn">Approver flag, no effect</Chip>{/if}
									</span>
									{#if !disabled}
										<button
											type="button"
											aria-expanded={open === `ungrant:${grant.project}`}
											aria-controls="ungrant-{person.id}-{grant.project}"
											onclick={() => toggle(`ungrant:${grant.project}`)}
											class={opener(true)}>Take back</button
										>
									{/if}
								</div>
								{#if grant.classes === null}
									<p class="pl-9.5 text-xs text-text-3">
										Can't be asked yet: the project has no manifest.
									</p>
								{:else}
									<ul
										class="flex flex-wrap gap-1 pl-9.5 {shared ? 'sr-only' : ''}"
										aria-label="What {grant.project} lets them ask"
									>
										{#each grant.classes as c (c.class)}
											<li
												class={classChip}
												title={c.signoff ? 'Its answers wait for an approver' : undefined}
											>
												{@render classInner(c)}
											</li>
										{/each}
									</ul>
								{/if}
								{#if grant.approver && !approver}
									<div class="flex flex-col gap-1.5 pl-9.5">
										<p class="text-xs/5 text-text-2">
											This flag grants nothing: only a project manager or a tech lead approves.
											{#if !disabled}To clear it and keep the grant, grant the project again without
												it:{/if}
										</p>
										{#if !disabled}
											<CopyCommand command={commands.dropApprover(person.email, [grant.project])} />
										{/if}
									</div>
								{/if}
								{#if !disabled}
									<div
										id="ungrant-{person.id}-{grant.project}"
										class="pl-9.5"
										hidden={open !== `ungrant:${grant.project}`}
									>
										<CopyCommand command={commands.ungrant(person.email, [grant.project])} />
									</div>
								{/if}
							</li>
						{/each}
					</ul>
					{#if shared}
						<!-- the same for every grant, said once; each grant above still names it to a reader -->
						<div
							class="flex flex-col gap-1.5 border-t border-border-subtle bg-surface-2/50 px-3 pt-2 pb-2.5"
							aria-hidden="true"
						>
							<span class="text-2xs font-medium text-text-3"
								>{person.grants.length === 2 ? 'Both projects let' : 'Each project lets'} them ask</span
							>
							<span class="flex flex-wrap gap-1">
								{#each shared as c (c.class)}
									<span
										class={classChip}
										title={c.signoff ? 'Its answers wait for an approver' : undefined}
										>{@render classInner(c)}</span
									>
								{/each}
							</span>
						</div>
					{/if}
				</div>
			{/if}
			{#if !disabled && options.length > 0}
				<button
					type="button"
					aria-expanded={open === 'grant'}
					aria-controls="grant-{person.id}"
					onclick={() => toggle('grant')}
					class={adder}>{@render plus()}Grant a project…</button
				>
				<!-- folded, not gone: the command and what it can't do are on the page, shown on a press -->
				<div id="grant-{person.id}" class={compose} hidden={open !== 'grant'}>
					<div class="flex flex-wrap items-center gap-x-2.5 gap-y-2 text-xs text-text-2">
						<label class="flex items-center gap-1.5">
							Grant
							<span class="relative inline-flex items-center">
								<select bind:value={toGrant} class={select} aria-label="Project to grant">
									{#each options as p (p)}<option value={p}>{p}</option>{/each}
								</select>
								{@render chevron()}
							</span>
						</label>
						{#if canApprove}
							<label class="flex items-center gap-1.5">
								<input type="checkbox" bind:checked={asApprover} class="size-3.5 accent-accent" />
								as an approver
							</label>
						{/if}
					</div>
					<CopyCommand
						command={commands.grant(person.email, [toGrant], canApprove && asApprover)}
					/>
					{#if !canApprove}
						<p class="text-xs text-text-3">
							Not as an approver: only a project manager or a tech lead approves.
						</p>
					{/if}
				</div>
			{/if}
		</section>

		<section class="flex flex-col gap-2.5 px-4 pt-3.5 pb-4" aria-labelledby="tokens-{person.id}">
			<div class="flex items-center justify-between gap-2">
				<h3 id="tokens-{person.id}" class={heading}>
					<span class="text-text-3"><Icon name="key" size={15} /></span>
					Tokens
				</h3>
				<span class="truncate text-xs text-text-3">Never shown again after they are made</span>
			</div>
			{#if tokens.length === 0}
				<p class="text-xs text-text-3">No token: they can't sign in.</p>
			{:else}
				<ul class="{box} flex flex-col divide-y divide-border-subtle">
					{#each tokens as token (token.public_id)}
						{@const chip = tokenChip(token)}
						{@const revoking = open === `revoke:${token.public_id}`}
						<li class="px-3 py-2 {token.state === 'active' ? '' : 'bg-surface-2/40'}">
							<div class="flex min-w-0 flex-col gap-0.5">
								<div class="flex min-h-7 items-center justify-between gap-2">
									<!-- the label keeps its room; the public id takes what is left -->
									<span class="flex min-w-0 flex-1 items-baseline gap-2">
										<span
											class="min-w-0 truncate text-compact font-medium {token.state === 'active'
												? 'text-text'
												: 'text-text-2'}">{token.label}</span
										>
										<code
											class="w-0 flex-1 truncate font-mono text-2xs text-text-3"
											title={token.public_id}>{token.public_id}</code
										>
									</span>
									<span class="flex shrink-0 items-center gap-2">
										<Chip tone={chip.tone}>{chip.label}</Chip>
										{#if token.state === 'active'}
											<button
												type="button"
												aria-expanded={revoking}
												aria-controls="revoke-{token.public_id}"
												onclick={() => toggle(`revoke:${token.public_id}`)}
												class={opener(true)}>Revoke…</button
											>
										{/if}
									</span>
								</div>
								<p class="text-xs text-text-3 tabular-nums">
									{#if token.revoked_at}Revoked {describeDay(token.revoked_at)}{:else}Expires {describeDay(
											token.expires_at
										)}{/if} · {token.last_used_at
										? `last used ${describeAge(token.last_used_at)}`
										: 'never used'}
								</p>
								{#if token.state === 'active'}
									<div id="revoke-{token.public_id}" class="mt-1.5" hidden={!revoking}>
										<CopyCommand command={commands.revokeToken(token.public_id)} />
									</div>
								{/if}
							</div>
						</li>
					{/each}
				</ul>
			{/if}
			{#if !disabled}
				<button
					type="button"
					aria-expanded={open === 'token'}
					aria-controls="token-{person.id}"
					onclick={() => toggle('token')}
					class={adder}>{@render plus()}New token…</button
				>
				<div id="token-{person.id}" class={compose} hidden={open !== 'token'}>
					<div class="flex flex-wrap items-center gap-x-2.5 gap-y-2 text-xs text-text-2">
						<label class="flex items-center gap-1.5">
							New token for
							<input
								bind:value={tokenLabel}
								class="{field} w-24"
								aria-label="What the token is for"
								maxlength="60"
							/>
						</label>
						<label class="flex items-center gap-1.5">
							valid
							<span class="relative inline-flex items-center">
								<select bind:value={tokenExpires} class={select} aria-label="How long it is valid">
									<option value="30d">30 days</option>
									<option value="90d">90 days</option>
									<option value="180d">180 days</option>
								</select>
								{@render chevron()}
							</span>
						</label>
					</div>
					<CopyCommand
						command={commands.createToken(
							person.email,
							tokenLabel.trim() || 'laptop',
							tokenExpires
						)}
					/>
					<p class="text-xs text-text-3">
						The terminal prints the token once; hand it over in person.
					</p>
				</div>
			{/if}
		</section>

		{#if !disabled}
			<!-- the changes to the person themselves, each a row whose command opens under it -->
			<div class="px-4 pt-3.5 pb-4">
				<div class="{box} divide-y divide-border-subtle">
					<section class="flex flex-col gap-2.5 px-3 py-2.5" aria-labelledby="role-{person.id}">
						<div class="flex items-center gap-2.5">
							<span
								class="grid size-7 shrink-0 place-items-center rounded-control bg-surface-2 text-text-3 ring-1 ring-border-subtle ring-inset"
								aria-hidden="true"><Icon name="user" size={14} /></span
							>
							<span class="flex min-w-0 flex-1 flex-col">
								<h3 id="role-{person.id}" class="text-compact leading-5 font-semibold text-text">
									Role
								</h3>
								<span class="truncate text-xs text-text-3">{roleLabel(person.role)}</span>
							</span>
							<button
								type="button"
								aria-expanded={open === 'role'}
								aria-controls="role-change-{person.id}"
								onclick={() => toggle('role')}
								class={opener(false)}>Change…</button
							>
						</div>
						<div id="role-change-{person.id}" class={compose} hidden={open !== 'role'}>
							<label class="flex flex-wrap items-center gap-1.5 text-xs text-text-2">
								From {roleLabel(person.role).toLowerCase()} to
								<span class="relative inline-flex items-center">
									<select bind:value={newRole} class={select} aria-label="New role">
										{#each choices as choice (choice.role)}<option
												value={choice.role}
												disabled={choice.refused !== null}>{roleLabel(choice.role)}</option
											>{/each}
									</select>
									{@render chevron()}
								</span>
							</label>
							<CopyCommand command={commands.setRole(person.email, newRole)} />
							<p class="text-xs text-text-3">It holds from their next request.</p>
							{#if refused.length > 0}
								<p class="text-xs/5 text-text-2">
									Not offered: {refused.map((c) => roleLabel(c.role).toLowerCase()).join(', ')}. {refused[0]
										.refused} To take the flag off and keep the grant, grant {flags.length === 1
										? 'the project'
										: 'the projects'} again without it:
								</p>
								<CopyCommand command={commands.dropApprover(person.email, flags)} />
							{/if}
						</div>
					</section>

					<section class="flex flex-col gap-2 px-3 py-2.5" aria-labelledby="disable-{person.id}">
						<div class="flex items-center gap-2.5">
							<span
								class="grid size-7 shrink-0 place-items-center rounded-control bg-danger/8 text-danger ring-1 ring-danger/25 ring-inset"
								aria-hidden="true"><Icon name="lock" size={14} /></span
							>
							<h3
								id="disable-{person.id}"
								class="min-w-0 flex-1 text-compact leading-5 font-semibold text-text"
							>
								Disable
							</h3>
							<button
								type="button"
								aria-expanded={open === 'disable'}
								aria-controls="disable-command-{person.id}"
								onclick={() => toggle('disable')}
								class={opener(true)}>Disable…</button
							>
						</div>
						<p class="-mt-1 pl-9.5 text-xs/4.5 text-text-3">
							Revokes every token of theirs and ends their browser sessions, in one step.
						</p>
						<div id="disable-command-{person.id}" hidden={open !== 'disable'}>
							<CopyCommand command={commands.disable(person.email)} />
						</div>
					</section>
				</div>
			</div>
		{/if}

		<section class="mt-auto bg-surface-2/40 px-4 pt-3 pb-4">
			<CopyCommand label="The same in the terminal" command={commands.accessOf(person.email)} />
		</section>
	</div>
</article>

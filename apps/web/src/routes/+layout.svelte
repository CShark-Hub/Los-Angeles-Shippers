<script lang="ts">
	import './layout.css';
	import { goto, invalidateAll } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import favicon from '$lib/assets/favicon.svg';
	import AppButton from '$lib/components/AppButton.svelte';
	import DisconnectButton from '$lib/components/DisconnectButton.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Logo from '$lib/components/Logo.svelte';
	import { roleLabel } from '$lib/format';
	import { approvals, isApprover } from '$lib/approvals/approvals.svelte';
	import { session } from '$lib/session.svelte';
	import { shell } from '$lib/shell.svelte';

	let { children } = $props();

	/** Whether the side panel is open on a narrow screen; a wide one always shows it. */
	let open = $state(false);

	const signedIn = $derived(session.status === 'signed-in');
	const approver = $derived(signedIn && isApprover(session.me));
	// the admin panel and the audit are an admin's alone
	const admin = $derived(signedIn && session.me?.user.role === 'admin');
	const links = $derived([
		{ path: '/ask', label: 'Ask', icon: 'chat', shown: true },
		{ path: '/approvals', label: 'Approvals', icon: 'shield', shown: approver },
		{ path: '/settings', label: 'Settings', icon: 'settings', shown: true }
	] as const);
	const adminLinks = [
		{ path: '/admin/overview', label: 'Overview', icon: 'chart' },
		{ path: '/admin/people', label: 'People & access', icon: 'users' },
		{ path: '/admin/repositories', label: 'Repositories', icon: 'repo' },
		{ path: '/audit', label: 'Audit', icon: 'audit' }
	] as const;

	// An approver hears of what waits for them while any tab of the app is open; nobody else asks.
	$effect(() => {
		if (approver) approvals.start();
		else approvals.stop();
	});

	// Whatever ends a session, Disconnect or a 401 on any request, takes the person to Sign in.
	$effect(() => {
		if (session.status === 'signed-out' && page.url.pathname !== '/signin')
			void goto(resolve('/signin'));
	});

	// Going somewhere closes the side panel on a narrow screen.
	$effect(() => {
		void page.url.pathname;
		open = false;
	});

	// The way past the side panel for a keyboard: the first stop of every page, seen only when it has the focus.
	function skip(event: MouseEvent) {
		event.preventDefault();
		document.getElementById('main')?.focus();
	}

	// The first letters of the name, for the round badge beside it.
	function initials(name: string): string {
		return name
			.split(/\s+/)
			.filter(Boolean)
			.slice(0, 2)
			.map((word) => word[0].toUpperCase())
			.join('');
	}

	// A row of the side panel: the page it leads to is lit, with a mark at the panel's edge.
	const navLink =
		'group relative flex h-8 items-center gap-2.5 rounded-control px-2.5 text-sm text-text-2 focus-ring transition-colors hover:bg-surface-2 hover:text-text aria-[current=page]:bg-accent-soft aria-[current=page]:font-medium aria-[current=page]:text-text';
	const navMark =
		'absolute top-1.5 bottom-1.5 -left-3 w-0.75 rounded-r-full bg-accent opacity-0 transition-opacity group-aria-[current=page]:opacity-100';
	const navIcon = 'text-text-3 group-hover:text-text-2 group-aria-[current=page]:text-accent-text';

	async function retry() {
		await session.restore();
		await invalidateAll();
	}
</script>

<svelte:head>
	<title>Mercury</title>
	<link rel="icon" href={favicon} />
</svelte:head>

<svelte:window
	onkeydown={(event) => {
		if (open && event.key === 'Escape') open = false;
	}}
/>

{#if signedIn}
	<a
		href="#main"
		onclick={skip}
		class="sr-only focus-ring focus:not-sr-only focus:fixed focus:top-2 focus:left-2 focus:z-50 focus:rounded-lg focus:border focus:border-border-strong focus:bg-surface focus:px-4 focus:py-2 focus:font-medium"
		>Skip to the page</a
	>

	<!-- a narrow screen: a bar with the mark and the button that opens the side panel -->
	<div
		class="sticky top-0 z-20 flex h-14 items-center gap-2 border-b border-border bg-sidebar px-3 lg:hidden"
	>
		<button
			type="button"
			aria-label="Menu"
			aria-expanded={open}
			aria-controls="sidebar"
			onclick={() => (open = !open)}
			class="grid size-9 place-items-center rounded-control text-text-2 focus-ring hover:bg-surface-2 hover:text-text"
		>
			<Icon name={open ? 'close' : 'menu'} size={20} />
		</button>
		<a
			href={resolve('/ask')}
			class="flex items-center gap-2 rounded-control font-semibold tracking-tight focus-ring"
			><Logo size={24} />Mercury</a
		>
	</div>

	{#if open}
		<div
			aria-hidden="true"
			class="fixed inset-0 z-30 bg-bg/70 backdrop-blur-sm lg:hidden"
			onclick={() => (open = false)}
		></div>
	{/if}

	<aside
		id="sidebar"
		class="{open
			? 'flex'
			: 'hidden'} fixed inset-y-0 left-0 z-40 w-64 flex-col border-r border-border bg-sidebar shadow-raised lg:flex lg:shadow-none"
	>
		<div class="flex h-14 shrink-0 items-center justify-between px-4">
			<a
				href={resolve('/ask')}
				class="flex items-center gap-2.5 rounded-control text-brand font-semibold tracking-tight text-text focus-ring"
				><Logo size={26} />Mercury</a
			>
			<button
				type="button"
				aria-label="Close the menu"
				onclick={() => (open = false)}
				class="-mr-1.5 grid size-9 place-items-center rounded-control text-text-2 focus-ring hover:bg-surface-2 hover:text-text lg:hidden"
			>
				<Icon name="close" size={20} />
			</button>
		</div>

		<nav aria-label="Main" class="flex min-h-0 shrink-0 flex-col gap-px px-3 pt-1">
			{#each links.filter((l) => l.shown) as link (link.path)}
				<a
					href={resolve(link.path)}
					aria-current={page.url.pathname.startsWith(link.path) ? 'page' : undefined}
					class={navLink}
				>
					<span aria-hidden="true" class={navMark}></span>
					<span class={navIcon}><Icon name={link.icon} size={17} /></span>
					<span class="flex-1">{link.label}</span>
					{#if link.path === '/approvals' && approvals.decidable.length > 0}<span
							aria-hidden="true"
							class="grid h-5 min-w-5 place-items-center rounded-full bg-accent px-1.5 text-2xs font-semibold text-on-accent tabular-nums"
							>{approvals.decidable.length}</span
						><span class="sr-only">, {approvals.decidable.length} waiting</span>{/if}
				</a>
			{/each}

			{#if admin}
				<!-- the admin's own group: what Mercury does, who may ask about what, the repositories -->
				<div role="group" aria-labelledby="admin-group" class="mt-4 flex flex-col gap-px">
					<span id="admin-group" class="px-2.5 pb-1 text-xs font-medium text-text-3"
						>Administration</span
					>
					{#each adminLinks as link (link.path)}
						<a
							href={resolve(link.path)}
							aria-current={page.url.pathname.startsWith(link.path) ? 'page' : undefined}
							class={navLink}
						>
							<span aria-hidden="true" class={navMark}></span>
							<span class={navIcon}><Icon name={link.icon} size={17} /></span>
							<span class="flex-1">{link.label}</span>
						</a>
					{/each}
				</div>
			{/if}
		</nav>

		{#if shell.panel}
			<div class="mt-3 flex min-h-0 flex-1 flex-col border-t border-border-subtle px-3 pt-3">
				{@render shell.panel()}
			</div>
		{:else}
			<div class="flex-1"></div>
		{/if}

		<!-- who is signed in, and the way out -->
		<div class="flex items-center gap-2.5 border-t border-border-subtle py-2.5 pr-2.5 pl-3.5">
			<span
				aria-hidden="true"
				class="grid size-8 shrink-0 place-items-center rounded-full bg-accent-soft text-2xs font-semibold text-accent-text"
				>{initials(session.me?.user.display_name ?? '')}</span
			>
			<span class="flex min-w-0 flex-1 flex-col">
				<span class="truncate text-compact leading-tight font-medium text-text"
					>{session.me?.user.display_name}</span
				>
				<span class="truncate text-2xs text-text-3"
					>{session.me ? roleLabel(session.me.user.role) : ''}</span
				>
			</span>
			<DisconnectButton />
		</div>
	</aside>
{/if}

<div class={signedIn ? 'lg:pl-64' : ''}>
	{#if session.status === 'unreachable'}
		<main class="mx-auto flex max-w-xl flex-col gap-4 p-8">
			<h1 class="text-2xl font-medium">Mercury</h1>
			<p role="alert" class="text-danger">{session.failure?.message}</p>
			{#if session.failure?.hint}<p class="text-text-2">{session.failure.hint}</p>{/if}
			<div><AppButton onclick={retry}>Try again</AppButton></div>
		</main>
	{:else}
		{@render children()}
	{/if}
</div>

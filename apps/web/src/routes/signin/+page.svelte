<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { mockSignInAccounts } from '$lib/api';
	import type { SignInAccount } from '$lib/api/mock';
	import Avatar from '$lib/components/admin/Avatar.svelte';
	import AppButton from '$lib/components/AppButton.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Logo from '$lib/components/Logo.svelte';
	import { describeSignIn, type Described } from '$lib/errors';
	import { roleLabel } from '$lib/format';
	import { session } from '$lib/session.svelte';

	const promises = [
		{
			icon: 'sparkles',
			title: 'Answers that show their sources',
			body: 'A probe next to the repository turns pull requests, CI and tests into facts, and where a project allows it a code reader adds findings. Every answer says what it rests on.'
		},
		{
			icon: 'shield',
			title: 'Every step is checked',
			body: 'The gateway checks your question, every probe call and every sentence of the answer before you see it.'
		},
		{
			icon: 'audit',
			title: 'Every question is audited',
			body: 'A tamper-evident trail records who asked what, what was withheld and who signed off.'
		}
	] as const;

	// A page-unique id for the gradient of the ring (the mark's ring, drawn large behind the form).
	const id = $props.id();

	// The only place a typed token lives: this field, until the call is made.
	let token = $state('');
	let busy = $state(false);
	let problem = $state<Described | null>(null);
	/** The mock account being signed in as, while its call runs. */
	let picked = $state<string | null>(null);

	/** Opens a session with the token and goes home; a refusal is said in plain words instead. */
	async function open(value: string) {
		busy = true;
		problem = null;
		try {
			await session.signIn(value);
			await goto(resolve('/'));
		} catch (error) {
			problem = describeSignIn(error);
		} finally {
			busy = false;
		}
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (busy || token.trim() === '') return;
		try {
			await open(token.trim());
		} finally {
			// whatever came of it, the token is not in the page any more
			token = '';
		}
	}

	/** Mock mode: signs in as the account on the tile, with the mock's own token for it. */
	async function pick(account: SignInAccount) {
		if (busy) return;
		picked = account.token;
		try {
			await open(account.token);
		} finally {
			picked = null;
		}
	}
</script>

<svelte:head>
	<title>Sign in · Mercury</title>
</svelte:head>

{#snippet problemAlert()}
	{#if problem}
		<div
			id="token-problem"
			role="alert"
			class="flex gap-3 rounded-card border border-danger/40 bg-danger/6 px-4 py-3 text-sm"
		>
			<span
				aria-hidden="true"
				class="mt-px grid size-5 shrink-0 place-items-center rounded-full bg-danger text-surface"
				><Icon name="close" size={12} weight={2.75} /></span
			>
			<div class="min-w-0">
				<p class="font-semibold text-text">{problem.message}</p>
				{#if problem.hint}<p class="mt-1 text-text-2">{problem.hint}</p>{/if}
			</div>
		</div>
	{/if}
{/snippet}

{#snippet tokenForm(withProblem: boolean)}
	<form onsubmit={submit} class="flex flex-col gap-5">
		<div class="flex flex-col gap-2">
			<div class="flex items-baseline justify-between gap-2">
				<label for="token" class="text-sm font-medium text-text">Your token</label>
				<span class="text-xs text-text-3"
					>starts with <code
						class="rounded-chip bg-surface-2 px-1 py-px font-mono text-2xs text-text-2 ring-1 ring-border-control ring-inset"
						>mcy_</code
					></span
				>
			</div>
			<!-- the field draws its edge and its focus on the box around it, with the key inside -->
			<div
				class="flex h-10 items-center gap-2.5 rounded-control border border-border-strong bg-surface px-3 text-text-3 shadow-control transition-[border-color,color] duration-150 focus-within:border-accent focus-within:text-accent-text focus-within:outline-3 focus-within:outline-offset-2 focus-within:outline-ring hover:border-text-3 has-aria-[invalid=true]:border-danger"
			>
				<Icon name="key" size={17} />
				<input
					id="token"
					name="token"
					type="password"
					bind:value={token}
					required
					placeholder="Paste your token"
					autocomplete="off"
					autocapitalize="none"
					spellcheck="false"
					aria-describedby="token-help{problem ? ' token-problem' : ''}"
					aria-invalid={problem ? 'true' : undefined}
					class="h-full w-full min-w-0 bg-transparent font-mono text-sm text-text outline-none placeholder:font-sans placeholder:text-text-3"
				/>
			</div>
			<p id="token-help" class="text-xs leading-relaxed text-text-3">
				It opens a session in this browser and is not kept: the page forgets it as soon as you press
				Sign in.
			</p>
		</div>

		{#if withProblem}{@render problemAlert()}{/if}

		<AppButton type="submit" wide disabled={busy}
			>{busy ? 'Signing in…' : 'Sign in'}{#if !busy}<Icon
					name="chevron"
					size={15}
					weight={2.2}
				/>{/if}</AppButton
		>
	</form>
{/snippet}

<!--
	One page on one ground: what Mercury is on the left, the form on the right, the light and the dots
	running under both, and the ring of the mark passing behind the form to tie the two together.
-->
<div class="relative isolate min-h-screen overflow-hidden bg-bg">
	<div aria-hidden="true" class="pointer-events-none absolute inset-0 -z-10">
		<div class="wash absolute inset-0"></div>
		<div class="dots absolute inset-0"></div>
		<div class="glow glow-accent absolute"></div>
		<div class="glow glow-accent-2 absolute"></div>
	</div>

	<div
		class="mx-auto grid min-h-screen w-full max-w-[84rem] grid-cols-1 lg:grid-cols-[minmax(0,1fr)_27rem] lg:gap-x-16 lg:px-12 xl:grid-cols-[minmax(0,1fr)_28rem] xl:gap-x-20 xl:px-16"
	>
		<!-- What Mercury is, beside the form on a wide screen: words only, nothing to act on. -->
		<aside class="hidden lg:flex lg:flex-col lg:justify-between lg:gap-10 lg:py-12">
			<div class="flex items-center gap-3">
				<Logo size={40} />
				<div>
					<p class="text-lg leading-tight font-semibold tracking-tight text-text">Mercury</p>
					<p class="text-sm text-text-3">Project answers, checked before you read them</p>
				</div>
			</div>

			<div class="max-w-2xl">
				<p
					class="text-5xl leading-[1.08] font-semibold tracking-tight text-balance text-text xl:text-[3.25rem]"
				>
					Ask about your project&nbsp;— <span class="text-accent-text">answered in seconds.</span>
				</p>

				<!-- the three promises as one path a question takes, each stage on the line to the next -->
				<ul class="mt-12 flex max-w-[32rem] flex-col gap-6">
					{#each promises as item, i (item.title)}
						<li class="relative flex gap-4">
							{#if i < promises.length - 1}
								<span aria-hidden="true" class="rail absolute top-12 -bottom-4 left-5 w-px"></span>
							{/if}
							<span
								class="node grid size-10 shrink-0 place-items-center rounded-full text-accent-text shadow-control ring-1 ring-accent/30 ring-inset"
							>
								<Icon name={item.icon} size={18} />
							</span>
							<div class="min-w-0 pt-2">
								<p class="text-base leading-6 font-semibold text-text">{item.title}</p>
								<p class="mt-1 text-sm leading-relaxed text-text-2">{item.body}</p>
							</div>
						</li>
					{/each}
				</ul>
			</div>

			<p class="flex items-center gap-2 text-xs text-text-3">
				<span class="shrink-0"><Icon name="lock" size={14} /></span>
				Your token opens a session and is never kept in this browser.
			</p>
		</aside>

		<main class="flex items-center justify-center px-6 py-12 lg:px-0">
			<div class="flex w-full max-w-md flex-col gap-4 lg:max-w-none">
				<div class="flex items-center gap-3 lg:hidden">
					<Logo size={32} />
					<span class="text-lg font-semibold tracking-tight">Mercury</span>
				</div>

				<!-- the form on a raised panel: paste the token and sign in, or in mock mode pick an account -->
				<div class="relative">
					<svg
						aria-hidden="true"
						focusable="false"
						class="orbit pointer-events-none absolute top-1/2 left-1/2 hidden -translate-x-1/2 -translate-y-1/2 overflow-visible lg:block"
						width="800"
						height="440"
						viewBox="-400 -220 800 440"
					>
						<defs>
							<linearGradient
								id="{id}-ring"
								gradientUnits="userSpaceOnUse"
								x1="-370"
								y1="0"
								x2="370"
								y2="0"
							>
								<stop offset="0" class="stop-fade-accent" />
								<stop offset="0.3" class="stop-accent" />
								<stop offset="0.72" class="stop-accent-2" />
								<stop offset="1" class="stop-fade-accent-2" />
							</linearGradient>
						</defs>
						<g transform="rotate(-22)">
							<ellipse
								cx="0"
								cy="0"
								rx="370"
								ry="116"
								fill="none"
								stroke="url(#{id}-ring)"
								stroke-width="1.25"
							/>
							<circle class="moon" cx="-320.4" cy="-58" r="4.5" />
						</g>
					</svg>
					<div
						class="relative overflow-hidden rounded-panel border border-border bg-surface-raised shadow-raised"
					>
						<span aria-hidden="true" class="edge pointer-events-none absolute inset-x-0 top-0 h-px"
						></span>
						<div class="flex flex-col gap-7 px-8 pt-8 pb-8">
							<div class="flex flex-col gap-1.5">
								<div class="flex items-center justify-between gap-3">
									<h1 class="text-2xl font-semibold tracking-tight text-text">
										Sign in to Mercury
									</h1>
									{#if mockSignInAccounts}
										<span
											class="flex shrink-0 items-center gap-1.5 rounded-chip bg-surface-2 px-2 py-1 text-2xs font-medium text-text-2 ring-1 ring-border-control ring-inset"
											><Icon name="terminal" size={12} />Mock mode</span
										>
									{/if}
								</div>
								<p class="text-sm text-text-2">
									{mockSignInAccounts
										? 'Pick who to sign in as.'
										: 'Use the personal access token your admin gave you.'}
								</p>
							</div>

							{#if session.notice}
								<p
									role="status"
									class="-mt-1 flex gap-3 rounded-card border border-info/35 bg-info/6 px-4 py-3 text-sm text-text"
								>
									<span
										aria-hidden="true"
										class="mt-px grid size-5 shrink-0 place-items-center rounded-full bg-info text-surface"
										><Icon name="info-mark" size={12} weight={2.75} /></span
									>
									<span class="min-w-0">{session.notice}</span>
								</p>
							{/if}

							{#if mockSignInAccounts}
								<!-- mock mode: one tile per person, so nobody types a token; a refusal is said under them -->
								{#await mockSignInAccounts then accounts}
									<ul class="grid grid-cols-3 gap-2">
										{#each accounts as account (account.token)}
											{#if account.kind === 'person'}
												{@const signing = picked === account.token}
												<li>
													<button
														type="button"
														onclick={() => pick(account)}
														disabled={busy}
														aria-busy={signing}
														class="group flex w-full cursor-pointer flex-col items-center rounded-card border bg-surface px-2 pt-3.5 pb-3 text-center shadow-control focus-ring transition-[border-color,background-color,box-shadow,translate] duration-150 hover:border-accent/45 hover:bg-accent-soft/40 hover:shadow-card disabled:cursor-default motion-safe:hover:-translate-y-px {signing
															? 'border-accent bg-accent-soft/50'
															: 'border-border-control disabled:opacity-55'}"
													>
														<Avatar name={account.name} size={34} selected={signing} />
														<span
															class="mt-2.5 block w-full truncate text-compact leading-5 font-semibold text-text"
															><span class="sr-only">Sign in as </span>{account.name}</span
														>
														<span
															class="block w-full truncate text-2xs leading-4 {signing
																? 'text-accent-text'
																: 'text-text-3'}"
															>{signing ? 'Signing in…' : roleLabel(account.role)}</span
														>
													</button>
												</li>
											{/if}
										{/each}
									</ul>
								{/await}
								{@render problemAlert()}
							{:else}
								{@render tokenForm(true)}
							{/if}

							<!-- what happens to the token, in small print under the one action -->
							<p class="-mt-2 flex gap-2.5 text-xs leading-relaxed text-text-3">
								<span class="mt-0.5 shrink-0"><Icon name="lock" size={14} /></span>
								<span class="min-w-0">
									The core checks the token and answers with a session cookie this page can't read.
									The token itself is kept nowhere in the browser: not in storage, not in the
									address.
								</span>
							</p>
						</div>

						{#if mockSignInAccounts}
							<!-- mock mode's extras, quiet at the foot: the two refusals, and a field for any other token -->
							{#await mockSignInAccounts then accounts}
								<div
									class="flex flex-col gap-3 border-t border-border-subtle bg-surface-2 px-8 py-4"
								>
									<div class="flex items-center justify-between gap-3">
										<p class="text-2xs text-text-3">Or try a refusal:</p>
										<ul class="flex gap-1.5">
											{#each accounts as account (account.token)}
												{#if account.kind === 'refused'}
													<li>
														<button
															type="button"
															onclick={() => pick(account)}
															disabled={busy}
															class="flex h-7 cursor-pointer items-center gap-1.5 rounded-control border border-border-control bg-surface px-2.5 text-2xs font-medium whitespace-nowrap text-text-2 shadow-control focus-ring transition-colors hover:border-warning/50 hover:text-text disabled:cursor-default disabled:opacity-55"
														>
															<span aria-hidden="true" class="size-1.5 rounded-full bg-warning"
															></span>
															{account.name}
														</button>
													</li>
												{/if}
											{/each}
										</ul>
									</div>
									<details class="group">
										<summary
											class="flex w-fit cursor-pointer list-none items-center gap-1.5 rounded-chip text-2xs font-medium text-text-2 focus-ring hover:text-text [&::-webkit-details-marker]:hidden"
										>
											<Icon name="key" size={13} />
											Paste a token instead
											<span class="transition-transform group-open:rotate-180"
												><Icon name="chevron-down" size={13} /></span
											>
										</summary>
										<div class="pt-4">{@render tokenForm(false)}</div>
									</details>
								</div>
							{/await}
						{/if}
					</div>
				</div>
			</div>
		</main>
	</div>
</div>

<style>
	/*
	 * The ground under both halves: the inset surface in the top left, where the words are, easing
	 * into the page under the form, so the raised form stands on the quieter side.
	 */
	.wash {
		background: radial-gradient(
			130% 110% at 0% 0%,
			var(--surface-2) 0%,
			color-mix(in srgb, var(--surface-2) 45%, var(--bg)) 45%,
			var(--bg) 80%
		);
	}

	/*
	 * The dotted ground behind the words, fading out from the top left so the dots sit behind the
	 * mark and the headline and leave the rest calm.
	 */
	.dots {
		background-image: radial-gradient(var(--border-control) 1px, transparent 1px);
		background-size: 22px 22px;
		mask-image: radial-gradient(ellipse 60% 70% at 12% 22%, black 25%, transparent 100%);
		opacity: 0.65;
	}

	/*
	 * The two soft lights. Each gradient carries its own falloff, so no blur filter has to be
	 * painted. The neutral look takes them at half strength, so the secondary text over them keeps
	 * its contrast; the demo look at full.
	 */
	.glow {
		opacity: 0.55;
	}
	:global([data-theme='demo']) .glow {
		opacity: 1;
	}
	.glow-accent {
		top: calc(-10rem - 140px);
		left: calc(-6rem - 140px);
		width: 880px;
		height: 880px;
		background: radial-gradient(
			circle,
			color-mix(in srgb, var(--accent) 13%, transparent) 0,
			color-mix(in srgb, var(--accent) 11.5%, transparent) 70px,
			color-mix(in srgb, var(--accent) 8%, transparent) 150px,
			color-mix(in srgb, var(--accent) 4%, transparent) 230px,
			color-mix(in srgb, var(--accent) 1.2%, transparent) 310px,
			color-mix(in srgb, var(--accent) 0.1%, transparent) 380px,
			transparent 440px
		);
	}
	.glow-accent-2 {
		/* on the light ground the mint reads as a grey stain: the neutral look goes without it */
		opacity: 0;
		right: -260px;
		bottom: -300px;
		width: 900px;
		height: 900px;
		background: radial-gradient(
			circle,
			color-mix(in srgb, var(--accent-2) 8%, transparent) 0,
			color-mix(in srgb, var(--accent-2) 7%, transparent) 80px,
			color-mix(in srgb, var(--accent-2) 4.8%, transparent) 170px,
			color-mix(in srgb, var(--accent-2) 2.4%, transparent) 260px,
			color-mix(in srgb, var(--accent-2) 0.8%, transparent) 350px,
			color-mix(in srgb, var(--accent-2) 0.1%, transparent) 410px,
			transparent 450px
		);
	}

	/* The line from one stage of the path to the next, fading as it reaches the next stage. */
	.rail {
		background: linear-gradient(
			to bottom,
			color-mix(in srgb, var(--accent) 55%, transparent),
			color-mix(in srgb, var(--accent) 12%, transparent)
		);
	}

	/* A stage: the accent's tint laid on a card, so the line and the dots do not show through. */
	.node {
		background: linear-gradient(var(--accent-soft), var(--accent-soft)), var(--surface);
	}

	/* The ring: indigo where it leaves the words, mint where it passes the form; and its moon. */
	.orbit {
		z-index: -1;
		opacity: 0.6;
	}
	:global([data-theme='demo']) .orbit {
		opacity: 0.85;
	}
	.stop-fade-accent {
		stop-color: var(--accent);
		stop-opacity: 0;
	}
	.stop-accent {
		stop-color: var(--accent);
		stop-opacity: 0.7;
	}
	.stop-accent-2 {
		stop-color: var(--accent-2);
		stop-opacity: 0.6;
	}
	.stop-fade-accent-2 {
		stop-color: var(--accent-2);
		stop-opacity: 0;
	}
	.moon {
		fill: var(--accent-2);
	}

	/* A hairline of light along the top of the form, in the two accents of the mark. */
	.edge {
		background: linear-gradient(
			90deg,
			transparent,
			color-mix(in srgb, var(--accent) 70%, transparent) 30%,
			color-mix(in srgb, var(--accent-2) 60%, transparent) 75%,
			transparent
		);
	}
</style>

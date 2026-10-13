<script lang="ts">
	import { onMount, tick, untrack } from 'svelte';

	import {
		approvals,
		isApprover,
		MAX_NOTE,
		noteProblem,
		type Held
	} from '$lib/approvals/approvals.svelte';
	import { flaggedRanges, unplaced } from '$lib/approvals/highlight';
	import { grouped, inline, markedBlocks, type Inline } from '$lib/ask/answer-text';
	import type { Flag } from '$lib/api';
	import { DOT, classWords, flagWords, probability, signoffWords, type Tone } from '$lib/ask/words';
	import AppButton from '$lib/components/AppButton.svelte';
	import Chip from '$lib/components/ask/Chip.svelte';
	import Meter from '$lib/components/ask/Meter.svelte';
	import Notice from '$lib/components/ask/Notice.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { describeMoment, roleLabel } from '$lib/format';
	import { projects } from '$lib/ask/projects.svelte';
	import { session } from '$lib/session.svelte';

	const approver = $derived(isApprover(session.me));
	/** The note typed for each held answer, kept on this page only. */
	let notes = $state<Record<string, string>>({});
	/** The clock the time left is read against, moved on twice a minute. */
	let now = $state(Date.now());

	// The list is polled from the layout; coming to the page asks at once.
	onMount(() => {
		if (approver) void approvals.refresh();
		// the projects' names; a held answer names its project by id
		if (projects.list === null) void projects.load();
		const clock = setInterval(() => (now = Date.now()), 30_000);
		return () => clearInterval(clock);
	});

	function projectName(project: string | null): string {
		if (project === null) return 'Across projects';
		return projects.find(project)?.display_name ?? project;
	}

	// The queue: what waits for the person's decision first, then their own questions, which wait for
	// another approver; each by its deadline, the soonest first (the core sends the oldest first, which
	// is the same order while every hold lasts as long; a stable sort keeps its order on a tie).
	const byDeadline = (a: Held, b: Held) => Date.parse(a.expires_at) - Date.parse(b.expires_at);
	const decidable = $derived([...approvals.decidable].sort(byDeadline));
	const own = $derived(approvals.items.filter((item) => item.asked_by_me).sort(byDeadline));
	const queue = $derived([...decidable, ...own]);
	const marked = $derived(decidable.reduce((n, item) => n + item.flags.length, 0));
	const soonest = $derived<Held | null>(decidable[0] ?? own[0] ?? null);

	/** Why an answer waits, in form: its tone along the card's top, the wash, the mark and its words. */
	const REASON = {
		review_band: {
			title: 'A check was unsure',
			icon: 'alert',
			edge: 'bg-warning',
			wash: 'from-warning/7',
			mark: 'bg-warning/15 text-warning ring-warning/25'
		},
		class: {
			title: 'Always read first',
			icon: 'shield',
			edge: 'bg-accent',
			wash: 'from-accent/7',
			mark: 'bg-accent-soft text-accent-text ring-accent-text/20'
		}
	} as const;
	type Reason = keyof typeof REASON;

	/** How many of the answers to decide wait for each reason, in the order the screen names them. */
	const reasons = $derived(
		(['review_band', 'class'] as Reason[])
			.map((reason) => ({ reason, count: decidable.filter((i) => i.reason === reason).length }))
			.filter((row) => row.count > 0)
	);

	/** Whether the page refused this card's note, while the note still reads as it did then. */
	function noteRefused(id: string): boolean {
		const failure = approvals.decisionFailure;
		if (failure?.id !== id) return false;
		return failure.failure.message === noteProblem(notes[id] ?? '');
	}

	const HOUR = 3_600_000;

	/** How long a held answer has left before it expires, and how urgent that is. */
	function timeLeft(expiresAt: string, at: number): { text: string; tone: Tone } {
		const left = Date.parse(expiresAt) - at;
		if (left <= 0) return { text: 'Expired', tone: 'bad' };
		if (left < HOUR) {
			return { text: `${Math.max(1, Math.floor(left / 60_000))} min left`, tone: 'bad' };
		}
		if (left < 4 * HOUR) {
			const hours = Math.floor(left / HOUR);
			const minutes = Math.floor((left % HOUR) / 60_000);
			return { text: minutes ? `${hours} h ${minutes} min left` : `${hours} h left`, tone: 'warn' };
		}
		return { text: `${Math.round(left / HOUR)} h left`, tone: 'neutral' };
	}

	async function reject(id: string) {
		await approvals.decide(id, 'reject', notes[id] ?? '');
		if (approvals.outcome?.id === id) delete notes[id];
	}

	// A decision takes its card, and the button the keyboard was on, with it: the focus goes to what
	// came of it. Left alone when it is somewhere that still exists (a note the page refused stays
	// under the cursor, and its alert is read out).
	$effect(() => {
		if (approvals.outcome || approvals.decisionFailure) untrack(() => void settle());
	});

	async function settle() {
		await tick();
		const active = document.activeElement;
		if (active && active !== document.body && document.contains(active)) return;
		document.querySelector<HTMLElement>('[data-settled] [data-focus-target]')?.focus();
	}
</script>

<svelte:head>
	<title>Approvals · Mercury</title>
</svelte:head>

<main id="main" tabindex="-1" class="flex flex-col gap-5 px-4 py-6 focus:outline-none sm:px-6">
	<!-- the page's header: what the screen is for on the left; on the right, how the person hears of a
	     new answer (the browser's notice, and how often the list is checked) -->
	<header class="flex flex-wrap items-start justify-between gap-x-8 gap-y-3">
		<div class="flex min-w-0 flex-1 basis-96 flex-col gap-1">
			<h1 class="text-xl font-semibold tracking-tight text-text">Approvals</h1>
			{#if approver}
				<p class="max-w-3xl text-compact leading-5 text-pretty text-text-2">
					Answers Mercury held back until an approver reads them. Approving sends the answer to the
					person who asked, exactly as it is shown here; to change something, reject it with a note.
				</p>
			{/if}
		</div>
		{#if approver}
			<!-- a button sits flush right with the line under it; two status lines share one icon column -->
			<div
				class="flex max-w-sm shrink-0 flex-col gap-1.5 {approvals.permission === 'granted'
					? 'items-start'
					: 'items-end'}"
			>
				{#if approvals.permission === 'default'}
					<AppButton variant="quiet" onclick={() => approvals.allowNotifications()}
						>Tell me when an answer waits</AppButton
					>
				{:else if approvals.permission === 'granted'}
					<p class="flex items-start gap-2 text-xs text-text-2">
						<span
							aria-hidden="true"
							class="grid size-5 shrink-0 place-items-center rounded-full bg-accent-2/15 text-accent-2"
							><Icon name="check" size={11} weight={2.75} /></span
						>This browser tells you when a new answer waits, while Mercury is open in a tab.
					</p>
				{/if}
				<p class="flex items-center gap-2 text-2xs text-text-3">
					<span
						aria-hidden="true"
						class="flex shrink-0 justify-center {approvals.permission === 'granted' ? 'w-5' : ''}"
						><Icon name="refresh" size={12} /></span
					>The list is checked every 30 seconds while Mercury is open.
				</p>
			</div>
		{/if}
	</header>

	{#if approver && approvals.loaded && approvals.items.length > 0}
		<!-- the queue at a glance, counted from the list on screen: only what this person has (an
		     approver whose own questions never wait here sees no figure for them) -->
		<dl
			class="grid overflow-clip rounded-card border border-border bg-surface shadow-card sm:auto-cols-fr sm:grid-flow-col sm:divide-x sm:divide-border-subtle"
		>
			<div class="flex flex-col justify-between gap-1 px-4 py-3">
				<dt class="flex items-center gap-1.5 text-xs font-medium text-text-2">
					<span aria-hidden="true" class="flex text-accent-text"
						><Icon name="shield" size={13} /></span
					>Waiting for you
				</dt>
				<dd class="flex flex-wrap items-baseline gap-x-2.5">
					<span class="text-figure font-semibold tracking-tight text-text tabular-nums"
						>{decidable.length}</span
					>
					<span class="text-xs text-text-3 tabular-nums"
						>{decidable.length === 0
							? 'Nothing to decide'
							: marked === 0
								? 'Nothing marked'
								: `${marked} marked ${marked === 1 ? 'sentence' : 'sentences'}`}</span
					>
				</dd>
			</div>
			{#if reasons.length}
				<!-- why they wait, one row a reason in its tone, as the cards below name it -->
				<div
					class="flex flex-col justify-between gap-1 border-t border-border-subtle px-4 py-3 sm:border-t-0"
				>
					<dt class="flex items-center gap-1.5 text-xs font-medium text-text-2">
						<span aria-hidden="true" class="flex text-text-3"><Icon name="flag" size={13} /></span
						>Why they wait
					</dt>
					{#each reasons as row (row.reason)}
						<dd class="flex max-w-60 items-center gap-2 text-compact text-text-2">
							<span
								aria-hidden="true"
								class="size-2 shrink-0 rounded-full {REASON[row.reason].edge}"
							></span>
							<span class="min-w-0 flex-1 truncate">{REASON[row.reason].title}</span>
							<span class="font-semibold text-text tabular-nums">{row.count}</span>
						</dd>
					{/each}
				</div>
			{/if}
			{#if soonest}
				{@const left = timeLeft(soonest.expires_at, now)}
				<div
					class="flex flex-col justify-between gap-1 border-t border-border-subtle px-4 py-3 sm:border-t-0"
				>
					<dt class="flex items-center gap-1.5 text-xs font-medium text-text-2">
						<span aria-hidden="true" class="flex text-text-3"><Icon name="clock" size={13} /></span
						>Soonest deadline
						{#if left.tone !== 'neutral'}<span
								aria-hidden="true"
								class="size-1.5 shrink-0 rounded-full {DOT[left.tone]}"
							></span>{/if}
					</dt>
					<dd class="flex flex-wrap items-baseline gap-x-2.5">
						<span class="text-figure font-semibold tracking-tight text-text tabular-nums"
							>{left.text}</span
						>
						<span class="text-xs text-text-3 tabular-nums"
							>{soonest.asked_by_me ? 'Waits until' : 'Decide by'}
							{describeMoment(soonest.expires_at)}</span
						>
					</dd>
				</div>
			{/if}
			{#if own.length}
				<div
					class="flex flex-col justify-between gap-1 border-t border-border-subtle px-4 py-3 sm:border-t-0"
				>
					<dt class="flex items-center gap-1.5 text-xs font-medium text-text-2">
						<span aria-hidden="true" class="flex text-text-3"><Icon name="user" size={13} /></span
						>Your questions waiting
					</dt>
					<dd class="flex flex-wrap items-baseline gap-x-2.5">
						<span class="text-figure font-semibold tracking-tight text-text tabular-nums"
							>{own.length}</span
						>
						<span class="text-xs text-text-3">For another approver</span>
					</dd>
				</div>
			{/if}
		</dl>
	{/if}

	{#if !approver}
		<Notice title="Nothing to approve" tone="info">
			<p class="text-text-2">
				You don't approve answers for any project. Your admin can make you an approver.
			</p>
		</Notice>
	{:else}
		{#if approvals.failure}
			<Notice title={approvals.failure.message} tone="bad" alert>
				{#if approvals.failure.hint}<p class="text-text-2">{approvals.failure.hint}</p>{/if}
			</Notice>
		{/if}

		{#if approvals.outcome}
			<div data-settled>
				<Notice
					title={approvals.outcome.decision === 'approve' ? 'Approved' : 'Rejected'}
					tone="good"
				>
					<p class="text-text-2">
						{approvals.outcome.decision === 'approve'
							? 'The person who asked gets the answer exactly as you saw it.'
							: 'The person who asked gets the refusal with your note.'}
					</p>
				</Notice>
			</div>
		{/if}

		<!-- above the list: a decision that lost the race takes its card with it -->
		{#if approvals.decisionFailure}
			<div data-settled>
				<Notice title={approvals.decisionFailure.failure.message} tone="bad" alert>
					{#if approvals.decisionFailure.failure.hint}<p class="text-text-2">
							{approvals.decisionFailure.failure.hint}
						</p>{/if}
				</Notice>
			</div>
		{/if}

		{#if approvals.loaded && approvals.items.length === 0}
			<!-- nothing waits: say so, with nothing to act on -->
			<div
				class="flex flex-col items-center gap-4 rounded-card border border-border bg-surface px-6 py-12 text-center shadow-card"
			>
				<span
					aria-hidden="true"
					class="grid size-14 place-items-center rounded-full bg-accent-2/12 text-accent-2 ring-[10px] ring-accent-2/5"
					><Icon name="check" size={24} weight={2.4} /></span
				>
				<div class="flex flex-col gap-1">
					<p class="text-lg font-semibold tracking-tight text-text">All clear</p>
					<p class="text-sm text-text-2">No answers wait for your sign-off.</p>
				</div>
			</div>
		{:else if !approvals.loaded && !approvals.failure}
			<!-- the first list is on its way: the shape of a card, quietly -->
			<div class="overflow-clip rounded-card border border-border bg-surface shadow-card">
				<div class="flex items-center gap-3 border-b border-border-subtle px-5 py-3">
					<span
						aria-hidden="true"
						class="size-4 shrink-0 rounded-full border-2 border-border-control border-t-accent-text motion-safe:animate-spin"
					></span>
					<p class="text-sm text-text-2">Checking…</p>
				</div>
				<div aria-hidden="true" class="flex flex-col gap-2.5 px-5 py-5 motion-safe:animate-pulse">
					<span class="h-2.5 w-2/3 max-w-xl rounded-full bg-surface-2"></span>
					<span class="h-2.5 w-1/2 max-w-md rounded-full bg-surface-2"></span>
				</div>
			</div>
		{/if}

		<ul class="flex flex-col gap-3">
			{#each queue as item (item.id)}
				<li>
					{#if item.asked_by_me}
						<!-- The person's own question waits for another approver: a slim status row, nothing of its
						     draft before the decision. -->
						<article
							aria-labelledby="held-{item.id}"
							class="flex items-start gap-3 rounded-card border border-border bg-surface/60 px-5 py-3"
						>
							<span
								aria-hidden="true"
								class="mt-px grid size-6 shrink-0 place-items-center rounded-full bg-surface-2 text-text-2 ring-1 ring-border ring-inset"
								><Icon name="user" size={13} weight={2.2} /></span
							>
							<div class="flex min-w-0 flex-1 flex-col gap-1">
								<div class="flex flex-wrap items-center gap-x-3 gap-y-1">
									<h2 id="held-{item.id}" class="text-sm font-semibold text-text">
										{projectName(item.project)} · {classWords(item.class)}
									</h2>
									<span class="hidden h-4 w-px bg-border sm:block" aria-hidden="true"></span>
									<span class="text-xs text-text-3">Your question</span>
									{@render deadline(item)}
								</div>
								<p class="max-w-5xl text-xs leading-relaxed text-text-2">
									<span class="font-medium text-text"
										>Your own question waits for another approver of the project.</span
									>
									{signoffWords(item.reason)} You get the answer, or why it was not released, on the question's
									page in Ask once they decide.
								</p>
							</div>
						</article>
					{:else}
						<!-- One held answer to review, in the answer's frame on Ask: why it waits as its tone along
						     the top and in the header (what, who asked, why, how long is left), the answer as they
						     will get it with each marked sentence in the margin beside it, and the decision as a
						     bar under both. -->
						{@const look = REASON[item.reason] ?? REASON.review_band}
						<article
							aria-labelledby="held-{item.id}"
							class="relative overflow-clip rounded-card border border-border bg-surface shadow-card"
						>
							<span aria-hidden="true" class="absolute inset-x-0 top-0 h-0.5 {look.edge}"></span>
							<header
								class="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-border-subtle bg-linear-to-b to-transparent px-5 py-3 {look.wash}"
							>
								<span
									aria-hidden="true"
									class="grid size-8 shrink-0 place-items-center rounded-full ring-1 ring-inset {look.mark}"
									><Icon name={look.icon} size={15} weight={2.2} /></span
								>
								<div class="flex min-w-0 flex-1 basis-96 flex-col gap-0.5">
									<div class="flex flex-wrap items-center gap-x-3 gap-y-0.5">
										<h2 id="held-{item.id}" class="text-title font-semibold text-text">
											{projectName(item.project)} · {classWords(item.class)}
										</h2>
										<span class="hidden h-4 w-px bg-border sm:block" aria-hidden="true"></span>
										<span class="text-xs text-text-3"
											>{`Asked by a ${roleLabel(item.asker_role).toLowerCase()}`}</span
										>
									</div>
									<p class="text-xs leading-5 text-text-2">
										<span class="font-semibold text-text">{look.title}</span><span
											aria-hidden="true"
											class="mx-1.5 text-text-3">·</span
										>{signoffWords(item.reason)}
									</p>
								</div>
								{@render deadline(item)}
							</header>

							{#if item.draft === null}
								<!-- Someone else's answer that came without its draft: nobody approves what they haven't read. -->
								<div class="flex items-start gap-2.5 px-5 py-4">
									<span class="mt-0.5 text-text-3"><Icon name="refresh" size={14} /></span>
									<p class="text-compact text-text-2">
										The list came without this answer's text. It is asked for again every 30
										seconds; decide once it shows.
									</p>
								</div>
							{:else}
								{@const draft = item.draft}
								<!-- The held answer: the core's checks passed it, and approving delivers exactly this
								     text. How sure Mercury is sits beside its label, as beside "Answer" on Ask; each
								     marked sentence is a note in the margin, level with the prose it marks. -->
								<section
									class="grid gap-x-8 gap-y-2 px-5 pt-4 pb-5 lg:grid-cols-[minmax(0,36rem)_minmax(17rem,1fr)]"
									aria-label="The answer"
								>
									<div class="flex flex-wrap items-center gap-x-3 gap-y-1 lg:col-span-2">
										<p class="text-xs font-medium text-text-3">The answer they will get</p>
										<span aria-hidden="true" class="h-3.5 w-px bg-border"></span>
										<Meter value={draft.confidence} />
									</div>
									<!-- read like the answer on Ask: the prose at a reading measure -->
									<div
										class="flex max-w-measure min-w-0 flex-col gap-2.5 text-title leading-relaxed text-text"
									>
										{#each markedBlocks(draft.text, flaggedRanges(draft.text, item.flags)) as block, b (b)}
											{#if block.kind === 'paragraph'}
												<p>{@render runs(block.parts)}</p>
											{:else if block.kind === 'list'}
												<ul class="flex list-disc flex-col gap-1 pl-5 marker:text-accent-text">
													{#each block.items as parts, j (j)}<li class="pl-0.5">
															{@render runs(parts)}
														</li>{/each}
												</ul>
											{:else}
												<p class="text-compact leading-normal text-text-3">{block.text}</p>
											{/if}
										{/each}
									</div>

									{#if item.flags.length}
										<!-- each flag as a callout: the sentence, what the check feared, and its own words -->
										<ul class="mt-2 flex min-w-0 flex-col gap-2 lg:mt-0.5">
											{#each item.flags as flag, i (i)}
												<li
													class="flex items-start gap-3 rounded-control border border-warning/25 bg-warning/6 px-3.5 py-3"
												>
													<span
														aria-hidden="true"
														class="grid size-6 shrink-0 place-items-center rounded-full bg-warning/15 text-warning"
														><Icon name="flag" size={12} weight={2.4} /></span
													>
													<div class="flex min-w-0 flex-col gap-1">
														<p class="text-compact leading-snug text-text">
															<span class="font-semibold">Marked:</span>
															“{@render runs(inline(flag.sentence))}”
														</p>
														<p class="text-xs text-text-2">
															{flagWords(flag.check)}
															<span class="font-medium text-text tabular-nums"
																>{Math.round(flag.p * 100)}% likely.</span
															>
															{#if unplaced(draft.text, [flag]).length}Not found word for word in
																the answer.{/if}
														</p>
														<p class="font-mono text-2xs leading-snug break-words text-text-3">
															{flag.check}, probability {probability(flag.p)}
														</p>
													</div>
												</li>
											{/each}
										</ul>
									{/if}
								</section>

								<!-- the decision: approve as it is, or reject it with a note; one bar under the answer,
								     on one line where the card is wide enough for the note's label beside its field -->
								{@const refused = noteRefused(item.id)}
								<section
									class="@container border-t border-border-subtle bg-surface-2/60 px-5 py-3"
									aria-label="Your decision"
								>
									<div class="flex flex-wrap items-end gap-x-5 gap-y-3">
										<div class="flex items-center gap-3">
											<AppButton
												disabled={approvals.deciding !== null}
												onclick={() => approvals.decide(item.id, 'approve')}
												><Icon name="check" size={14} weight={2.4} />Approve</AppButton
											>
											<p class="text-2xs text-text-3">Sends it exactly as shown.</p>
										</div>

										<span aria-hidden="true" class="hidden h-8 w-px bg-border-control md:block"
										></span>

										<div
											class="flex min-w-0 flex-1 basis-80 flex-col gap-1.5 @5xl:flex-row @5xl:items-center @5xl:gap-3"
										>
											<label
												class="text-xs font-medium text-text-2 @5xl:shrink-0"
												for="note-{item.id}"
												>Or reject it, with a note for the person who asked</label
											>
											<div class="flex min-w-0 flex-1 gap-2">
												<input
													id="note-{item.id}"
													type="text"
													maxlength={MAX_NOTE}
													autocomplete="off"
													placeholder="What should they know?"
													aria-invalid={refused ? 'true' : undefined}
													bind:value={notes[item.id]}
													class="h-8 min-w-0 flex-1 rounded-control border bg-surface px-3 text-compact text-text shadow-control focus-ring transition-colors placeholder:text-text-3 {refused
														? 'border-danger'
														: 'border-border-strong hover:border-text-3'}"
												/>
												<AppButton
													variant="quiet"
													disabled={approvals.deciding !== null}
													onclick={() => reject(item.id)}>Reject with this note</AppButton
												>
											</div>
										</div>
									</div>
								</section>
							{/if}
						</article>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</main>

<!-- By when a held answer is decided (or, for the person's own question, how long it waits), and how
     long is left: a chip that turns to a warning as the time runs short. -->
{#snippet deadline(item: Held)}
	{@const left = timeLeft(item.expires_at, now)}
	<span class="ml-auto flex items-center gap-2 text-xs text-text-2 tabular-nums">
		<span class="flex items-center gap-1.5"
			><span aria-hidden="true" class="flex text-text-3"><Icon name="clock" size={13} /></span
			>{item.asked_by_me ? 'Waits until' : 'Decide by'}
			<span class="font-medium text-text">{describeMoment(item.expires_at)}</span></span
		>
		<Chip tone={left.tone}>{left.text}</Chip>
	</span>
{/snippet}

<!-- A run of the draft's text: bold where the Markdown says so, one mark per flagged stretch, drawn
     as a highlighter with a line under it, unbroken across a wrap. -->
{#snippet runs(
	parts: Inline<Flag>[]
)}{#each grouped(parts) as group, g (g)}{#if group.marks.length}<mark
				class="border-b-2 border-warning/70 bg-warning/15 box-decoration-clone px-0.5 text-text"
				title={group.marks.map((f) => `${f.check} (p ${probability(f.p)})`).join(', ')}
				>{#each group.parts as part, k (k)}{#if part.strong}<strong class="font-semibold"
							>{part.text}</strong
						>{:else}{part.text}{/if}{/each}</mark
			>{:else}{#each group.parts as part, k (k)}{#if part.strong}<strong class="font-semibold"
						>{part.text}</strong
					>{:else}{part.text}{/if}{/each}{/if}{/each}{/snippet}

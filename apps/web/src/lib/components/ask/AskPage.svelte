<script lang="ts">
	import { onMount, tick, untrack, type Snippet } from 'svelte';

	import { afterNavigate, replaceState } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { navigating, page } from '$app/state';
	import { mockSamples } from '$lib/api';
	import { questionIn } from '$lib/ask/address';
	import { conversation } from '$lib/ask/conversation.svelte';
	import { examplesFor, gridFor, type IconName } from '$lib/ask/examples';
	import {
		newConversation as startConversation,
		openQuestion,
		threads,
		turnOf
	} from '$lib/ask/history.svelte';
	import { projects } from '$lib/ask/projects.svelte';
	import { seesFullTrail, stagesInOneLine, summarizeStages } from '$lib/ask/timeline';
	import { probeNotice, signoffWords } from '$lib/ask/words';
	import AppButton from '$lib/components/AppButton.svelte';
	import AnswerCard from '$lib/components/ask/AnswerCard.svelte';
	import ChatSidebar from '$lib/components/ask/ChatSidebar.svelte';
	import Chip from '$lib/components/ask/Chip.svelte';
	import EarlierReply from '$lib/components/ask/EarlierReply.svelte';
	import Notice from '$lib/components/ask/Notice.svelte';
	import QuestionForm from '$lib/components/ask/QuestionForm.svelte';
	import RefusalNotice from '$lib/components/ask/RefusalNotice.svelte';
	import ShortView from '$lib/components/ask/ShortView.svelte';
	import Thinking from '$lib/components/ask/Thinking.svelte';
	import Timeline from '$lib/components/ask/Timeline.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import Logo from '$lib/components/Logo.svelte';
	import { describeAge, describeMoment } from '$lib/format';
	import { session } from '$lib/session.svelte';
	import { shell } from '$lib/shell.svelte';

	let draft = $state('');
	/** The project picked; '' lets Mercury work it out from the question. */
	let selected = $state('');
	let scroller = $state<HTMLElement | null>(null);

	const SAMPLE_ICONS: IconName[] = ['check', 'lock', 'chat', 'shield', 'sparkles'];

	/** Whether the person reads the whole trail under an answer, or the short view of it. */
	const fullTrail = $derived(seesFullTrail(session.me?.user.role));
	/** What to offer when nothing has been asked yet: the open tiles only where a project takes them. */
	const examples = $derived(examplesFor(projects.list ?? []));

	onMount(() => {
		void projects.load();
		// the conversations of this tab go in the side panel, under the main links
		shell.panel = conversationsPanel;
		return () => {
			shell.panel = null;
		};
	});

	// Whoever has only one project has nothing to pick.
	$effect(() => {
		const list = projects.list;
		if (list && list.length === 1 && selected === '') selected = list[0].project;
	});

	// The question the address names opens, in the conversation it was asked in when this tab still
	// has it, with its words: when the page opens (a reload, an address typed, a way back from another
	// screen), before the address is next looked at, and after every navigation that names one, a
	// click in the side panel among them. Every navigation counts, the one to the address the router
	// went to last too: the address is replaced as questions are asked, which the router's own
	// parameter never hears of, so a parameter that stayed the same can still be a conversation to
	// bring back.
	const named = questionIn(window.location.pathname);
	if (named) void openQuestion(conversation, named);
	/** Whether a navigation has finished since the page opened: until then the router can't replace the address. */
	let routed = $state(false);
	afterNavigate(({ to }) => {
		routed = true;
		const id = to ? questionIn(to.url.pathname) : null;
		if (id) void openQuestion(conversation, id);
	});

	// The question on screen is the one in the address, so a reload finds it again.
	$effect(() => {
		const id = conversation.questionId;
		// a navigation (the Ask link) puts the address back to /ask: run again to name the question
		void page.url.pathname;
		// not while the router starts, nor while a navigation on its way may be opening another question
		if (!routed || navigating.to) return;
		// the address as the browser has it: `page.url` and the route's parameter are those of the
		// last navigation, and don't follow a replaced state
		if (id === questionIn(window.location.pathname)) return;
		replaceState(id ? resolve('/ask/[id]', { id }) : resolve('/ask'), page.state);
	});

	// The conversation keeps each of its questions as it stands, so an earlier one stays on screen;
	// a question goes in once the core knows it (`turnOf`).
	$effect(() => {
		const turn = turnOf(conversation, notes);
		if (turn) untrack(() => threads.record(turn));
	});

	// The questions before the one on screen, in the conversation on screen.
	const earlier = $derived(
		(threads.current?.turns ?? []).filter((turn) => turn.id !== conversation.questionId)
	);

	// New events, and another conversation, keep the end of the conversation in view: a smooth
	// glide, or a jump for whoever asked the system for less motion (read at each scroll, so a
	// change to that setting counts at once).
	$effect(() => {
		void conversation.entries.length;
		void conversation.phase;
		void earlier.length;
		untrack(
			() =>
				void tick().then(() =>
					scroller?.scrollTo({
						top: scroller.scrollHeight,
						behavior: window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
							? 'auto'
							: 'smooth'
					})
				)
		);
	});

	// Whoever works the page from the keyboard has it follow them. Disabling the box when a question
	// is sent drops the focus, so it is put where the news will be: on the conversation while the
	// question runs, then on what came of it (the answer, the refusal, the question to answer). Only
	// for a question asked from this page, and only while the focus is nowhere or already in the
	// conversation: it never takes it from somewhere else.
	let followed = false;
	const ENDED = ['delivered', 'refused', 'failed', 'unavailable', 'expired'];
	const SETTLED = [...ENDED, 'clarify', 'pending_signoff', 'disconnected'];

	$effect(() => {
		const phase = conversation.phase;
		const unsent = phase === 'failed' && conversation.questionId === null;
		untrack(() => void follow(phase, unsent));
	});

	async function follow(phase: string, unsent: boolean) {
		if (!followed) return;
		await tick();
		const active = document.activeElement;
		const conversationBox = document.getElementById('conversation');
		const free =
			!active || active === document.body || (conversationBox?.contains(active) ?? false);
		if (!free) return;
		if (unsent) {
			// the question is still in the box
			document.getElementById('question')?.focus();
			followed = false;
		} else if (phase === 'sending' || phase === 'running') {
			conversationBox?.focus();
		} else if (SETTLED.includes(phase)) {
			document.querySelector<HTMLElement>('#conversation [data-focus-target]')?.focus();
			if (ENDED.includes(phase)) followed = false;
		}
	}

	async function ask() {
		followed = true;
		await conversation.ask(draft, selected === '' ? null : selected);
		// a question that could not be sent stays in the box to be sent again
		if (conversation.phase !== 'failed') draft = '';
		void projects.load();
	}

	/** Asks again a question an approver rejected: its words go back in the box, ready to send. */
	async function askAgain() {
		const words = conversation.askAgain();
		if (words) draft = words;
		await tick();
		document.getElementById('question')?.focus();
	}

	async function newConversation() {
		followed = false;
		startConversation(conversation);
		draft = '';
		// the box is ready for the next question
		await tick();
		document.getElementById('question')?.focus();
	}

	function suggest(text: string, project: string | null) {
		draft = text;
		selected = project ?? '';
		void ask();
	}

	function projectName(project: string | null): string | null {
		if (!project) return null;
		return projects.find(project)?.display_name ?? project;
	}

	const asked = $derived(projects.find(conversation.project?.project ?? (selected || null)));
	/** What was asked: this page's, or the conversation's for a question that came back by its address. */
	const question = $derived(
		conversation.text ||
			(threads.current?.turns.find((turn) => turn.id === conversation.questionId)?.text ?? '')
	);

	// What the person should know about the data an answer rests on, besides the answer itself; what
	// the checks kept back from it the answer card says (`answerCard`).
	const notes = $derived.by(() => {
		if (!asked) return [];
		const probe = asked.probe;
		const when = probe ? `${describeAge(probe.as_of)} (${describeMoment(probe.as_of)})` : '';
		const stale = probeNotice(probe, when);
		return stale ? [stale] : [];
	});

	const running = $derived(
		conversation.phase === 'sending' ||
			conversation.phase === 'running' ||
			conversation.phase === 'loading'
	);
	const ready = $derived(projects.list !== null && projects.list.length > 0);
	/** A new conversation before its first question: the start screen, with the box in its middle. */
	const start = $derived(conversation.phase === 'idle' && ready && earlier.length === 0);
	/** What the gateway did, under the reply it describes, once the question has stopped running. */
	const tray = $derived(conversation.entries.length > 0 && !running);
	/**
	 * A reply with its tray is one piece: the reply's foot is square, and the tray keeps to its edges
	 * with a square head and no edge of its own along the seam. Written out in full, so the
	 * stylesheet finds every class.
	 */
	const UNIT = [
		'[&>:first-child:not(:only-child)]:rounded-b-none',
		'[&>:last-child:not(:only-child)]:mx-0',
		'[&>:last-child:not(:only-child)]:rounded-t-none',
		'[&>:last-child:not(:only-child)]:border-t-0'
	].join(' ');

	/** A reply's tone in its frame: the edge along its top, the wash under its header, its mark. */
	type FrameTone = 'info' | 'warn' | 'bad';
	const FRAME: Record<FrameTone, { edge: string; wash: string; mark: string }> = {
		info: { edge: 'bg-info', wash: 'from-info/8', mark: 'bg-info/15 text-info ring-info/25' },
		warn: {
			edge: 'bg-warning',
			wash: 'from-warning/8',
			mark: 'bg-warning/15 text-warning ring-warning/25'
		},
		bad: {
			edge: 'bg-danger',
			wash: 'from-danger/8',
			mark: 'bg-danger/15 text-danger ring-danger/25'
		}
	};

	/**
	 * How the four stages went, in one line on the folded trail: the words the short view puts over
	 * its four, from the same rows (what stopped it, what needs a look, or that all passed).
	 */
	const checked = $derived(
		stagesInOneLine(summarizeStages(conversation.entries.map((entry) => entry.event)))
	);

	/** The person's initials, beside each of their questions. */
	const me = $derived(
		(session.me?.user.display_name ?? '')
			.split(/\s+/)
			.filter(Boolean)
			.slice(0, 2)
			.map((word) => word[0].toUpperCase())
			.join('')
	);
</script>

<svelte:head>
	<title>Ask · Mercury</title>
</svelte:head>

{#snippet conversationsPanel()}
	<ChatSidebar
		items={threads.items}
		current={threads.active}
		projects={projects.list ?? []}
		onnew={newConversation}
	/>
{/snippet}

{#snippet tile(label: string, text: string, icon: IconName, wide: boolean, onpick: () => void)}
	<!-- a way to start: the question itself first, then what kind of reply it shows; an arrow comes
	     up in the corner of the one under the pointer or the focus -->
	<button
		type="button"
		onclick={onpick}
		class="group relative flex min-h-24 flex-col justify-between gap-3 rounded-card border border-border bg-surface px-4 py-3.5 text-left shadow-card focus-ring transition-[border-color,box-shadow,translate] duration-200 hover:border-accent/50 hover:shadow-raised motion-safe:hover:-translate-y-0.5 {wide
			? 'sm:col-span-2'
			: ''}"
	>
		<span class="line-clamp-2 pr-6 text-sm leading-snug font-medium text-pretty text-text"
			>{text}</span
		>
		<span
			class="flex min-w-0 items-center gap-2 text-xs text-text-3 transition-colors group-hover:text-accent-text"
		>
			<span
				class="grid size-5.5 shrink-0 place-items-center rounded-chip bg-accent-soft text-accent-text"
				><Icon name={icon} size={13} /></span
			>
			<span class="truncate">{label}</span>
		</span>
		<span
			class="absolute top-3 right-3 rotate-45 text-accent-text opacity-0 transition-opacity duration-200 group-hover:opacity-100 group-focus-visible:opacity-100"
			><Icon name="send" size={15} weight={2.1} /></span
		>
	</button>
{/snippet}

{#snippet bubble(text: string, project: string | null)}
	<!-- the question, on the same edge as its answer: the person's initials, then their words -->
	<div class="flex items-start gap-3">
		<span
			aria-hidden="true"
			class="mt-1 grid size-7 shrink-0 place-items-center rounded-full bg-surface-3 text-2xs font-semibold text-text-2"
			>{me}</span
		>
		<div class="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
			<p
				class="max-w-measure rounded-card rounded-tl-chip bg-accent-soft px-3.5 py-1.5 text-sm leading-6 break-words whitespace-pre-line text-text"
			>
				{#if text}{text}{:else}<span class="text-text-2 italic"
						>A question asked before this page was opened</span
					>{/if}
			</p>
			{#if projectName(project)}
				<p class="text-xs text-text-3">About {projectName(project)}</p>
			{/if}
		</div>
	</div>
{/snippet}

{#snippet reply(body: Snippet)}
	<!-- Mercury's reply: its mark, then the answer, the notice or the loader -->
	<div class="flex items-start gap-3">
		<span class="mt-1.5 shrink-0"><Logo size={28} /></span>
		<div class="flex min-w-0 flex-1 flex-col">{@render body()}</div>
	</div>
{/snippet}

{#snippet frame(
	title: string,
	tone: FrameTone,
	icon: IconName,
	alert: boolean,
	body?: Snippet,
	foot?: Snippet
)}
	<!-- a reply that isn't an answer, in the answer's frame: its tone along the top edge, a header row
	     with its mark and its heading (where the focus goes), what to know, then a strip at the foot;
	     read out at once when something went wrong, quietly otherwise -->
	<section
		role={alert ? 'alert' : 'status'}
		class="relative overflow-hidden rounded-card border border-border bg-surface shadow-card"
	>
		<span aria-hidden="true" class="absolute inset-x-0 top-0 h-0.5 {FRAME[tone].edge}"></span>
		<div
			class="flex min-h-12 items-center gap-2.5 bg-linear-to-b to-transparent px-5 py-2.5 {FRAME[
				tone
			].wash} {body || foot ? 'border-b border-border-subtle' : ''}"
		>
			<span
				aria-hidden="true"
				class="grid size-6 shrink-0 place-items-center rounded-full ring-1 ring-inset {FRAME[tone]
					.mark}"><Icon name={icon} size={13} weight={2.5} /></span
			>
			<h2
				class="w-fit rounded-sm text-sm font-semibold text-text focus-ring"
				tabindex="-1"
				data-focus-target
			>
				{title}
			</h2>
		</div>
		{#if body}
			<div
				class="flex max-w-measure flex-col gap-2.5 px-5 pt-3.5 pb-4 text-sm leading-relaxed text-text-2"
			>
				{@render body()}
			</div>
		{/if}
		{#if foot}
			<div
				class="flex min-h-10 flex-wrap items-center gap-x-3 gap-y-2 border-t border-border-subtle bg-surface-2/60 px-5 py-2"
			>
				{@render foot()}
			</div>
		{/if}
	</section>
{/snippet}

<main
	id="main"
	tabindex="-1"
	class="flex h-[calc(100dvh-3.5rem)] flex-col focus:outline-none lg:h-dvh {start
		? 'overflow-y-auto'
		: ''}"
>
	<h1 class="sr-only">Ask</h1>

	<div
		bind:this={scroller}
		class="min-h-0 flex-1 overflow-y-auto px-4 sm:px-5 lg:px-6 {start ? 'hidden' : ''}"
	>
		<div class="flex min-h-full flex-col gap-10 py-6">
			{#if projects.failure && projects.list === null}
				<Notice title={projects.failure.message} tone="bad" alert>
					{#if projects.failure.hint}<p class="text-text-2">{projects.failure.hint}</p>{/if}
					<div>
						<AppButton variant="quiet" onclick={() => projects.load()}>Try again</AppButton>
					</div>
				</Notice>
			{:else if projects.list === null}
				<p class="text-sm text-text-2" role="status">Loading your projects…</p>
			{:else if projects.list.length === 0}
				<Notice title="You have no projects yet" tone="info">
					<p class="text-text-2">Ask your admin to give you access to a project, then come back.</p>
				</Notice>
			{/if}

			<!-- the conversation's earlier questions, as they ended -->
			{#each earlier as turn (turn.id)}
				<div class="flex flex-col gap-2.5">
					{@render bubble(turn.text, turn.project)}
					{#snippet earlierReply()}
						<EarlierReply {turn} />
					{/snippet}
					{@render reply(earlierReply)}
				</div>
			{/each}

			{#if conversation.phase === 'failed' && conversation.failure && conversation.questionId === null}
				<!-- the question never left: it is still in the box -->
				<Notice title={conversation.failure.message} tone="bad" alert>
					{#if conversation.failure.hint}<p class="text-text-2">{conversation.failure.hint}</p>{/if}
				</Notice>
			{:else if conversation.phase !== 'idle'}
				<section
					id="conversation"
					tabindex="-1"
					aria-label="This conversation"
					class="flex flex-col gap-2.5 focus:outline-none"
				>
					{@render bubble(question, conversation.project?.project ?? null)}
					{#snippet liveReply()}
						<!-- the reply and, once the question has stopped, what the gateway did with it: one
						     piece on the same edges, the tray tucked flush under the reply's square foot -->
						<div class="flex flex-col {tray ? UNIT : ''}">
							{#if running}
								<Thinking entries={conversation.entries} phase={conversation.phase} />
							{:else if conversation.phase === 'delivered' && conversation.answer}
								<AnswerCard
									answer={conversation.answer}
									done={conversation.done}
									{notes}
									withheld={conversation.withheld}
									leftOut={conversation.stripped}
									fromCode={conversation.fromCode}
								/>
							{:else if conversation.phase === 'refused' && conversation.refusal}
								<RefusalNotice
									refusal={conversation.refusal}
									onaskagain={conversation.canAskAgain ? askAgain : undefined}
									askingAgain={conversation.following !== null &&
										conversation.following === conversation.questionId}
								/>
							{:else if conversation.phase === 'clarify' && conversation.clarify}
								{@const clarify = conversation.clarify}
								{#snippet choices()}
									{#if conversation.failure}
										<p role="alert" class="text-danger">
											Your choice wasn't sent: {conversation.failure.message} Pick again to retry.
										</p>
									{/if}
									{#if conversation.canChoose}
										<!-- the choices; a project wears the mark of the box's project picker -->
										<div class="flex flex-wrap gap-2">
											{#each clarify.options as option (option.id)}
												<AppButton
													variant="quiet"
													onclick={() => {
														followed = true;
														void conversation.choose(option.id);
													}}
													>{#if clarify.kind === 'project'}<span class="text-text-3"
															><Icon name="folder" size={14} /></span
														>{/if}{option.label}</AppButton
												>
											{/each}
										</div>
									{:else}
										<p>
											This question was asked before the page was reloaded, so Mercury no longer has
											its words. Ask it again to choose.
										</p>
									{/if}
								{/snippet}
								{@render frame(clarify.question, 'info', 'chat', false, choices)}
							{:else if conversation.phase === 'pending_signoff'}
								{@const signoff = conversation.signoff}
								{#snippet why()}
									{#if signoff}<p class="text-title text-text">
											{signoffWords(signoff.reason)}
										</p>{/if}
								{/snippet}
								{#snippet wait()}
									<!-- the wait itself, in form: a slow beat beside the line that says what happens next -->
									<p class="flex items-center gap-2.5 text-xs text-text-2">
										<span
											aria-hidden="true"
											class="grid size-3 shrink-0 place-items-center rounded-full bg-info/20"
											><span class="size-1.5 rounded-full bg-info motion-safe:animate-pulse"
											></span></span
										>
										The answer appears here when they decide; this page checks every few seconds.
									</p>
								{/snippet}
								{@render frame(
									'Waiting for an approver',
									'info',
									'clock',
									false,
									signoff ? why : undefined,
									wait
								)}
							{:else if conversation.phase === 'disconnected'}
								{#snippet lost()}
									<p class="text-title text-text">
										The question is still running. Connect again to follow it.
									</p>
									<div>
										<AppButton
											onclick={() => {
												followed = true;
												void conversation.reconnect();
											}}>Connect again</AppButton
										>
									</div>
								{/snippet}
								{@render frame('The connection was lost', 'warn', 'exclamation', true, lost)}
							{:else if conversation.phase === 'failed' && conversation.failure}
								{@const failure = conversation.failure}
								{#snippet hint()}
									<p class="text-title text-text">{failure.hint}</p>
								{/snippet}
								{@render frame(
									failure.message,
									'bad',
									'close',
									true,
									failure.hint ? hint : undefined
								)}
							{:else if conversation.phase === 'unavailable'}
								{#snippet gone()}
									<p class="text-title text-text">
										It may belong to someone else, or it no longer exists. Start a new conversation
										to ask something.
									</p>
								{/snippet}
								{@render frame("That question isn't available", 'warn', 'exclamation', false, gone)}
							{:else if conversation.phase === 'expired'}
								{#snippet old()}
									<p class="text-title text-text">
										Mercury keeps what it answered for a limited time, and this question is older
										than that. Ask it again for an answer from today's data.
									</p>
								{/snippet}
								{@render frame('This answer is no longer kept', 'info', 'clock', false, old)}
							{/if}

							{#if tray}
								{#if fullTrail}
									<!-- how the gateway got there, folded away in the tray: the answer comes first, and
									     the line at the right says at a glance how its four stages went -->
									<details
										class="group/trail rounded-b-card border border-t-0 border-border bg-surface"
									>
										<summary
											class="flex min-h-10 cursor-pointer list-none items-center gap-2 rounded-b-card px-4 py-2 text-compact font-medium text-text-2 focus-ring transition-colors group-open/trail:rounded-b-none hover:bg-surface-2/60 hover:text-text [&::-webkit-details-marker]:hidden"
										>
											<span
												class="text-text-3 transition-transform group-open/trail:rotate-90 motion-reduce:transition-none"
												><Icon name="chevron" size={14} /></span
											>
											<span>How Mercury checked this</span>
											<span class="ml-auto"><Chip tone={checked.tone}>{checked.words}</Chip></span>
										</summary>
										<div class="rounded-b-card border-t border-border-subtle px-4 pt-2.5 pb-3">
											<Timeline entries={conversation.entries} running={false} />
										</div>
									</details>
								{:else}
									<!-- one row per stage, open, for everyone who doesn't read the whole trail -->
									<ShortView entries={conversation.entries} />
								{/if}
							{/if}
						</div>
					{/snippet}
					{@render reply(liveReply)}
				</section>
			{/if}
		</div>
	</div>

	{#if start}
		<!-- a new conversation, before its first question: what Mercury is for, the box to ask in, and
		     ways to start, held together a little above the middle; the first question sends the
		     greeting and the tiles away and the box down to the foot of the conversation -->
		<div
			class="mt-auto flex flex-col items-center gap-3 px-4 pt-10 pb-7 text-center sm:px-5 lg:px-6"
		>
			<Logo size={40} />
			<p class="text-2xl font-semibold tracking-tight text-balance text-text">
				What would you like to know?
			</p>
			<p class="max-w-2xl text-sm leading-6 text-balance text-text-2">
				Ask anything about your projects. Mercury answers from facts about the repository and, where
				a project allows it, from a reader of its code, and checks every answer before you see it.
			</p>
		</div>
	{/if}

	{#if ready}
		<div class="px-4 sm:px-5 lg:px-6 {start ? '' : 'pt-2 pb-4'}">
			{#if conversation.phase !== 'idle'}
				<!-- a narrow screen keeps the side panel shut: the way to start again stays at hand -->
				<div class="mb-2 flex justify-end lg:hidden">
					<AppButton variant="quiet" onclick={newConversation}>New conversation</AppButton>
				</div>
			{/if}
			<QuestionForm
				projects={projects.list ?? []}
				busy={conversation.busy}
				bind:text={draft}
				bind:project={selected}
				onask={ask}
			/>
		</div>
	{/if}

	{#if start}
		<div class="mb-auto px-4 pt-4 pb-[12vh] sm:px-5 lg:px-6">
			{#if mockSamples}
				{#await mockSamples then { questions }}
					{@const layout = gridFor(questions.length)}
					<div class="grid auto-rows-fr gap-3 sm:grid-cols-2 {layout.columns}">
						{#each questions as sample, i (sample.text)}
							{@render tile(
								sample.label,
								sample.text,
								SAMPLE_ICONS[i % SAMPLE_ICONS.length],
								i < layout.wide,
								() => suggest(sample.text, sample.project)
							)}
						{/each}
					</div>
				{/await}
			{:else}
				{@const layout = gridFor(examples.length)}
				<div class="grid auto-rows-fr gap-3 sm:grid-cols-2 {layout.columns}">
					{#each examples as example, i (example.text)}
						{@render tile(example.label, example.text, example.icon, i < layout.wide, () =>
							suggest(example.text, null)
						)}
					{/each}
				</div>
			{/if}
		</div>
	{/if}
</main>

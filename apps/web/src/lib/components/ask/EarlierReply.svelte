<script lang="ts">
	import type { Tone } from '$lib/ask/words';
	import type { Turn } from '$lib/ask/history.svelte';
	import Icon from '$lib/components/Icon.svelte';

	import AnswerCard from './AnswerCard.svelte';
	import RefusalNotice from './RefusalNotice.svelte';

	/** An earlier question of the conversation on screen: what came of it, as it ended. */
	let { turn }: { turn: Turn } = $props();

	/** How it stands, when it has no answer, refusal or question back to show. */
	const EARLIER: Record<string, string> = {
		clarify: 'Mercury asked you to choose.',
		pending_signoff: 'Waiting for an approver.',
		failed: 'This question failed.',
		unavailable: "This question isn't available.",
		expired: 'The answer is no longer kept.',
		disconnected: 'The connection was lost while it ran.',
		running: 'Still running when you moved on.',
		sending: 'Still running when you moved on.',
		loading: 'Still running when you moved on.'
	};

	type Mark = { tone: Tone; icon: 'chat' | 'clock' | 'close' | 'exclamation' };
	/** Its mark: what kind of ending it was, in the tone the live notice for it has. */
	const MARK: Record<string, Mark> = {
		clarify: { tone: 'info', icon: 'chat' },
		pending_signoff: { tone: 'info', icon: 'clock' },
		failed: { tone: 'bad', icon: 'close' },
		unavailable: { tone: 'warn', icon: 'exclamation' },
		disconnected: { tone: 'warn', icon: 'exclamation' }
	};
	const QUIET: Mark = { tone: 'neutral', icon: 'clock' };
	const DISC: Record<Tone, string> = {
		neutral: 'bg-surface-3 text-text-2',
		good: 'bg-accent-2/15 text-accent-2',
		warn: 'bg-warning/15 text-warning',
		bad: 'bg-danger/15 text-danger',
		info: 'bg-info/15 text-info'
	};
	const mark = $derived(MARK[turn.phase] ?? QUIET);
</script>

<!-- an ending with nothing more to show: a one-line reply, as wide as the others, its kind in its mark -->
{#snippet line(text: string)}
	<p
		class="flex min-h-10 items-center gap-2.5 rounded-card border border-border bg-surface px-4 py-2 text-sm text-text-2 shadow-card"
	>
		<span
			aria-hidden="true"
			class="grid size-5 shrink-0 place-items-center rounded-full {DISC[mark.tone]}"
			><Icon name={mark.icon} size={11} weight={2.5} /></span
		>{text}
	</p>
{/snippet}

{#if turn.phase === 'delivered' && turn.answer}
	<AnswerCard
		answer={turn.answer}
		done={turn.done}
		notes={turn.notes}
		withheld={turn.withheld}
		leftOut={turn.stripped}
		fromCode={turn.fromCode}
		live={false}
	/>
{:else if turn.phase === 'refused' && turn.refusal}
	<RefusalNotice refusal={turn.refusal} live={false} />
{:else if turn.phase === 'clarify' && turn.clarify}
	{@render line(`Mercury asked: ${turn.clarify.question}`)}
{:else if EARLIER[turn.phase]}
	{@render line(EARLIER[turn.phase])}
{/if}

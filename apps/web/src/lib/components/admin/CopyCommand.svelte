<script module lang="ts">
	/**
	 * Puts `text` on the clipboard: false when the page has none (plain http on another host) or the
	 * browser refused, so the person is shown how to copy it themselves.
	 */
	export async function toClipboard(
		text: string,
		clipboard: Pick<Clipboard, 'writeText'> | undefined
	): Promise<boolean> {
		if (!clipboard) return false;
		try {
			await clipboard.writeText(text);
			return true;
		} catch {
			return false;
		}
	}

	/** The keys that copy what is selected: ⌘C on a Mac, Ctrl+C elsewhere. */
	export function copyKeys(userAgent: string): string {
		return /Mac|iPhone|iPad|iPod/.test(userAgent) ? '⌘C' : 'Ctrl+C';
	}
</script>

<script lang="ts">
	import { onDestroy, tick } from 'svelte';

	import Icon from '$lib/components/Icon.svelte';

	// A command the admin types at the admin shell on the server, as text they can select, with a
	// button that copies it. The panel changes nothing itself: this is the way from what it shows to
	// the change.
	let { command, label }: { command: string; label?: string } = $props();

	/** What the last press came to: copied, or selected for the person to copy by hand. */
	let outcome = $state<'idle' | 'copied' | 'selected'>('idle');
	let keys = $state('Ctrl+C');
	let timer: ReturnType<typeof setTimeout> | undefined;
	let code = $state<HTMLElement>();

	function copied() {
		outcome = 'copied';
		clearTimeout(timer);
		timer = setTimeout(() => (outcome = 'idle'), 1600);
	}

	async function copy() {
		// started within the press itself, as some browsers allow a page to write the clipboard only then
		const written = toClipboard(command, navigator.clipboard);
		clearTimeout(timer);
		// emptied first, so a second press is said again
		outcome = 'idle';
		await tick();
		if (await written) {
			copied();
			return;
		}
		// no clipboard (plain http on another host, or refused): the command is selected instead
		const selection = window.getSelection();
		const range = document.createRange();
		if (code) range.selectNodeContents(code);
		selection?.removeAllRanges();
		selection?.addRange(range);
		keys = copyKeys(navigator.userAgent);
		outcome = 'selected';
	}

	// While the command waits to be copied by hand: the person's own copy of it is the "Copied", and
	// once the selection has left it the hint is no longer true.
	$effect(() => {
		if (outcome !== 'selected' || !code) return;
		const box = code;
		const held = () => {
			const selection = document.getSelection();
			return (
				!!selection &&
				!selection.isCollapsed &&
				box.contains(selection.anchorNode) &&
				box.contains(selection.focusNode)
			);
		};
		const onselectionchange = () => {
			if (!held()) outcome = 'idle';
		};
		const oncopy = () => {
			if (held()) copied();
		};
		document.addEventListener('selectionchange', onselectionchange);
		document.addEventListener('copy', oncopy);
		return () => {
			document.removeEventListener('selectionchange', onselectionchange);
			document.removeEventListener('copy', oncopy);
		};
	});

	onDestroy(() => clearTimeout(timer));

	// the words of the command, each kept whole; the space between them is where a line may break
	const words = $derived(command.split(' '));
	const gap = ' ';
</script>

<div class="flex min-w-0 flex-col">
	{#if label}<span class="pb-1.5 text-2xs font-medium text-text-3">{label}</span>{/if}
	<!-- a code chip: an inset with the admin shell's prompt in the accent, the command, and its copy
	     button -->
	<div
		class="group flex min-w-0 items-start gap-2 rounded-control border border-border bg-surface-2 py-1 pr-1 pl-2.5 transition-colors duration-150 hover:border-border-control"
	>
		<span
			class="shrink-0 py-0.5 font-mono text-2xs leading-4.5 font-semibold text-accent-text select-none"
			aria-hidden="true">mercury&gt;</span
		>
		<code bind:this={code} class="min-w-0 flex-1 py-0.5 font-mono text-2xs leading-4.5 text-text"
			><!-- a line breaks between the words of the command, never inside a flag or an address -->{#each words as word, i (i)}{#if i > 0}{gap}{/if}<span
					class="whitespace-nowrap">{word}</span
				>{/each}</code
		>
		<button
			type="button"
			onclick={copy}
			aria-label={outcome === 'copied' ? 'Copied' : `Copy: ${command}`}
			class="grid h-6 min-w-6 shrink-0 place-items-center gap-1 rounded-chip px-1.5 text-2xs font-medium focus-ring transition-colors duration-150 hover:bg-surface-3 hover:text-text {outcome ===
			'copied'
				? 'bg-surface-3 text-text'
				: 'text-text-3'}"
		>
			{#if outcome === 'copied'}<span class="flex items-center gap-1"
					><Icon name="check" size={13} />Copied</span
				>{:else}<Icon name="copy" size={14} />{/if}
		</button>
	</div>
	<!-- what a press came to, said to a screen reader: the region is on the page before it changes,
	     and takes no room while it says nothing -->
	<p role="status" aria-live="polite" class="text-2xs text-text-2">
		{#if outcome === 'selected'}<span class="block pt-1.5"
				>The browser didn't let this page copy. The command is selected: press <kbd
					class="rounded-chip border border-border bg-surface px-1.5 py-px font-mono text-text"
					>{keys}</kbd
				> to copy it.</span
			>{:else if outcome === 'copied'}<span class="sr-only">Copied</span>{/if}
	</p>
</div>

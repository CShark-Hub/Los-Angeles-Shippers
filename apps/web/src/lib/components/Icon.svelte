<script lang="ts">
	// A few line icons drawn for the app, in the text's colour; decoration only, so hidden from readers.
	type Name =
		| 'sparkles'
		| 'shield'
		| 'audit'
		| 'key'
		| 'lock'
		| 'send'
		| 'plus'
		| 'clock'
		| 'chat'
		| 'stop'
		| 'chevron'
		| 'settings'
		| 'menu'
		| 'close'
		| 'logout'
		| 'user'
		| 'folder'
		| 'sun'
		| 'moon'
		| 'monitor'
		| 'flag'
		| 'check'
		| 'chart'
		| 'users'
		| 'repo'
		| 'copy'
		| 'refresh'
		| 'alert'
		| 'search'
		| 'terminal'
		| 'info'
		| 'chevron-down'
		| 'exclamation'
		| 'info-mark';
	// `weight` thickens the line for a small glyph on a filled disc.
	let { name, size = 18, weight = 1.9 }: { name: Name; size?: number; weight?: number } = $props();
</script>

<svg
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill="none"
	stroke="currentColor"
	stroke-width={weight}
	stroke-linecap="round"
	stroke-linejoin="round"
	aria-hidden="true"
	focusable="false"
>
	{#if name === 'sparkles'}
		<path d="M11 3.5l1.7 4.8 4.8 1.7-4.8 1.7L11 16.5l-1.7-4.8L4.5 10l4.8-1.7z" />
		<path d="M18 14.5l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8z" />
	{:else if name === 'shield'}
		<path d="M12 3l7 3v5.2c0 4.4-3 8.1-7 9.8-4-1.7-7-5.4-7-9.8V6z" />
		<path d="M9 12.2l2.1 2.1L15.2 10" />
	{:else if name === 'audit'}
		<path d="M7 3h8l3 3v15H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z" />
		<path d="M9 9.5h6M9 13h6M9 16.5h4" />
	{:else if name === 'key'}
		<circle cx="8" cy="15.5" r="3.8" />
		<path d="M10.8 12.8L19 4.5M15.5 8l2.3 2.3M13.3 10.2l1.8 1.8" />
	{:else if name === 'send'}
		<path d="M12 19V5M5.5 11.5L12 5l6.5 6.5" />
	{:else if name === 'plus'}
		<path d="M12 5v14M5 12h14" />
	{:else if name === 'clock'}
		<circle cx="12" cy="12" r="8.5" />
		<path d="M12 7.5V12l3 2" />
	{:else if name === 'chat'}
		<path
			d="M5 6.5A2.5 2.5 0 0 1 7.5 4h9A2.5 2.5 0 0 1 19 6.5v7a2.5 2.5 0 0 1-2.5 2.5H11l-4 3.5V16h0a2 2 0 0 1-2-2z"
		/>
	{:else if name === 'stop'}
		<circle cx="12" cy="12" r="8.5" />
		<path d="M9 9l6 6M15 9l-6 6" />
	{:else if name === 'chevron'}
		<path d="M9 6l6 6-6 6" />
	{:else if name === 'settings'}
		<circle cx="12" cy="12" r="3" />
		<path
			d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"
		/>
	{:else if name === 'menu'}
		<path d="M4 7h16M4 12h16M4 17h16" />
	{:else if name === 'close'}
		<path d="M6 6l12 12M18 6L6 18" />
	{:else if name === 'user'}
		<circle cx="12" cy="8.5" r="3.5" />
		<path d="M5 20c.8-3.6 3.6-5.5 7-5.5s6.2 1.9 7 5.5" />
	{:else if name === 'folder'}
		<path d="M3.5 7.5A2 2 0 0 1 5.5 5.5h4l2 2h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z" />
	{:else if name === 'sun'}
		<circle cx="12" cy="12" r="3.8" />
		<path
			d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4L7 17M17 7l1.4-1.4"
		/>
	{:else if name === 'moon'}
		<path d="M19.5 14.5A7.5 7.5 0 0 1 9.5 4.5a7.5 7.5 0 1 0 10 10z" />
	{:else if name === 'monitor'}
		<rect x="3.5" y="4.5" width="17" height="11.5" rx="2" />
		<path d="M9 20h6M12 16v4" />
	{:else if name === 'flag'}
		<path d="M5.5 21V4.5M5.5 4.5h11l-2 4 2 4h-11" />
	{:else if name === 'check'}
		<path d="M5 12.5l4.5 4.5L19 7.5" />
	{:else if name === 'chart'}
		<path d="M4 20h16" />
		<path d="M7 16.5v-5M12 16.5V6.5M17 16.5v-8" />
	{:else if name === 'users'}
		<circle cx="9" cy="8.5" r="3.2" />
		<path d="M3.5 19.5c.7-3.2 2.9-5 5.5-5s4.8 1.8 5.5 5" />
		<path d="M15.5 5.6a3.2 3.2 0 0 1 0 5.8M17.5 14.8c1.6.6 2.6 2.2 3 4.7" />
	{:else if name === 'repo'}
		<path d="M6 4h11a1 1 0 0 1 1 1v12H7a2 2 0 0 0-2 2V5a1 1 0 0 1 1-1z" />
		<path d="M5 19a2 2 0 0 0 2 2h11v-4" />
		<path d="M9 8h5" />
	{:else if name === 'copy'}
		<rect x="8.5" y="8.5" width="11" height="11" rx="2" />
		<path d="M15.5 8.5V6a1.5 1.5 0 0 0-1.5-1.5H6A1.5 1.5 0 0 0 4.5 6v8A1.5 1.5 0 0 0 6 15.5h2.5" />
	{:else if name === 'refresh'}
		<path d="M19.5 12a7.5 7.5 0 0 1-13.1 5" />
		<path d="M4.5 12a7.5 7.5 0 0 1 13.1-5" />
		<path d="M17.8 3.5v3.7h-3.7M6.2 20.5v-3.7h3.7" />
	{:else if name === 'alert'}
		<path d="M12 4l9 15.5H3z" />
		<path d="M12 10v4.2M12 17.2v.1" />
	{:else if name === 'search'}
		<circle cx="11" cy="11" r="6.5" />
		<path d="M20 20l-4.3-4.3" />
	{:else if name === 'terminal'}
		<rect x="3.5" y="4.5" width="17" height="15" rx="2" />
		<path d="M7.5 9.5l3 2.5-3 2.5M12.5 15h4" />
	{:else if name === 'info'}
		<circle cx="12" cy="12" r="8.5" />
		<path d="M12 11v5M12 8v.1" />
	{:else if name === 'chevron-down'}
		<path d="M6 9l6 6 6-6" />
	{:else if name === 'exclamation'}
		<path d="M12 6v8M12 18v.1" />
	{:else if name === 'info-mark'}
		<path d="M12 11v7M12 6.5v.1" />
	{:else if name === 'logout'}
		<path d="M14 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3" />
		<path d="M10 16l-4-4 4-4M6 12h9" />
	{:else}
		<rect x="5" y="10.5" width="14" height="10" rx="2" />
		<path d="M8.5 10.5V7.5a3.5 3.5 0 0 1 7 0v3" />
	{/if}
</svg>

<script lang="ts">
	// Mercury's mark: a planet with its ring on the violet square. Its own colours, in every look.
	let { size = 28 }: { size?: number } = $props();
	const id = $props.id();
</script>

<svg width={size} height={size} viewBox="0 0 1024 1024" aria-hidden="true" focusable="false">
	<defs>
		<linearGradient id="{id}-bg" x1="0" y1="0" x2="1" y2="1">
			<stop offset="0" stop-color="#7C74F0" />
			<stop offset="0.55" stop-color="#5145D6" />
			<stop offset="1" stop-color="#2B2A8F" />
		</linearGradient>
		<radialGradient id="{id}-orb" cx="0.36" cy="0.32" r="0.8">
			<stop offset="0" stop-color="#FFFFFF" />
			<stop offset="0.55" stop-color="#E4E6FB" />
			<stop offset="1" stop-color="#A9AEEA" />
		</radialGradient>
		<clipPath id="{id}-front"><rect x="0" y="512" width="1024" height="512" /></clipPath>
	</defs>
	<rect x="48" y="48" width="928" height="928" rx="224" fill="url(#{id}-bg)" />
	<g transform="rotate(-22 512 512)">
		<ellipse
			cx="512"
			cy="512"
			rx="352"
			ry="112"
			fill="none"
			stroke="#FFFFFF"
			stroke-opacity="0.45"
			stroke-width="40"
		/>
	</g>
	<circle cx="512" cy="512" r="212" fill="url(#{id}-orb)" />
	<g transform="rotate(-22 512 512)" clip-path="url(#{id}-front)">
		<ellipse cx="512" cy="512" rx="352" ry="112" fill="none" stroke="#FFFFFF" stroke-width="40" />
	</g>
	<circle cx="812" cy="330" r="38" fill="#9EF0D4" />
</svg>

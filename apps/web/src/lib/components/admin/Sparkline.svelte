<script lang="ts">
	// A tile's small trend: the totals of the window's buckets as a thin line over a wash that fades to
	// the baseline, the last bucket marked. Decoration beside a number the tile states, so hidden from
	// readers. It fills the width it is given.
	let { values, height = 28 }: { values: readonly number[]; height?: number } = $props();

	let w = $state(96);
	const uid = $props.id();
	const wash = `spark-${uid}`;

	const path = $derived.by(() => {
		if (values.length < 2 || w < 8) return null;
		const max = Math.max(...values, 1);
		const step = (w - 6) / (values.length - 1);
		const points = values.map(
			(v, i) => [3 + i * step, height - 3 - (v / max) * (height - 8)] as const
		);
		const line = points
			.map(([x, y], i) => `${i === 0 ? 'M' : 'L'}${x.toFixed(1)},${y.toFixed(1)}`)
			.join(' ');
		const [lastX, lastY] = points[points.length - 1];
		return {
			line,
			area: `${line} L${lastX.toFixed(1)},${height} L3,${height} Z`,
			last: { x: lastX, y: lastY }
		};
	});
</script>

<div class="w-full min-w-0" style="height: {height}px" bind:clientWidth={w} aria-hidden="true">
	{#if path}
		<svg width={w} {height} viewBox="0 0 {w} {height}" class="block overflow-visible">
			<defs>
				<linearGradient id={wash} x1="0" x2="0" y1="0" y2="1">
					<stop offset="0" stop-color="var(--series-1)" stop-opacity="0.22" />
					<stop offset="1" stop-color="var(--series-1)" stop-opacity="0" />
				</linearGradient>
			</defs>
			<path d={path.area} fill="url(#{wash})" />
			<path
				d={path.line}
				fill="none"
				stroke="var(--series-1)"
				stroke-width="1.6"
				stroke-linejoin="round"
				stroke-linecap="round"
			/>
			<circle
				cx={path.last.x}
				cy={path.last.y}
				r="2.75"
				fill="var(--series-1)"
				stroke="var(--surface)"
				stroke-width="1.75"
			/>
		</svg>
	{/if}
</div>

<script lang="ts">
	import type { Bucket, SeriesPoint } from '$lib/api';
	import { bucketLabel, bucketTitle, labelled, peak, ticks, total } from '$lib/admin/stats';
	import { OUTCOME_COLOR, OUTCOME_WORDS, OUTCOMES, type Outcome } from '$lib/admin/words';

	// The window's questions in columns, one per bucket, each stacked by what became of its questions
	// from the baseline up: answered, degraded, held, refused, other. A legend names the colours; the
	// pointer or the arrow keys read a column out (the others step back while one is read); the table
	// shows every number without either. With `fill` the plot grows to the height its card is given
	// (the card beside it may be taller), `height` being the least it takes.
	let {
		series,
		bucket,
		height: tall = 196,
		fill = false
	}: { series: readonly SeriesPoint[]; bucket: Bucket; height?: number; fill?: boolean } = $props();

	let width = $state(640);
	let room = $state(0);
	// a phone gets a lower chart: the same columns, less scrolling past them
	const least = $derived(width < 520 ? Math.min(tall, 220) : tall);
	const height = $derived(fill ? Math.max(least, room) : least);
	let active = $state<number | null>(null);
	let tabular = $state(false);

	const margin = { top: 12, right: 4, bottom: 26, left: 34 };
	const plotWidth = $derived(Math.max(10, width - margin.left - margin.right));
	const plotHeight = $derived(height - margin.top - margin.bottom);
	const axis = $derived(ticks(peak(series)));
	const top = $derived(axis[axis.length - 1] ?? 1);
	const band = $derived(plotWidth / Math.max(1, series.length));
	const barWidth = $derived(Math.max(3, Math.min(40, band * 0.56)));
	const showLabel = $derived(labelled(series.length));
	const y = (value: number) => margin.top + plotHeight - (value / top) * plotHeight;

	/** A column's segments, bottom up, each with a 2px gap of the card's colour above the one below. */
	function segments(point: SeriesPoint) {
		const out: { outcome: Outcome; y: number; h: number; top: boolean }[] = [];
		let base = margin.top + plotHeight;
		const present = OUTCOMES.filter((o) => point.counts[o] > 0);
		present.forEach((outcome, i) => {
			const h = Math.max(1, (point.counts[outcome] / top) * plotHeight - (i > 0 ? 2 : 0));
			const yTop = base - h;
			out.push({ outcome, y: yTop, h, top: i === present.length - 1 });
			base = yTop - 2;
		});
		return out;
	}

	/** A rectangle with its top corners rounded: a column's data end, square at the baseline. */
	function roundedTop(x: number, yTop: number, w: number, h: number): string {
		const r = Math.min(4, w / 4, h);
		return `M${x},${yTop + h} V${yTop + r} Q${x},${yTop} ${x + r},${yTop} H${x + w - r} Q${x + w},${yTop} ${x + w},${yTop + r} V${yTop + h} Z`;
	}

	const point = $derived(active === null ? null : (series[active] ?? null));
	const tipWidth = 208;
	const tipLeft = $derived(active === null ? 0 : margin.left + band * active + band / 2);
	const flip = $derived(tipLeft > width * 0.62);

	function keydown(event: KeyboardEvent) {
		if (series.length === 0) return;
		const current = active ?? series.length - 1;
		if (event.key === 'ArrowLeft') active = Math.max(0, current - 1);
		else if (event.key === 'ArrowRight') active = Math.min(series.length - 1, current + 1);
		else if (event.key === 'Home') active = 0;
		else if (event.key === 'End') active = series.length - 1;
		else if (event.key === 'Escape') active = null;
		else return;
		event.preventDefault();
	}

	const totals = $derived(
		Object.fromEntries(
			OUTCOMES.map((o) => [o, series.reduce((sum, p) => sum + p.counts[o], 0)])
		) as Record<Outcome, number>
	);
</script>

<div class="flex flex-col gap-3 {fill ? 'min-h-0 flex-1' : ''}">
	<div class="flex flex-wrap items-center justify-between gap-2">
		<ul class="flex flex-wrap items-center gap-x-4 gap-y-1.5" aria-label="Legend">
			{#each OUTCOMES as outcome (outcome)}
				<li
					class="flex items-center gap-1.5 text-xs/normal text-text-2"
					title={OUTCOME_WORDS[outcome].meaning}
				>
					<span
						class="size-2.5 shrink-0 rounded-xs"
						style="background: {OUTCOME_COLOR[outcome]}"
						aria-hidden="true"
					></span>
					{OUTCOME_WORDS[outcome].label}
					<span class="font-semibold text-text tabular-nums">{totals[outcome]}</span>
				</li>
			{/each}
		</ul>
		<button
			type="button"
			aria-pressed={tabular}
			onclick={() => (tabular = !tabular)}
			class="inline-flex h-7 items-center rounded-control border border-border-control bg-surface px-2.5 text-xs font-medium text-text-2 shadow-control focus-ring transition-colors duration-150 hover:border-border-strong hover:bg-surface-2 hover:text-text aria-pressed:border-transparent aria-pressed:bg-accent-soft aria-pressed:text-accent-text aria-pressed:shadow-none"
			>{tabular ? 'Chart' : 'Table'}</button
		>
	</div>

	{#if tabular}
		<div
			class="overflow-auto rounded-control border border-border-subtle"
			style="max-height: {height}px"
		>
			<table class="w-full text-left text-xs/normal">
				<caption class="sr-only"
					>Questions per {bucket === 'hour' ? 'hour' : 'day'}, by outcome</caption
				>
				<thead class="sticky top-0 bg-surface-2 text-2xs text-text-2">
					<tr class="border-b border-border-subtle">
						<th scope="col" class="px-3 py-2 font-medium">{bucket === 'hour' ? 'Hour' : 'Day'}</th>
						{#each OUTCOMES as outcome (outcome)}
							<th scope="col" class="px-3 py-2 text-right font-medium">
								<span class="inline-flex items-center gap-1.5"
									><span
										class="size-2 shrink-0 rounded-xs"
										style="background: {OUTCOME_COLOR[outcome]}"
										aria-hidden="true"
									></span>{OUTCOME_WORDS[outcome].label}</span
								>
							</th>
						{/each}
						<th scope="col" class="px-3 py-2 text-right font-medium">Total</th>
					</tr>
				</thead>
				<tbody class="divide-y divide-border-subtle tabular-nums">
					{#each series as p (p.start)}
						<tr class="transition-colors hover:bg-surface-2/60">
							<th scope="row" class="px-3 py-1.5 font-normal whitespace-nowrap text-text-2"
								>{bucketTitle(p.start, bucket)}</th
							>
							{#each OUTCOMES as outcome (outcome)}
								<td
									class="px-3 py-1.5 text-right {p.counts[outcome] === 0
										? 'text-text-3'
										: 'text-text'}">{p.counts[outcome]}</td
								>
							{/each}
							<td class="px-3 py-1.5 text-right font-semibold text-text">{total(p.counts)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{:else}
		<!-- a focusable group the arrow keys read column by column -->
		<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
		<!-- filling, the plot is laid over a box the card sizes, so the drawing never sizes the box -->
		<div
			class="relative rounded-control focus-ring {fill ? 'flex-1' : ''}"
			style={fill ? `min-height: ${least}px` : undefined}
			bind:clientWidth={width}
			bind:clientHeight={room}
			role="group"
			tabindex="0"
			aria-label="Questions per {bucket === 'hour'
				? 'hour'
				: 'day'}, by outcome. The arrow keys read each column; the Table button shows every number."
			onkeydown={keydown}
			onfocus={() => (active ??= series.length - 1)}
			onblur={() => (active = null)}
			onpointerleave={() => (active = null)}
		>
			<svg
				{width}
				{height}
				viewBox="0 0 {width} {height}"
				aria-hidden="true"
				class="block text-2xs tabular-nums {fill ? 'absolute inset-0' : ''}"
			>
				{#each axis as tick (tick)}
					<line
						x1={margin.left}
						x2={width - margin.right}
						y1={y(tick)}
						y2={y(tick)}
						stroke={tick === 0 ? 'var(--border-control)' : 'var(--grid)'}
						stroke-width="1"
						stroke-dasharray={tick === 0 ? undefined : '3 4'}
						shape-rendering="crispEdges"
					/>
					<text x={margin.left - 10} y={y(tick)} dy="0.32em" text-anchor="end" fill="var(--text-3)"
						>{tick}</text
					>
				{/each}

				{#each series as p, i (p.start)}
					{@const x = margin.left + band * i + (band - barWidth) / 2}
					{#if active === i}
						<rect
							x={margin.left + band * i + 2}
							y={margin.top - 4}
							width={Math.max(1, band - 4)}
							height={plotHeight + 4}
							rx="6"
							fill="var(--surface-2)"
						/>
					{/if}
					<g
						opacity={active === null || active === i ? 1 : 0.42}
						class="motion-safe:transition-opacity motion-safe:duration-150"
					>
						{#each segments(p) as s (s.outcome)}
							{#if s.top}
								<path d={roundedTop(x, s.y, barWidth, s.h)} fill={OUTCOME_COLOR[s.outcome]} />
							{:else}
								<rect {x} y={s.y} width={barWidth} height={s.h} fill={OUTCOME_COLOR[s.outcome]} />
							{/if}
						{/each}
					</g>
					{#if showLabel(i)}
						<text
							x={margin.left + band * i + band / 2}
							y={height - 7}
							text-anchor="middle"
							fill={active === i ? 'var(--text)' : 'var(--text-3)'}
							font-weight={active === i ? 600 : 400}>{bucketLabel(p.start, bucket)}</text
						>
					{/if}
					<!-- the column's hit area: its whole band, taller than the marks -->
					<rect
						x={margin.left + band * i}
						y={margin.top}
						width={band}
						height={plotHeight + 6}
						fill="transparent"
						role="presentation"
						onpointerenter={() => (active = i)}
						onpointermove={() => (active = i)}
					/>
				{/each}
			</svg>

			{#if point}
				<div
					class="pointer-events-none absolute top-1 z-10 rounded-control border border-border bg-surface-raised px-3 py-2.5 shadow-raised"
					style="left: {flip ? tipLeft - tipWidth - 14 : tipLeft + 14}px; width: {tipWidth}px"
					role="status"
				>
					<p class="text-2xs font-medium text-text-3">{bucketTitle(point.start, bucket)}</p>
					<p class="text-title font-semibold text-text tabular-nums">
						{total(point.counts)} question{total(point.counts) === 1 ? '' : 's'}
					</p>
					<ul class="mt-1.5 flex flex-col gap-1 border-t border-border-subtle pt-2">
						{#each [...OUTCOMES].reverse() as outcome (outcome)}
							<li
								class="flex items-center gap-2 text-xs/normal {point.counts[outcome] === 0
									? 'text-text-3'
									: 'text-text-2'}"
							>
								<span
									class="size-2 shrink-0 rounded-xs"
									style="background: {OUTCOME_COLOR[outcome]}"
									aria-hidden="true"
								></span>
								<span class="flex-1">{OUTCOME_WORDS[outcome].label}</span>
								<span
									class="text-right font-semibold tabular-nums {point.counts[outcome] === 0
										? 'text-text-3'
										: 'text-text'}">{point.counts[outcome]}</span
								>
							</li>
						{/each}
					</ul>
				</div>
			{/if}
		</div>
	{/if}
</div>

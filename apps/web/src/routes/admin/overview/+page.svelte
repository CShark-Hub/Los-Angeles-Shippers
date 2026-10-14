<script lang="ts">
	import { AdminRead, overviewPath, WINDOWS } from '$lib/admin/admin.svelte';
	import { commands } from '$lib/admin/commands';
	import { compact, delivered, percentOf, total, trend } from '$lib/admin/stats';
	import {
		alertWords,
		allClearWords,
		channelLabel,
		classLabel,
		judgeLabel,
		OUTCOME_COLOR,
		OUTCOME_WORDS,
		OUTCOMES,
		refusalLabel
	} from '$lib/admin/words';
	import type { AdminOverview, CheckLine, OverviewWindow } from '$lib/api';
	import { DOT, type Tone } from '$lib/ask/words';
	import AppButton from '$lib/components/AppButton.svelte';
	import Chip from '$lib/components/ask/Chip.svelte';
	import Notice from '$lib/components/ask/Notice.svelte';
	import BudgetMeter from '$lib/components/admin/BudgetMeter.svelte';
	import Card from '$lib/components/admin/Card.svelte';
	import CopyCommand from '$lib/components/admin/CopyCommand.svelte';
	import OutcomeChart from '$lib/components/admin/OutcomeChart.svelte';
	import PageFrame from '$lib/components/admin/PageFrame.svelte';
	import RankList from '$lib/components/admin/RankList.svelte';
	import RunHint from '$lib/components/admin/RunHint.svelte';
	import Segmented from '$lib/components/admin/Segmented.svelte';
	import StatTile from '$lib/components/admin/StatTile.svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { describeCost, describeDuration, describeMoment } from '$lib/format';
	import { session } from '$lib/session.svelte';

	const read = new AdminRead<AdminOverview>();
	const admin = $derived(session.me?.user.role === 'admin');
	let range = $state<OverviewWindow>('7d');
	const windowWords = $derived(WINDOWS.find((w) => w.value === range)?.long ?? '');

	// one read when the screen opens and one per window picked: each read is an audit row
	$effect(() => {
		if (admin) void read.load(overviewPath(range));
	});

	const o = $derived(read.data);
	const all = $derived(o ? total(o.totals) : 0);
	const reached = $derived(o ? delivered(o.totals) : 0);
	const todayShare = $derived(
		o?.spend.daily_budget_usd ? o.spend.everyone_usd / o.spend.daily_budget_usd : 0
	);
	const endings = $derived(o ? o.judge.jev + o.judge.chat + o.judge.rules + o.judge.none : 0);
	const signoffs = $derived<{ label: string; value: number; tone: Tone }[]>(
		o
			? [
					{
						label: 'Waiting now',
						value: o.signoffs.pending,
						tone: o.signoffs.pending > 0 ? 'warn' : 'neutral'
					},
					{ label: 'Approved', value: o.signoffs.approved, tone: 'neutral' },
					{ label: 'Rejected', value: o.signoffs.rejected, tone: 'neutral' },
					{
						label: 'Expired after 24 h',
						value: o.signoffs.expired,
						tone: o.signoffs.expired > 0 ? 'bad' : 'neutral'
					}
				]
			: []
	);
	// every answer held in the window is in one of the four, so they share one scale
	const held = $derived(signoffs.reduce((sum, item) => sum + item.value, 0));

	const stages = ['input', 'tool', 'data', 'output'] as const;
	// the reference in two columns, in the order a question meets the checks: what is asked and the
	// calls planned for it, then what came back and what is said
	const stageColumns = [stages.slice(0, 2), stages.slice(2)] as const;
	const stageWords = {
		input: 'The question',
		tool: 'The planned tool calls',
		data: 'What a probe returned',
		output: 'Each sentence of the answer'
	} as const;
	// the thresholds a check may hold, one column each, marked in the colour of what they lead to
	const levels = [
		{ key: 'block_at', label: 'Block', tone: 'bad' },
		{ key: 'review_at', label: 'Hold', tone: 'warn' },
		{ key: 'pass_at', label: 'Pass', tone: 'good' },
		{ key: 'clarify_below', label: 'Ask back', tone: 'info' }
	] as const satisfies readonly { key: keyof CheckLine; label: string; tone: Tone }[];
	const budgets = $derived(
		o?.settings.budgets
			? [
					{ label: 'Per question', value: o.settings.budgets.per_question_usd },
					{ label: 'Per person a day', value: o.settings.budgets.per_user_daily_usd },
					{ label: 'Everyone a day', value: o.settings.budgets.global_daily_usd }
				]
			: []
	);
	// the models in two columns, filled down first, so the four roles read in their order
	const modelRows = $derived(Math.ceil((o?.settings.models?.length ?? 0) / 2));
	const roleWords = {
		judge: 'Judge',
		judge_fallback: 'Judge fallback',
		planner: 'Planner',
		composer: 'Composer'
	} as const;

	/** A check held by no threshold at all is decided by a rule. */
	function ruled(check: CheckLine): boolean {
		return levels.every((level) => check[level.key] === null);
	}
</script>

<svelte:head>
	<title>Overview · Admin · Mercury</title>
</svelte:head>

<main
	id="main"
	tabindex="-1"
	class="flex flex-col px-2 py-2.5 focus:outline-none sm:px-3 lg:min-h-dvh"
>
	<PageFrame
		title="Overview"
		description="What Mercury did in {windowWords}: every question and what became of it, what it cost, and what needs a look now."
		icon="chart"
	>
		{#snippet summary()}
			{#if read.readAt}<span
					class="inline-flex items-center gap-1.5 text-xs text-text-3 tabular-nums"
					><Icon name="clock" size={13} />Read {describeMoment(read.readAt)}</span
				>{/if}
		{/snippet}
		{#snippet actions()}
			<Segmented
				label="Window"
				options={WINDOWS}
				value={range}
				onchange={(value) => (range = value)}
			/>
			<AppButton
				variant="quiet"
				onclick={() => void read.load(overviewPath(range))}
				disabled={read.loading || !admin}
			>
				<span
					class="text-text-3 {read.loading
						? 'motion-safe:animate-spin motion-reduce:text-accent-text'
						: ''}"><Icon name="refresh" size={14} /></span
				>
				Refresh
			</AppButton>
		{/snippet}

		{#if !admin}
			<Notice title="Only an admin sees the admin panel" tone="info">
				<p class="text-text-2">Ask an admin about people, access or repositories.</p>
			</Notice>
		{:else}
			{#if read.failure}
				<Notice title={read.failure.message} tone="bad" alert>
					{#if read.failure.hint}<p class="text-text-2">{read.failure.hint}</p>{/if}
				</Notice>
			{/if}

			{#if o}
				<div
					class="flex flex-col gap-3 transition-opacity {read.loading ? 'opacity-60' : ''}"
					aria-busy={read.loading}
				>
					<!-- the window in six figures, as one band read from its lead figure; a figure that
					     needs a look is washed in its tone. The lead widens only where its neighbours'
					     lines still fit on one line each -->
					<section
						aria-label="Key figures"
						class="grid grid-cols-2 gap-px overflow-hidden rounded-card border border-border bg-border-subtle shadow-card sm:grid-cols-3 xl:grid-cols-6 min-[90rem]:grid-cols-[minmax(0,1.7fr)_repeat(5,minmax(0,1fr))]"
					>
						<StatTile
							primary
							label="Questions"
							value={compact(all)}
							detail="{o.askers} {o.askers === 1 ? 'person' : 'people'} asked"
							trend={trend(o.series)}
						/>
						<StatTile
							label="Delivered"
							value={percentOf(reached, all)}
							detail="{o.totals.answered} by Jev · {o.totals.degraded} degraded"
							tone={o.totals.degraded > 0 ? 'warn' : undefined}
						/>
						<StatTile
							label="Refused"
							value={compact(o.totals.refused)}
							detail="{percentOf(o.totals.refused, all)} of all, each with a hint"
						/>
						<StatTile
							label="Held for sign-off"
							value={compact(o.totals.held)}
							detail="{o.signoffs.pending} waiting now"
							tone={o.signoffs.pending > 0 ? 'warn' : undefined}
						/>
						<StatTile
							label="Typical answer time"
							value={o.duration_ms.p50 === null ? '–' : describeDuration(o.duration_ms.p50)}
							detail={o.duration_ms.p95 === null
								? 'Nothing finished yet'
								: `95% within ${describeDuration(o.duration_ms.p95)}`}
						/>
						<StatTile
							label="Cost"
							value={describeCost(o.cost_usd)}
							detail="Today {describeCost(o.spend.everyone_usd)}{o.spend.daily_budget_usd !== null
								? ` of ${describeCost(o.spend.daily_budget_usd)}`
								: ''}"
							tone={todayShare >= 1 ? 'bad' : todayShare >= 0.8 ? 'warn' : undefined}
						/>
					</section>

					<!-- the chart, and beside it the state of things now; the chart takes the height the
					     column beside it needs -->
					<div class="grid gap-3 lg:grid-cols-3">
						<Card
							class="lg:col-span-2"
							title="Questions over time"
							subtitle="Per {o.bucket === 'hour'
								? 'hour'
								: 'UTC day'}, by what became of them. Point at a column, or use the arrow keys."
						>
							<OutcomeChart series={o.series} bucket={o.bucket} height={264} fill />
						</Card>

						<div class="flex flex-col gap-3">
							<Card title="Needs attention" subtitle="The alerts the core checks every minute">
								{#snippet actions()}
									{#if o.alerts.length > 0}<Chip tone="bad"
											>{o.alerts.length} {o.alerts.length === 1 ? 'alert' : 'alerts'}</Chip
										>{/if}
								{/snippet}
								{#if o.alerts.length === 0}
									<div
										class="flex items-start gap-2.5 rounded-control border border-success/35 bg-success/6 px-3 py-2.5"
									>
										<span
											aria-hidden="true"
											class="mt-px grid size-5 shrink-0 place-items-center rounded-full bg-success text-surface"
											><Icon name="check" size={12} weight={2.75} /></span
										>
										<p class="text-xs leading-5 text-text-2">
											<span class="font-semibold text-text">All clear.</span>
											{allClearWords(o).join(' ')}
										</p>
									</div>
								{:else}
									<ul class="flex flex-col gap-2">
										{#each o.alerts as alert, i (i)}
											<li
												class="flex flex-col gap-2.5 rounded-control border border-danger/40 bg-danger/6 px-3 py-2.5"
											>
												<p class="flex items-start gap-2.5 text-xs leading-5 font-medium text-text">
													<span
														aria-hidden="true"
														class="mt-px grid size-5 shrink-0 place-items-center rounded-full bg-danger text-surface"
														><Icon name="exclamation" size={12} weight={2.75} /></span
													>
													{alertWords(alert, (iso) => describeMoment(iso))}
												</p>
												<CopyCommand
													command={alert.kind === 'probe_silent'
														? commands.probes()
														: commands.status()}
												/>
											</li>
										{/each}
									</ul>
								{/if}
							</Card>

							<Card
								class="flex-1"
								title="Spend today"
								subtitle="UTC day {o.spend.day}; the alert fires at 80% of the budget"
							>
								<div class="flex flex-col gap-3.5">
									<BudgetMeter
										primary
										label="Everyone"
										spent={o.spend.everyone_usd}
										cap={o.spend.daily_budget_usd}
									/>
									<div class="flex flex-col gap-2.5 border-t border-border-subtle pt-3">
										{#each o.spend.people.slice(0, 4) as spender (spender.user_id)}
											<BudgetMeter
												label={spender.display_name ?? spender.email ?? 'Someone no longer here'}
												spent={spender.spent_usd}
												cap={o.spend.per_user_daily_usd}
											/>
										{:else}
											<p class="text-xs/normal text-text-3">Nobody has spent anything today.</p>
										{/each}
									</div>
								</div>
							</Card>
						</div>
					</div>

					<!-- where the questions came from and why some were refused: the same rows throughout,
					     each pair of cards holding about as much as its neighbours -->
					<div
						class="grid gap-3 md:grid-cols-2 xl:grid-cols-[repeat(2,minmax(0,1fr))_minmax(0,2.2fr)]"
					>
						<Card title="By project" subtitle="Questions and what they cost">
							<div class="pb-3">
								<RankList
									rows={o.projects.map((row) => ({
										key: row.project ?? '',
										label: row.project ?? 'No project yet',
										count: total(row.counts),
										counts: row.counts,
										aside: describeCost(row.cost_usd),
										mono: row.project !== null,
										muted: row.project === null
									}))}
									total={all}
									empty="Nothing asked in this window."
								/>
							</div>
							{#if o.projects.some((p) => p.project === null)}
								<p
									class="mt-auto border-t border-border-subtle pt-2.5 text-2xs leading-4 text-text-3"
								>
									No project yet: refused or clarified before a project was picked.
								</p>
							{/if}
						</Card>

						<Card title="By channel" subtitle="Where the questions came from">
							<div class="pb-3">
								<RankList
									rows={o.channels.map((row) => ({
										key: row.channel,
										label: channelLabel(row.channel),
										count: total(row.counts),
										counts: row.counts
									}))}
									total={all}
								/>
							</div>
							<!-- the key to the split bars of this row, which the chart's legend is far from;
							     the bars say their own numbers to a reader -->
							<div
								class="mt-auto flex flex-wrap gap-x-3 gap-y-1 border-t border-border-subtle pt-2.5"
								aria-hidden="true"
							>
								{#each OUTCOMES as outcome (outcome)}
									<span class="inline-flex items-center gap-1.5 text-2xs leading-4 text-text-3"
										><span
											class="size-2 shrink-0 rounded-xs"
											style="background: {OUTCOME_COLOR[outcome]}"
										></span>{OUTCOME_WORDS[outcome].label}</span
									>
								{/each}
							</div>
						</Card>

						<Card
							class="md:col-span-2 xl:col-span-1"
							title="Refusals by reason"
							subtitle="Each came with a hint of what to ask instead"
						>
							<RankList
								rows={o.refusals.map((r) => ({
									key: r.code,
									label: refusalLabel(r.code),
									count: r.count
								}))}
								total={o.refusals.reduce((s, r) => s + r.count, 0)}
								color="var(--series-4)"
								empty="Nothing was refused in this window."
								wide
							/>
						</Card>
					</div>

					<!-- how the questions were judged, and the answers that waited for a person -->
					<div class="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
						<Card title="Kinds of question" subtitle="The class the judge gave each">
							<RankList
								rows={o.classes.map((c) => ({
									key: c.class,
									label: classLabel(c.class),
									count: c.count
								}))}
								total={o.classes.reduce((s, c) => s + c.count, 0)}
							/>
						</Card>

						<Card title="Judge" subtitle="Which judge answered the questions that ended">
							<div class="pb-3">
								<RankList
									rows={(['jev', 'chat', 'rules', 'none'] as const).map((mode) => ({
										key: mode,
										label: judgeLabel(mode).label,
										count: o.judge[mode],
										tone: judgeLabel(mode).tone
									}))}
									total={endings}
									max={endings}
								/>
							</div>
							<p
								class="mt-auto border-t border-border-subtle pt-2.5 text-2xs leading-4 text-text-3"
							>
								On a fallback every threshold moves 0.2 toward refusing, so more is refused or held.
							</p>
						</Card>

						<Card
							class="md:col-span-2 xl:col-span-1"
							title="Sign-offs"
							subtitle="Answers that waited for an approver"
						>
							{#snippet actions()}
								{#if o.signoffs.pending > 0}<Chip tone="warn">{o.signoffs.pending} waiting</Chip
									>{/if}
							{/snippet}
							<!-- the same rows as its neighbours, on one scale: every answer held in the window -->
							<RankList
								rows={signoffs.map((item) => ({
									key: item.label,
									label: item.label,
									count: item.value,
									tone: item.tone === 'neutral' ? undefined : item.tone
								}))}
								total={held}
								max={held}
								wide
							/>
						</Card>
					</div>

					<!-- what the server is configured with: a reference, changed by pull request -->
					<Card
						title="Models and budgets"
						subtitle="From models.yaml on the server: changed by pull request"
					>
						<!-- two references side by side: each model and what it is for, and the caps on spend -->
						<div class="grid gap-x-8 gap-y-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,16rem)]">
							{#if o.settings.models === null}
								<p class="text-xs/normal text-text-3">The core can't read models.yaml.</p>
							{:else}
								<dl
									class="grid gap-x-8 sm:grid-flow-col sm:grid-cols-2 sm:grid-rows-(--rows)"
									style="--rows: repeat({modelRows}, auto)"
								>
									{#each o.settings.models as model, i (model.role)}
										<!-- the role and where it runs on one line, the model's name under them -->
										<div
											class="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-baseline gap-x-3 gap-y-0.5 border-border-subtle py-2.5 {i ===
											0
												? 'pt-0'
												: 'border-t'} {i === modelRows ? 'sm:border-t-0 sm:pt-0' : ''} {i ===
												modelRows - 1 || i === o.settings.models.length - 1
												? 'sm:pb-0'
												: ''} last:pb-0"
										>
											<dt class="text-xs/normal font-medium text-text-2">
												{roleWords[model.role]}
											</dt>
											<dd
												class="col-span-2 row-start-2 truncate font-mono text-xs/normal text-text"
												title={model.model}
											>
												{model.model}
											</dd>
											<dd class="col-start-2 row-start-1 text-2xs whitespace-nowrap text-text-3">
												{model.backend === 'openrouter' ? 'OpenRouter' : 'Own server'}{model.kind
													? ` · ${model.kind}`
													: ''}
											</dd>
										</div>
									{/each}
								</dl>
							{/if}
							{#if o.settings.budgets}
								<dl
									class="flex flex-col divide-y divide-border-subtle border-border-subtle max-lg:border-t max-lg:pt-2.5 lg:border-l lg:pl-8"
								>
									{#each budgets as budget (budget.label)}
										<div
											class="flex items-baseline justify-between gap-3 py-2 first:pt-0 last:pb-0"
										>
											<dt class="text-xs/normal text-text-2">{budget.label}</dt>
											<dd class="text-compact font-semibold text-text tabular-nums">
												{describeCost(budget.value)}
											</dd>
										</div>
									{/each}
								</dl>
							{/if}
						</div>
					</Card>

					<Card
						title="Checks and thresholds"
						subtitle="From checks.yaml on the server: what the judge rates at each stage, and where it blocks, holds, passes or asks back. Changed by pull request."
					>
						{#if o.settings.checks === null}
							<p class="text-xs/normal text-text-3">The core can't read checks.yaml.</p>
						{:else}
							<div class="grid gap-x-10 gap-y-5 xl:grid-cols-2">
								{#each stageColumns as column, n (n)}
									<div class="flex min-w-0 flex-col gap-5">
										{#each column as stage (stage)}
											{@const checks = o.settings.checks.filter((line) => line.stage === stage)}
											<div class="flex min-w-0 flex-col">
												<!-- the stage, and the names of the threshold columns under it -->
												<div class="flex items-center gap-2 border-b border-border pb-1.5">
													<h3
														id="checks-{stage}"
														class="truncate text-xs font-semibold text-text-2"
													>
														{stageWords[stage]}
													</h3>
													<span
														aria-hidden="true"
														class="grid h-4.5 min-w-4.5 shrink-0 place-items-center rounded-full bg-surface-3 px-1 text-2xs font-semibold text-text-2 tabular-nums"
														>{checks.length}</span
													>
													<span
														aria-hidden="true"
														class="ml-auto grid shrink-0 grid-cols-[3rem_3rem_3rem_4rem] text-2xs text-text-3"
													>
														{#each levels as level (level.key)}
															<span class="inline-flex items-center justify-end gap-1"
																><span class="size-1.5 shrink-0 rounded-full {DOT[level.tone]}"
																></span>{level.label}</span
															>
														{/each}
													</span>
												</div>
												<table
													class="w-full table-fixed text-2xs leading-4"
													aria-labelledby="checks-{stage}"
												>
													<colgroup>
														<col />
														<col class="w-12" />
														<col class="w-12" />
														<col class="w-12" />
														<col class="w-16" />
													</colgroup>
													<thead class="sr-only">
														<tr>
															<th scope="col">Check</th>
															{#each levels as level (level.key)}<th scope="col">{level.label}</th
																>{/each}
														</tr>
													</thead>
													<tbody class="divide-y divide-border-subtle">
														{#each checks as check (check.id)}
															<tr class="transition-colors duration-150 hover:bg-surface-2/60">
																<th scope="row" class="py-1.5 pr-3 text-left font-normal">
																	<code class="block truncate font-mono text-text" title={check.id}
																		>{check.id}</code
																	>
																</th>
																{#if ruled(check)}
																	<td colspan="4" class="py-1.5 text-right text-text-3">rule</td>
																{:else}
																	{#each levels as level (level.key)}
																		{@const value = check[level.key]}
																		<td class="py-1.5 text-right text-text tabular-nums">
																			{#if value === null}<span
																					class="text-text-3"
																					aria-hidden="true">–</span
																				>{:else}{level.key === 'clarify_below'
																					? '< '
																					: ''}{value.toFixed(2)}{/if}
																		</td>
																	{/each}
																{/if}
															</tr>
														{:else}
															<tr><td colspan="5" class="py-1.5 text-text-3">None.</td></tr>
														{/each}
													</tbody>
												</table>
											</div>
										{/each}
									</div>
								{/each}
							</div>
						{/if}
					</Card>

					<RunHint what="This page reads the core's numbers and changes nothing." />
				</div>
			{:else if read.loading}
				<p class="text-sm text-text-3" role="status">Reading the numbers…</p>
			{/if}
		{/if}
	</PageFrame>
</main>

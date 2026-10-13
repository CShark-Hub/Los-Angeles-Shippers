/**
 * The arithmetic behind the admin panel's figures and charts: totals and shares of outcome counts,
 * clean axis ticks, and the labels of a series' buckets. Pure functions, so the charts draw what
 * these say and the tests read the same numbers.
 */

import type { Bucket, OutcomeCounts, SeriesPoint } from '$lib/api';
import { LOCALE } from '$lib/format';
import { OUTCOMES } from './words';

export function total(counts: OutcomeCounts): number {
	return OUTCOMES.reduce((sum, outcome) => sum + counts[outcome], 0);
}

/** The questions that reached their asker: answered, or answered while the judge was degraded. */
export function delivered(counts: OutcomeCounts): number {
	return counts.answered + counts.degraded;
}

/** `part` of `whole`, 0 when there is no whole. */
export function share(part: number, whole: number): number {
	return whole > 0 ? part / whole : 0;
}

/** A share as a whole percentage, or a dash when there is nothing to share. */
export function percentOf(part: number, whole: number): string {
	if (whole <= 0) return '–';
	const value = (part / whole) * 100;
	// under one percent but not nothing: say so rather than round it away
	if (value > 0 && value < 1) return '<1%';
	return `${Math.round(value)}%`;
}

/** A count for a tile, in English: 1,284 · 12.9K · 1.2M. */
export function compact(value: number, locale: string = LOCALE): string {
	return new Intl.NumberFormat(locale, {
		notation: value >= 10_000 ? 'compact' : 'standard',
		maximumFractionDigits: 1
	}).format(value);
}

/**
 * Axis ticks from 0 to at least `max`, on a clean step (1, 2 or 5 times a power of ten), about
 * `count` of them. A max of 0 still has an axis, up to 1.
 */
export function ticks(max: number, count = 4): number[] {
	const top = Math.max(max, 1);
	const rough = top / count;
	const power = 10 ** Math.floor(Math.log10(rough));
	const step = [1, 2, 5, 10].map((m) => m * power).find((s) => s >= rough) ?? 10 * power;
	const whole = Math.max(1, Math.round(step));
	const steps = Math.ceil(top / whole);
	return Array.from({ length: steps + 1 }, (_, i) => i * whole);
}

/** The biggest bucket's total, for the chart's scale. */
export function peak(series: readonly SeriesPoint[]): number {
	return series.reduce((max, point) => Math.max(max, total(point.counts)), 0);
}

/**
 * A bucket's label under its column: the hour for a day's window, the day otherwise. A day's bucket
 * is the core's UTC day, from midnight UTC, so it is named in UTC: in their own time a viewer west
 * of UTC would read each day under the one before. An hour is the same hour anywhere, so it is
 * named in the viewer's time. Either way it is written in English.
 */
export function bucketLabel(start: string, bucket: Bucket, locale: string = LOCALE): string {
	const at = new Date(start);
	if (bucket === 'hour') {
		return new Intl.DateTimeFormat(locale, { hour: '2-digit', minute: '2-digit' }).format(at);
	}
	return new Intl.DateTimeFormat(locale, {
		weekday: 'short',
		day: 'numeric',
		timeZone: 'UTC'
	}).format(at);
}

/**
 * A bucket in full, for its tooltip, in English: the day and hour span in the viewer's time, or the
 * UTC day.
 */
export function bucketTitle(start: string, bucket: Bucket, locale: string = LOCALE): string {
	const at = new Date(start);
	if (bucket === 'hour') {
		const end = new Date(at.getTime() + 60 * 60 * 1000);
		const time = new Intl.DateTimeFormat(locale, { hour: '2-digit', minute: '2-digit' });
		const day = new Intl.DateTimeFormat(locale, {
			weekday: 'short',
			day: 'numeric',
			month: 'short'
		});
		return `${day.format(at)}, ${time.format(at)}–${time.format(end)}`;
	}
	return new Intl.DateTimeFormat(locale, {
		weekday: 'long',
		day: 'numeric',
		month: 'long',
		timeZone: 'UTC'
	}).format(at);
}

/**
 * Which buckets get a label under the axis, so the labels never crowd: every one up to 8 buckets,
 * then about 6 spread evenly, the last one always among them.
 */
export function labelled(count: number): (index: number) => boolean {
	if (count <= 8) return () => true;
	const every = Math.ceil(count / 6);
	return (index) => (count - 1 - index) % every === 0;
}

/** The totals of the series, for a tile's small trend line. */
export function trend(series: readonly SeriesPoint[]): number[] {
	return series.map((point) => total(point.counts));
}

/**
 * What a new conversation offers to ask. The tiles are only suggestions: a person may type anything
 * and the gateway decides, so a tile is left out where the person's own projects would refuse it.
 */

import type { ComponentProps } from 'svelte';

import type { ProjectSummary } from '$lib/api';
import type Icon from '$lib/components/Icon.svelte';

export type IconName = ComponentProps<typeof Icon>['name'];

export interface Example {
	label: string;
	text: string;
	icon: IconName;
	/** A question of no fixed kind, which only a project that answers open questions takes. */
	open?: boolean;
}

/** What to ask, when nothing has been asked yet, outside the mock (which offers its recordings). */
export const EXAMPLES: readonly Example[] = [
	{ label: 'Done this week', text: 'What was finished this week?', icon: 'check' },
	{ label: 'Blocked now', text: 'What is blocked right now?', icon: 'stop' },
	{ label: 'Missing tests', text: 'Which modules have no tests?', icon: 'flag' },
	{ label: 'Build health', text: 'How healthy is the build?', icon: 'shield' },
	{
		label: 'An estimate',
		text: 'How long would an export to Excel take?',
		icon: 'clock',
		open: true
	},
	{
		label: 'How it works',
		text: 'Does the invoice module support more than one currency?',
		icon: 'chat',
		open: true
	}
];

/**
 * The tiles for a person's projects: the four everyone may ask, and the open ones only when at
 * least one of their projects lists `open` among what it answers, so nobody is offered a tile their
 * own policy refuses.
 */
export function examplesFor(projects: ProjectSummary[]): Example[] {
	const answersOpen = projects.some((project) => {
		const classes: readonly string[] = project.classes;
		return classes.includes('open');
	});
	return EXAMPLES.filter((example) => answersOpen || !example.open);
}

/** How the start screen lays out its tiles. */
export interface TileGrid {
	/** The class that sets the columns on a wide screen. */
	columns: string;
	/** How many tiles, the first ones, are twice as wide. */
	wide: number;
}

/** The columns on a wide screen, by count, written out in full so the stylesheet finds each. */
const COLUMNS = ['', 'xl:grid-cols-1', 'xl:grid-cols-2', 'xl:grid-cols-3', 'xl:grid-cols-4'];

/**
 * The start screen's grid, so that every row is full: up to four tiles in one row; then rows of
 * four or three, and where neither divides the tiles, the first ones are twice as wide (seven as
 * a wide tile and two, then four; five as a wide tile and one, then three). On a middling screen
 * two columns, where a wide tile takes the whole row.
 */
export function gridFor(count: number): TileGrid {
	if (count <= 4) return { columns: COLUMNS[Math.max(count, 1)], wide: 0 };
	if (count % 4 === 0) return { columns: COLUMNS[4], wide: 0 };
	if (count % 3 === 0) return { columns: COLUMNS[3], wide: 0 };
	if (count % 3 === 2) return { columns: COLUMNS[3], wide: 1 };
	return { columns: COLUMNS[4], wide: 4 - (count % 4) };
}

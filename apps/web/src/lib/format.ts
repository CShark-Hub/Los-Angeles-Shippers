/** Words for what the core sends as codes and timestamps. */

import type { Role } from '$lib/api';

const ROLES: Record<Role, string> = {
	pm: 'Project manager',
	tech_lead: 'Tech lead',
	board: 'Board member',
	admin: 'Administrator'
};

export function roleLabel(role: Role): string {
	return ROLES[role] ?? role;
}

/**
 * The locale every date and number is written in. The client speaks English only, so the browser's
 * own language does not change it; times are still shown in the person's own time zone.
 */
export const LOCALE = 'en-GB';

const DAY = 24 * 60 * 60 * 1000;

export interface Expiry {
	/** The day and time, in English and the person's own time zone. */
	when: string;
	/** How long is left, or that it is over. */
	left: string;
	/** Whether it is over, or under two days away: worth a warning. */
	soon: boolean;
}

/** When a token expires and how long is left, at `now`. */
export function describeExpiry(
	expiresAt: string,
	now: Date = new Date(),
	locale: string = LOCALE
): Expiry {
	const end = new Date(expiresAt);
	const when = new Intl.DateTimeFormat(locale, { dateStyle: 'long', timeStyle: 'short' }).format(
		end
	);
	const remaining = end.getTime() - now.getTime();
	if (remaining <= 0) return { when, left: 'It has expired.', soon: true };
	const days = Math.floor(remaining / DAY);
	if (days >= 1) {
		return {
			when,
			left: `${days} day${days === 1 ? '' : 's'} left.`,
			soon: days < 2
		};
	}
	const hours = Math.max(1, Math.floor(remaining / (60 * 60 * 1000)));
	return { when, left: `Less than ${hours + 1} hours left.`, soon: true };
}

/** How long a question took: milliseconds under a second, then seconds, then minutes. */
export function describeDuration(ms: number): string {
	if (ms < 1000) return `${Math.round(ms)} ms`;
	const seconds = ms / 1000;
	if (seconds < 60) return `${seconds.toFixed(1)} s`;
	const minutes = Math.floor(seconds / 60);
	return `${minutes} min ${Math.round(seconds - minutes * 60)} s`;
}

/** What a question cost: cents under a dollar (a tenth of one under ten), dollars above. */
export function describeCost(usd: number): string {
	if (usd >= 1) return `$${usd.toFixed(2)}`;
	const cents = usd * 100;
	if (cents >= 10) return `${Math.round(cents)}¢`;
	return `${cents.toFixed(1).replace(/\.0$/, '')}¢`;
}

/** How long ago something was, in the words one would say it in. */
export function describeAge(then: string, now: Date = new Date()): string {
	const minutes = Math.floor((now.getTime() - new Date(then).getTime()) / 60_000);
	if (minutes < 1) return 'less than a minute ago';
	if (minutes < 60) return `${minutes} minute${minutes === 1 ? '' : 's'} ago`;
	const hours = Math.floor(minutes / 60);
	if (hours < 48) return `${hours} hour${hours === 1 ? '' : 's'} ago`;
	return `${Math.floor(hours / 24)} days ago`;
}

/** A day, in English and the person's own time zone. */
export function describeDay(iso: string, locale: string = LOCALE): string {
	return new Intl.DateTimeFormat(locale, { dateStyle: 'medium' }).format(new Date(iso));
}

/** A day and a time, in English and the person's own time zone. */
export function describeMoment(iso: string, locale: string = LOCALE): string {
	return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' }).format(
		new Date(iso)
	);
}

/** The time now, written as the core writes its times (RFC 3339, UTC). */
export function nowAsText(): string {
	return new Date().toISOString();
}

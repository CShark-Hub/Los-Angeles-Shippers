import { describe, expect, it } from 'vitest';

import {
	describeAge,
	describeCost,
	describeDay,
	describeDuration,
	describeExpiry,
	describeMoment,
	roleLabel
} from './format';

const NOW = new Date('2026-10-05T10:00:00.000Z');
const at = (ms: number) => new Date(NOW.getTime() + ms).toISOString();
const HOUR = 60 * 60 * 1000;

describe('roleLabel', () => {
	it('names every role in words', () => {
		expect((['pm', 'tech_lead', 'board', 'admin'] as const).map((role) => roleLabel(role))).toEqual(
			['Project manager', 'Tech lead', 'Board member', 'Administrator']
		);
	});
});

describe('describeExpiry', () => {
	it('counts the days left, and warns when under two', () => {
		expect(describeExpiry(at(29 * 24 * HOUR + HOUR), NOW).left).toBe('29 days left.');
		expect(describeExpiry(at(2 * 24 * HOUR + HOUR), NOW)).toMatchObject({
			left: '2 days left.',
			soon: false
		});
		expect(describeExpiry(at(24 * HOUR + HOUR), NOW)).toMatchObject({
			left: '1 day left.',
			soon: true
		});
	});

	it('says hours when under a day, and that it is over once it is', () => {
		expect(describeExpiry(at(5 * HOUR + 1000), NOW)).toMatchObject({
			left: 'Less than 6 hours left.',
			soon: true
		});
		expect(describeExpiry(at(0), NOW)).toMatchObject({ left: 'It has expired.', soon: true });
		expect(describeExpiry(at(-HOUR), NOW).left).toBe('It has expired.');
	});

	it('writes the date in English', () => {
		expect(describeExpiry('2026-12-31T12:00:00.000Z', NOW).when).toMatch(/^31 December 2026\b/);
	});
});

describe('describeDuration', () => {
	it('says milliseconds under a second, then seconds, then minutes', () => {
		expect(describeDuration(420)).toBe('420 ms');
		expect(describeDuration(1900)).toBe('1.9 s');
		expect(describeDuration(59_949)).toBe('59.9 s');
		expect(describeDuration(64_000)).toBe('1 min 4 s');
	});
});

describe('describeCost', () => {
	it('says cents under a dollar, with a decimal under ten, and dollars above', () => {
		expect(describeCost(0)).toBe('0¢');
		expect(describeCost(0.004)).toBe('0.4¢');
		expect(describeCost(0.05)).toBe('5¢');
		expect(describeCost(0.0624)).toBe('6.2¢');
		expect(describeCost(0.6)).toBe('60¢');
		expect(describeCost(1.2)).toBe('$1.20');
	});
});

describe('describeAge', () => {
	it('says how long ago in the words one would use', () => {
		const ago = (ms: number) => describeAge(at(-ms), NOW);
		expect(ago(20_000)).toBe('less than a minute ago');
		expect(ago(60_000)).toBe('1 minute ago');
		expect(ago(25 * 60_000)).toBe('25 minutes ago');
		expect(ago(3 * HOUR)).toBe('3 hours ago');
		expect(ago(72 * HOUR)).toBe('3 days ago');
	});
});

describe('describeDay', () => {
	it('writes a day in English', () => {
		expect(describeDay('2026-10-01T14:20:00Z')).toBe('1 Oct 2026');
	});
});

describe('describeMoment', () => {
	it('writes a day and a time in English, on a 24-hour clock', () => {
		expect(describeMoment('2026-10-01T14:20:00Z')).toMatch(/^1 Oct 2026, \d{2}:20$/);
	});
});

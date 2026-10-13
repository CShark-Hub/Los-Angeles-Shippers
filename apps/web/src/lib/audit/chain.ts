/**
 * The audit chain as far as one page of rows shows it, checked in the browser: each row's `hash`
 * recomputed from what the row carries, exactly as the core writes it
 * (`SHA-256(prev_hash ‖ seq ‖ len(type) ‖ type ‖ payload_sha256 ‖ at)`, `seq` as 8 big-endian
 * bytes, `len(type)` as one byte, `at` with milliseconds), and each row that follows the one before
 * it on the page naming that row's hash as its `prev_hash`. The whole chain, rows the person may
 * not see included, is the admin's `mercury-core admin audit verify`.
 */

import type { AuditItem } from '$lib/api';

export type ChainStatus =
	| { state: 'empty' }
	/** Every row's hash matches, and every row that follows another on the page links to it. */
	| { state: 'holds'; rows: number; linked: number }
	| { state: 'broken'; seq: number; what: 'hash' | 'link' };

function bytes(hex: string): Uint8Array {
	const out = new Uint8Array(hex.length / 2);
	for (let i = 0; i < out.length; i += 1) out[i] = parseInt(hex.slice(2 * i, 2 * i + 2), 16);
	return out;
}

function hex(buffer: ArrayBuffer): string {
	return [...new Uint8Array(buffer)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** `at` as the core stores it and hashes it: RFC 3339 in UTC with milliseconds. */
export function storedTime(at: string): string {
	return new Date(at).toISOString();
}

/** The hash a row should have, from what it carries. */
export async function rowHash(item: AuditItem): Promise<string> {
	const type = new TextEncoder().encode(item.type);
	const at = new TextEncoder().encode(storedTime(item.at));
	const seq = new Uint8Array(8);
	new DataView(seq.buffer).setBigUint64(0, BigInt(item.seq));
	const input = new Uint8Array(32 + 8 + 1 + type.length + 32 + at.length);
	let offset = 0;
	for (const part of [
		bytes(item.prev_hash),
		seq,
		Uint8Array.of(type.length),
		type,
		bytes(item.payload_sha256),
		at
	]) {
		input.set(part, offset);
		offset += part.length;
	}
	return hex(await crypto.subtle.digest('SHA-256', input));
}

/** Checks the rows of a page, in `seq` order; the first row that doesn't hold is named. */
export async function checkChain(items: readonly AuditItem[]): Promise<ChainStatus> {
	if (items.length === 0) return { state: 'empty' };
	let linked = 0;
	for (const [i, item] of items.entries()) {
		if ((await rowHash(item)) !== item.hash)
			return { state: 'broken', seq: item.seq, what: 'hash' };
		const before = items[i - 1];
		if (before && before.seq + 1 === item.seq) {
			if (item.prev_hash !== before.hash) return { state: 'broken', seq: item.seq, what: 'link' };
			linked += 1;
		}
	}
	return { state: 'holds', rows: items.length, linked };
}

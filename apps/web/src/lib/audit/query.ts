/** The audit routes' query, from the filters a person sets on the Audit screen. */

import type { QuestionClass, QuestionStatus } from '$lib/api';

/** Every class a question can have, refused ones (`forbidden`, `other`) included. */
export const CLASSES = [
	'progress',
	'tested',
	'blocked',
	'health',
	'changes',
	'open',
	'portfolio',
	'forbidden',
	'other'
] as const satisfies readonly QuestionClass[];

/** Every way a question can stand or end. */
export const OUTCOMES = [
	'running',
	'pending_signoff',
	'delivered',
	'refused',
	'clarify',
	'failed'
] as const satisfies readonly QuestionStatus[];

// a class or a status the core's types have and the lists lack is a type error here
const everyClass: Exclude<QuestionClass, (typeof CLASSES)[number]> extends never ? true : false =
	true;
const everyOutcome: Exclude<QuestionStatus, (typeof OUTCOMES)[number]> extends never
	? true
	: false = true;
void everyClass;
void everyOutcome;

/** Rows per page the screen asks for. */
export const PAGE_SIZE = 100;

export interface AuditFilters {
	project: string;
	user: string;
	question: string;
	class: string;
	outcome: string;
	/** A day, `YYYY-MM-DD`: from its midnight UTC. */
	from: string;
	/** A day, `YYYY-MM-DD`: before its midnight UTC. */
	to: string;
}

export function noFilters(): AuditFilters {
	return { project: '', user: '', question: '', class: '', outcome: '', from: '', to: '' };
}

/** The filters that are set, as the query both audit routes take. */
export function filterQuery(filters: AuditFilters): URLSearchParams {
	const query = new URLSearchParams();
	for (const [name, value] of Object.entries(filters) as [keyof AuditFilters, string][]) {
		const trimmed = value.trim();
		if (trimmed !== '') query.set(name, trimmed);
	}
	return query;
}

/** A page of rows, after `cursor` when there is one. */
export function pagePath(filters: AuditFilters, cursor: string | null = null): string {
	const query = filterQuery(filters);
	query.set('limit', String(PAGE_SIZE));
	if (cursor) query.set('cursor', cursor);
	return `/v1/audit?${query}`;
}

/**
 * One row with the export's filters, from the page route: what the export is tried with before the
 * browser downloads it. Both routes check the caller and read the filters alike, and a page writes no
 * audit row, where every export does.
 */
export function exportCheckPath(filters: AuditFilters): string {
	const query = filterQuery(filters);
	query.set('limit', '1');
	return `/v1/audit?${query}`;
}

/** Every matching row as JSON lines; the core answers it for an admin only. */
export function exportPath(filters: AuditFilters): string {
	const query = filterQuery(filters);
	query.set('format', 'jsonl');
	return `/v1/audit/export?${query}`;
}

/**
 * The Audit screen's rows under their headers, in the chain's order: a header for each run of rows
 * of one question that follow each other, and one for each run of rows of no question (an admin's,
 * a probe's). A question whose rows the chain interleaves with another's is shown in more than one
 * run, so no row moves out of its place, and the rows of a page loaded after the others land after
 * them: in the last run when the first of them belongs to it, under headers of their own after it
 * otherwise.
 */

import type { AuditItem } from '$lib/api';

export interface RowGroup {
	/** `rows-` and the number of the run's first row: one per run on the page, kept as the run grows. */
	key: string;
	/** The question its rows belong to; none for an admin's or a probe's rows. */
	question: string | null;
	/** Said once in the group's header; a row that names another says so itself. */
	project: string | null;
	user: string | null;
	items: AuditItem[];
}

/** The rows as they come, in `seq` order, run by run. */
export function groupRows(items: readonly AuditItem[]): RowGroup[] {
	const out: RowGroup[] = [];
	let run: RowGroup | undefined;
	for (const item of items) {
		if (run === undefined || run.question !== item.question_id) {
			run = {
				key: `rows-${item.seq}`,
				question: item.question_id,
				project: null,
				user: null,
				items: []
			};
			out.push(run);
		}
		// a question's header names its project and person; rows of no question each say their own
		if (run.question !== null) {
			run.project ??= item.project;
			run.user ??= item.user;
		}
		run.items.push(item);
	}
	return out;
}

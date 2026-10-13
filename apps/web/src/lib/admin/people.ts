/**
 * The People screen's arithmetic: which people the filters keep, the counts its header shows, and
 * which changes the core would take for a person, so the panel offers no command it refuses.
 * Pure, so the tests read the same answers the screen shows.
 */

import type { AdminGrant, AdminPerson, Role } from '$lib/api';

/** Every role, in the order the role picker lists them. */
const ROLES: readonly Role[] = ['pm', 'tech_lead', 'board', 'admin'];

/**
 * Whether a person of this role may approve. Only a project manager or a tech lead may: `user
 * grant --approver` refuses anyone else, and the core counts no one else's flag.
 */
export function mayApprove(role: Role): boolean {
	return role === 'pm' || role === 'tech_lead';
}

/**
 * Whether a grant's approver flag is in effect. A flag left on another role from before only a
 * project manager or a tech lead could approve grants nothing. Takes any person with a role and any
 * grant with a flag: a repository's list of who may ask carries both on one row.
 */
export function approves(
	person: Pick<AdminPerson, 'role'>,
	grant: Pick<AdminGrant, 'approver'>
): boolean {
	return grant.approver && mayApprove(person.role);
}

/** The projects the person holds an approver flag on, in effect or not. */
export function flagged(person: AdminPerson): string[] {
	return person.grants.filter((g) => g.approver).map((g) => g.project);
}

/** A role the person could be moved to, and why the core would refuse it, if it would. */
export interface RoleChoice {
	role: Role;
	refused: string | null;
}

/**
 * The roles the person could be moved to. `user set-role` refuses a board or admin role to anyone
 * who holds an approver flag on any grant, in effect or not, until the flags are off.
 */
export function roleChoices(person: AdminPerson): RoleChoice[] {
	const approver = flagged(person).length > 0;
	return ROLES.filter((role) => role !== person.role).map((role) => ({
		role,
		refused:
			approver && !mayApprove(role)
				? 'They hold an approver flag, which only a project manager or a tech lead may hold.'
				: null
	}));
}

export type StatusFilter = 'all' | 'active' | 'disabled';

export interface PeopleFilter {
	/** Part of a name or an e-mail address, any case. */
	query: string;
	role: Role | 'all';
	/** A project granted, `portfolio` included; `all` for any. */
	project: string;
	status: StatusFilter;
}

export function noFilter(): PeopleFilter {
	return { query: '', role: 'all', project: 'all', status: 'all' };
}

export function filterPeople(people: readonly AdminPerson[], filter: PeopleFilter): AdminPerson[] {
	const query = filter.query.trim().toLowerCase();
	return people.filter(
		(person) =>
			(query === '' ||
				person.display_name.toLowerCase().includes(query) ||
				person.email.toLowerCase().includes(query)) &&
			(filter.role === 'all' || person.role === filter.role) &&
			(filter.project === 'all' || person.grants.some((g) => g.project === filter.project)) &&
			(filter.status === 'all' || (filter.status === 'disabled') === (person.disabled_at !== null))
	);
}

export interface PeopleSummary {
	people: number;
	disabled: number;
	activeTokens: number;
	/** Active people with no project granted: they can sign in and ask about nothing. */
	withoutProject: number;
	/** Active people with an approver flag in effect on at least one grant. */
	approvers: number;
}

export function summarize(people: readonly AdminPerson[]): PeopleSummary {
	const active = people.filter((p) => p.disabled_at === null);
	return {
		people: people.length,
		disabled: people.length - active.length,
		activeTokens: active.reduce(
			(n, p) => n + p.tokens.filter((t) => t.state === 'active').length,
			0
		),
		withoutProject: active.filter((p) => p.grants.length === 0).length,
		approvers: active.filter((p) => p.grants.some((g) => approves(p, g))).length
	};
}

/** The projects a person could still be granted: the ones the server lists, and `portfolio`. */
export function grantable(person: AdminPerson, projects: readonly string[]): string[] {
	const held = new Set(person.grants.map((g) => g.project));
	return [...projects, 'portfolio'].filter((p) => !held.has(p));
}

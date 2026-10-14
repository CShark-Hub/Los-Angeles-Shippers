/**
 * The admin commands the panel shows beside what they change. The panel itself changes nothing:
 * every change is made on the server, over SSH, at the admin shell's prompt, which takes the
 * commands with nothing before them. These only put the command in words, ready to copy; the
 * values in them are the ones on screen.
 */

import type { Role } from '$lib/api';

/** What opens the admin shell on the server, for the line that says how to run the commands. */
export const ADMIN_SHELL = 'docker exec -it mercury-core-1 mercury-core admin shell';

/** A value as the admin shell reads it: bare when it is plain, in single quotes otherwise. */
export function quote(value: string): string {
	if (/^[A-Za-z0-9@._,:/+=-]+$/.test(value)) return value;
	return `'${value.replaceAll("'", `'\\''`)}'`;
}

function line(...parts: string[]): string {
	return parts.join(' ');
}

function grant(email: string, projects: readonly string[], approver = false): string {
	return line(
		'user grant --email',
		quote(email),
		'--projects',
		quote(projects.join(',')),
		...(approver ? ['--approver'] : [])
	);
}

export const commands = {
	addPerson: (email: string, name: string, role: Role) =>
		line('user add --email', quote(email), '--name', quote(name), '--role', role),
	grant,
	/**
	 * Takes the approver flag off the projects and keeps them granted: `user grant` sets the flag to
	 * what it says, so granting them again without `--approver` clears it.
	 */
	dropApprover: (email: string, projects: readonly string[]) => grant(email, projects, false),
	ungrant: (email: string, projects: readonly string[]) =>
		line('user ungrant --email', quote(email), '--projects', quote(projects.join(','))),
	setRole: (email: string, role: Role) =>
		line('user set-role --email', quote(email), '--role', role),
	disable: (email: string) => line('user disable --email', quote(email)),
	createToken: (email: string, label: string, expires = '90d') =>
		line('token create --email', quote(email), '--label', quote(label), '--expires', expires),
	listTokens: (email: string) => line('token list --email', quote(email)),
	revokeToken: (publicId: string) => line('token revoke', quote(publicId)),
	accessOf: (email: string) => line('access list --email', quote(email)),
	accessTo: (project: string) => line('access list --project', quote(project)),
	quarantine: (project: string, reason: string) =>
		line('probe quarantine', quote(project), '--reason', quote(reason)),
	release: (project: string) => line('probe release', quote(project)),
	probes: () => line('probe list'),
	status: () => line('status'),
	auditVerify: () => line('audit verify')
} as const;

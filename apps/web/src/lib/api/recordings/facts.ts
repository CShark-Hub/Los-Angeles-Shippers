import type { Fact } from '../types';

/** Facts as a probe states them, for the recordings to cite. Invented, like the fixture's. */
export const invoiceList: Fact = {
	id: 'f_7Qx2',
	kind: 'feature',
	module: 'Invoicing',
	title: 'Invoice list with filters',
	status: 'done',
	tested: 'passing',
	confidence: 0.93,
	evidence: [
		{ type: 'pr', ref: '#412', state: 'merged', at: '2026-10-01' },
		{ type: 'ci', ref: null, state: 'passed', at: '2026-10-01' }
	],
	people: [],
	title_withheld: false,
	updated_at: '2026-10-01T14:20:00Z',
	waiting_on: null
};

export const roundingFix: Fact = {
	id: 'f_9Kd1',
	kind: 'fix',
	module: 'Invoicing',
	title: 'Fix rounding in invoice totals',
	status: 'done',
	tested: 'passing',
	confidence: 0.91,
	evidence: [{ type: 'pr', ref: '#418', state: 'merged', at: '2026-10-02' }],
	people: [],
	title_withheld: false,
	updated_at: '2026-10-02T09:05:00Z',
	waiting_on: null
};

export const planUpgrade: Fact = {
	id: 'f_3Mn8',
	kind: 'feature',
	module: 'Plans',
	title: 'Monthly plan upgrade flow',
	status: 'done',
	tested: 'passing',
	confidence: 0.88,
	evidence: [{ type: 'pr', ref: '#97', state: 'merged', at: '2026-10-02' }],
	people: [],
	title_withheld: false,
	updated_at: '2026-10-02T16:40:00Z',
	waiting_on: null
};

/** A fact whose title a check replaced. */
export const withheldTitle: Fact = {
	id: 'f_5Rt4',
	kind: 'fix',
	module: 'Payments',
	title: '(title withheld)',
	status: 'done',
	tested: 'unknown',
	confidence: 0.74,
	evidence: [{ type: 'pr', ref: '#101', state: 'merged', at: '2026-10-03' }],
	people: [],
	title_withheld: true,
	updated_at: '2026-10-03T11:15:00Z',
	waiting_on: null
};

export const exportBlocked: Fact = {
	id: 'f_2Bw6',
	kind: 'feature',
	module: 'Export',
	title: 'SEPA export format',
	status: 'blocked',
	tested: 'missing',
	confidence: 0.8,
	evidence: [{ type: 'issue', ref: '#77', state: 'open', at: '2026-09-30' }],
	people: [],
	title_withheld: false,
	updated_at: '2026-10-03T08:00:00Z',
	waiting_on: 'decision'
};

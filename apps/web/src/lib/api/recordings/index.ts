import { blocked } from './blocked';
import { clarifiedBilling, clarifiedDelta, clarify } from './clarify';
import { confidential } from './confidential';
import { open } from './open';
import { progress } from './progress';
import { refused } from './refused';
import { signoff } from './signoff';
import type { Recording } from './recording';

export type { RecordedEvent, Recording } from './recording';

/** Every recorded question; the first is what any question not recorded plays. */
export const RECORDINGS: readonly Recording[] = [
	progress,
	refused,
	clarify,
	clarifiedDelta,
	clarifiedBilling,
	signoff,
	blocked,
	open,
	confidential
];

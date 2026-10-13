/**
 * The core's types as the app uses them, taken from `schema.d.ts`, which `just types` writes from
 * the core's OpenAPI document. Never write an API type by hand; add a line here instead.
 */

import type { components } from './schema';

type Schemas = components['schemas'];

/** The codes of the core's error body. */
export type ErrorCode = Schemas['ErrorCode'];

/** One event of a question's stream, the core's `Event`: its name decides what its data is. */
export type CoreEvent = Schemas['Event'];

/** `GET /v1/me`: who the session belongs to. */
export type Me = Schemas['Me'];

export type Role = Schemas['Role'];

/** A project the person was granted, or the pseudo-project `portfolio`. */
export type Grant = Schemas['Grant'];

/** `POST /v1/session`: the one request that carries the token. */
export type SessionRequest = Schemas['SessionRequest'];

/** `GET /v1/projects`: the projects the person may ask about, with what they may ask. */
export type ProjectList = Schemas['ProjectList'];
export type ProjectSummary = Schemas['ProjectSummary'];

/** The state of a project's probe: how fresh its data is. */
export type ProbeState = Schemas['ProbeState'];

/** `POST /v1/questions`, and the id it answers with. */
export type NewQuestion = Schemas['NewQuestion'];
export type Accepted = Schemas['Accepted'];
export type ClarifyAnswer = Schemas['ClarifyAnswer'];

/** `GET /v1/questions/{id}`: where a question stands. */
export type QuestionState = Schemas['QuestionState'];
export type QuestionStatus = Schemas['QuestionStatus'];
export type QuestionClass = Schemas['QuestionClass'];

/** What a question's events carry. */
export type Answer = Schemas['Answer'];
export type Fact = Schemas['Fact'];
export type FactStatus = Schemas['Status'];
export type FactTested = Schemas['Tested'];
export type Evidence = Schemas['Evidence'];
export type Refusal = Schemas['Refusal'];
export type RefusalCode = Schemas['RefusalCode'];
export type Clarify = Schemas['Clarify'];
export type CheckResult = Schemas['CheckResult'];
export type Verdict = Schemas['Verdict'];
export type Done = Schemas['Done'];
/**
 * How a question ended, as the answer card shows it: its `done`, or what the question's state says
 * of it, which doesn't name the composer.
 */
export type Ending = Omit<Done, 'composer'> & Partial<Pick<Done, 'composer'>>;
/** Who wrote an answer: the composer model, or the template while the model can't. */
export type ComposerMode = Schemas['ComposerMode'];
export type JudgeMode = Schemas['JudgeMode'];
export type PendingSignoff = Schemas['PendingSignoff'];
export type ProjectPicked = Schemas['ProjectPicked'];
export type AccessMode = Schemas['AccessMode'];

/** `GET /v1/approvals?status=pending`: the held answers of the projects the person approves. */
export type ApprovalList = Schemas['ApprovalList'];
export type Approval = Schemas['Approval'];
/** A sentence of a held draft that a check flagged, with the check and its probability. */
export type Flag = Schemas['Flag'];
/** Why an answer waits for an approver. */
export type SignoffReason = Schemas['SignoffReason'];
/** `POST /v1/approvals/{id}`. */
export type ApprovalDecision = Schemas['ApprovalDecision'];
export type Decision = Schemas['Decision'];

/** `GET /v1/audit`: a page of audit rows, each verifiable on its own. */
export type AuditPage = Schemas['AuditPage'];
export type AuditItem = Schemas['AuditItem'];

/** `GET /v1/admin/overview?window=…`: the admin panel's analytics. */
export type AdminOverview = Schemas['AdminOverview'];
export type OverviewWindow = Schemas['OverviewWindow'];
export type Bucket = Schemas['Bucket'];
export type OutcomeCounts = Schemas['OutcomeCounts'];
export type SeriesPoint = Schemas['SeriesPoint'];
export type AdminAlert = Schemas['AdminAlert'];
export type AlertKind = Schemas['AlertKind'];
/** What the core runs with, shown in the panel and never changed there. */
export type AdminSettings = Schemas['Settings'];
export type ModelRoleLine = Schemas['ModelRoleLine'];
export type CheckLine = Schemas['CheckLine'];
export type Channel = Schemas['Channel'];

/** `GET /v1/admin/people`: everyone, with their grants and tokens (never a secret). */
export type AdminPeople = Schemas['AdminPeople'];
export type AdminPerson = Schemas['AdminPerson'];
export type AdminGrant = Schemas['AdminGrant'];
export type AdminToken = Schemas['AdminToken'];
export type TokenState = Schemas['TokenState'];

/** `GET /v1/admin/repositories`: each project's probe and who may ask about it. */
export type AdminRepositories = Schemas['AdminRepositories'];
export type AdminRepository = Schemas['AdminRepository'];
export type AdminProbe = Schemas['AdminProbe'];
export type AccessLimit = Schemas['AccessLimit'];

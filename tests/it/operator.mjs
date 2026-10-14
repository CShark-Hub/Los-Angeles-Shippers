// What the operator and the API promise, shown through the stack rather than only in the core's own
// tests: a clarify round trip, a stream replayed after a reconnect, an `Idempotency-Key` that gives the
// same question again, a body over the cap, a token revoked while in use (the revocation check), one
// token's flood of questions (the flood check), the installation's daily budget across `/v1` and
// `/mcp`, and no code on a disk after the run. Each is a scenario of `just it` (run.mjs) with its own
// name in the run's list.
//
// The limits and the budget need the core restarted on lowered values (the run raises them for the
// golden set), so they run last, after everything else has asked what it needs. `reconfigure` is
// run.mjs's too, for a policy that leaves the open class out.

import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { admin, ask, configDir, http, mercury, restartCore, root, rows, secrets } from "./stack.mjs";
import { mcpClient } from "./mcp.mjs";

/** A question no recording answers: the rules and the template answer it from the fixture. */
export const OPEN_QUESTION = "What is in progress?";
/** A question the fixture answers whatever the date, with the rules and the template. */
const BLOCKED = "What is blocked right now, and what is it waiting on?";

const bearer = (token) => ({ authorization: `Bearer ${token}` });
/** What the CLI sends with a question, as a script calling the API would. */
const client = (token, key) => ({ ...bearer(token), "idempotency-key": key, "x-mercury-client": "cli" });

/** A person of the run's own, with a token: for a scenario that changes what the person may do. */
export function person(email, name, role, projects) {
  admin(["user", "add", "--email", email, "--name", name, "--role", role]);
  admin(["user", "grant", "--email", email, "--projects", projects.join(",")]);
  return token(email, "it");
}

/** A new token of `email`'s. */
export function token(email, label) {
  const created = admin(["token", "create", "--email", email, "--label", label]).match(/mcy_[A-Za-z0-9_-]+/)?.[0];
  if (!created) throw new Error(`token create printed no token for ${email}`);
  secrets.push(created);
  return created;
}

/** The public id of a token: the 12 characters after `mcy_`. */
const publicId = (secret) => secret.slice(4, 16);

/** The rows `admin audit export` writes for these filters, parsed. */
function exported(args) {
  return admin(["audit", "export", ...args])
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

/**
 * A clarify round trip: a manager with two projects asks without naming one, is asked which, and
 * answers with the option's id; the second question is the first's text, delivered from the project
 * picked, and the audit links it to the clarify.
 */
export async function clarifyChecks(asker, t, outputs) {
  const first = ask(asker, [OPEN_QUESTION]);
  outputs.push(first.stdout, first.stderr);
  t.eq(first.code, 3, `the question without a project: exit code (${first.stderr.trim().split("\n").at(-1)})`);
  t.eq(first.summary?.status, "clarify", "the question without a project: status");
  t.eq(first.summary?.clarify?.kind, "project", "the clarify asks for the project");
  t.eq(
    first.summary?.clarify?.options?.map((o) => o.id),
    ["delta", "poison"],
    "the clarify offers the asker's two projects",
  );
  t.eq(first.summary?.plan ?? null, null, "nothing planned before the clarify");
  const asked = await rows(asker, first.summary?.id);
  outputs.push(JSON.stringify(asked));
  t.eq(asked.map((r) => r.event), ["accepted", "classified", "clarify"], "the clarify's rows: no project to plan for");

  const second = ask(asker, [OPEN_QUESTION, "--clarify", `${first.summary?.id}:delta`]);
  outputs.push(second.stdout, second.stderr);
  t.eq(second.code, 0, `the answer to the clarify: exit code (${second.stderr.trim().split("\n").at(-1)})`);
  t.eq([second.summary?.status, second.summary?.project], ["delivered", "delta"], "delivered from the project picked");
  t.ok(second.summary?.answer?.facts?.length > 0, "with facts");
  t.ok(second.summary?.id !== first.summary?.id, "a question of its own");

  const again = ask(asker, [OPEN_QUESTION, "--clarify", `${first.summary?.id}:kappa`]);
  outputs.push(again.stdout, again.stderr);
  t.ok(again.code !== 0 && again.summary?.status !== "delivered", `an option the clarify didn't offer: exit ${again.code}`);
}

/**
 * The API's promises to a client that reconnects or retries: the same `Idempotency-Key` gives the same
 * question, a request without one is refused, `Last-Event-ID` replays the stream after that row only,
 * and a body one byte over 64 KB is refused before anything reads it.
 */
export async function apiChecks(asker, t) {
  const body = { text: BLOCKED, project: "delta" };
  const sent = await http("POST", "/v1/questions", { headers: client(asker, "it-retry-1"), body });
  const retried = await http("POST", "/v1/questions", { headers: client(asker, "it-retry-1"), body });
  t.eq([sent.status, retried.status], [202, 202], "a question and its retry: 202 each");
  t.ok(Boolean(sent.json?.id) && sent.json?.id === retried.json?.id, `the retry is the same question (${sent.json?.id}, ${retried.json?.id})`);
  const other = await http("POST", "/v1/questions", { headers: client(asker, "it-retry-2"), body });
  t.ok(other.status === 202 && other.json?.id !== sent.json?.id, "another key is another question");
  const keyless = await http("POST", "/v1/questions", { headers: { ...bearer(asker), "x-mercury-client": "cli" }, body });
  t.eq([keyless.status, keyless.json?.error?.code], [400, "invalid_request"], "no Idempotency-Key: refused");

  // the whole stream, then the stream after its fourth row, as a reconnecting client asks for it
  const whole = await rows(asker, sent.json?.id);
  t.eq(whole.at(-1)?.event, "done", "the question ends");
  const after = whole[3]?.id;
  const replayed = await http("GET", `/v1/questions/${sent.json?.id}/events`, {
    headers: { ...bearer(asker), accept: "text/event-stream", "last-event-id": String(after) },
  });
  const ids = [...replayed.text.matchAll(/^id: ?(\d+)$/gm)].map((m) => Number(m[1]));
  t.eq(replayed.status, 200, "the replay: status");
  t.eq(ids, whole.slice(4).map((r) => r.id), `the replay after row ${after}: only the rows after it`);

  // one byte over the cap: refused before the body is read, nothing started
  const before = exported(["--token", publicId(asker)]).filter((r) => r.type === "accepted").length;
  const padding = "x".repeat(64 * 1024 - JSON.stringify({ text: "", project: "delta" }).length + 1);
  const over = await http("POST", "/v1/questions", { headers: client(asker, "it-over"), body: { text: padding, project: "delta" } });
  t.eq(over.status, 413, `a body one byte over 64 KB: ${over.status} ${over.text.slice(0, 120)}`);
  const started = exported(["--token", publicId(asker)]).filter((r) => r.type === "accepted").length;
  t.eq(started, before, "the refused body started no question");
}

/**
 * The revocation check: a token revoked while in use. The next request with it is refused within a
 * second, and the audit export by that token lists what it asked.
 */
export async function revokedInUseChecks(owner, t, outputs) {
  const live = token(owner, "revoked");
  const run = ask(live, ["--project", "delta", BLOCKED]);
  outputs.push(run.stdout, run.stderr);
  t.eq(run.code, 0, `the token answers before the revocation (${run.stderr.trim().split("\n").at(-1)})`);
  const me = await http("GET", "/v1/me", { headers: bearer(live) });
  t.eq(me.status, 200, "GET /v1/me before the revocation");

  const revoked = Date.now();
  admin(["token", "revoke", publicId(live)]);
  const refused = await http("GET", "/v1/me", { headers: bearer(live) });
  const took = Date.now() - revoked;
  t.eq(refused.status, 401, "GET /v1/me right after the revocation: 401");
  t.ok(took < 1000, `refused within a second of the revocation (${took} ms, the admin command included)`);
  const cli = mercury(live, ["ask", "--json", "--project", "delta", BLOCKED]);
  outputs.push(cli.stdout, cli.stderr);
  t.eq(cli.code, 1, "the CLI with the revoked token: exit code");

  const trail = exported(["--token", publicId(live)]);
  const types = trail.map((r) => r.type);
  t.ok(types.includes("token_created") && types.includes("token_revoked"), `the export by token holds its creation and revocation (${types.join(",")})`);
  t.ok(trail.some((r) => r.type === "accepted" && r.question_id === run.summary?.id), "the export by token lists the question it asked");
  t.ok(trail.every((r) => r.token === publicId(live)), "every row of the export is the token's");
}

/**
 * The run's configuration file `name`, with `edit` applied, so the next restart reads it. Gives the
 * text it had before, for a scenario that puts it back.
 */
export function reconfigure(name, edit) {
  const file = join(configDir, name);
  const before = readFileSync(file, "utf8");
  const after = edit(before);
  if (after === before) throw new Error(`${name}: the edit changed nothing`);
  writeFileSync(file, after);
  return before;
}

/**
 * The flood check: one token sending 100 questions in a minute, on the limits the private and strict
 * presets ship (30 an hour, a burst of 5): five start, the rest are refused with `429` and
 * `Retry-After` before anything is spent, and the person's spend is what the five cost.
 */
export async function floodChecks(t) {
  reconfigure("policies.yaml", (text) =>
    text.replace(/questions_per_hour: \d+/, "questions_per_hour: 30").replace(/burst: \d+/, "burst: 5"),
  );
  await restartCore();
  const flooder = person("flood@example.com", "Flood", "pm", ["delta"]);
  const statuses = {};
  let retryAfter = null;
  let code = null;
  const started = Date.now();
  for (let i = 0; i < 100; i += 1) {
    const sent = await http("POST", "/v1/questions", { headers: client(flooder, `it-flood-${i}`), body: { text: BLOCKED, project: "delta" } });
    statuses[sent.status] = (statuses[sent.status] ?? 0) + 1;
    if (sent.status === 429) {
      retryAfter ??= sent.headers["retry-after"];
      code ??= sent.json?.error?.code;
    }
  }
  const took = Date.now() - started;
  t.ok(took < 60_000, `100 questions within a minute (${took} ms)`);
  t.eq(statuses, { 202: 5, 429: 95 }, "five accepted, the other 95 refused");
  t.eq(code, "rate_limited", "the refusal's code");
  t.ok(Number(retryAfter) > 0, `Retry-After on the refusal (${retryAfter})`);
  const accepted = exported(["--token", publicId(flooder)]).filter((r) => r.type === "accepted").length;
  t.eq(accepted, 5, "the audit holds five questions started by the token");
  const spend = admin(["status"]).match(/^flood@example\.com\s+\$(\d+\.\d+)/m)?.[1];
  t.ok(spend !== undefined && Number(spend) < 0.05, `the person's spend is the five questions' (${spend})`);
}

/**
 * The installation's daily budget across the API and MCP: with the day's spend over the global cap,
 * a question from the CLI and one from an agent over MCP are refused with `budget_exceeded` before
 * they start, and nothing is spent.
 */
export async function globalBudgetChecks(t, outputs) {
  reconfigure("models.yaml", (text) => text.replace(/global_daily_usd: [\d.]+/, "global_daily_usd: 0.01"));
  await restartCore();
  const asker = person("late@example.com", "Late", "pm", ["delta"]);
  const before = admin(["status"]).match(/^everyone\s+\$(\d+\.\d+)/m)?.[1];

  const cli = ask(asker, ["--project", "delta", BLOCKED]);
  outputs.push(cli.stdout, cli.stderr);
  t.eq(cli.code, 1, "the CLI: exit code");
  t.eq(cli.summary?.error?.code, "budget_exceeded", "the CLI: budget_exceeded");
  t.ok(/tomorrow/.test(cli.summary?.error?.hint ?? ""), `the CLI: the hint (${cli.summary?.error?.hint})`);

  const agent = mcpClient(asker);
  await agent.initialize();
  const called = await agent.callTool("ask", { question: BLOCKED, project: "delta" });
  outputs.push(called.raw);
  t.eq(called.status, 200, "MCP ask: status");
  t.eq(called.result?.structuredContent?.error?.code, "budget_exceeded", "MCP ask: budget_exceeded");
  t.eq(called.result?.structuredContent?.answer ?? null, null, "MCP ask: no answer");

  const after = admin(["status"]).match(/^everyone\s+\$(\d+\.\d+)/m)?.[1];
  t.eq(after, before, "nothing spent by the two refusals");
  t.eq(exported(["--token", publicId(asker)]).filter((r) => r.type === "accepted").length, 0, "neither question started");
}

/** No file of the fixture's on a named volume or in Docker's data directory, after every question of the run. */
export function diskChecks(project, t, outputs) {
  const run = spawnSync("bash", [join(root, "deploy/compose/scripts/check.sh"), "disk"], {
    cwd: root,
    encoding: "utf8",
    env: { ...process.env, MERCURY_COMPOSE_PROJECT: project, MSYS_NO_PATHCONV: "1" },
    timeout: 300_000,
  });
  outputs.push(run.stdout ?? "", run.stderr ?? "");
  if (run.error) return t.ok(false, `check.sh disk can't be run: ${run.error.message}`);
  t.eq(run.status, 0, `check.sh disk: exit code (${(run.stderr || run.stdout).trim().split("\n").at(-1)})`);
  t.ok(/^ok: no file of the fixture repository/m.test(run.stdout), "check.sh disk: no copy of the fixture found");
}

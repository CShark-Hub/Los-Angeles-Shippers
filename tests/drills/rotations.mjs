// The drill "rotate a secret": on a fresh stack, each secret the operator rotates is replaced the way
// docs/deploy.md says (a new file, then a restart of the services that read it), and every project must
// answer again within a minute of the restart. The model provider's key: the core reads it at start,
// so it is replaced and the core restarted; a question afterwards is still judged and written by the
// models. A probe's token: the core and that probe both read it at start, so both restart with the new
// one; the probe registers with it and the project answers. A person's token is revoked and replaced
// (the revocation check in `just it` shows the revocation itself). The git host's token is the
// operator's own, on a stack that syncs, and isn't drilled here.
//
// The models are the replay server's recordings, so the drill costs nothing, and the model key it
// rotates is the one the core sends to that server (the run's `replay_key`): the same file, read
// the same way, as the provider's key on a stack with real models. It runs under its own Compose project, port and subnets, so it never touches
// the local stack; the stack goes down with its volumes when the drill ends, unless `--keep`.
//
//   node tests/drills/rotations.mjs [--keep]

import { randomBytes } from "node:crypto";
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join, relative, sep } from "node:path";

// stack.mjs reads these when it is imported, so they are set first
const DEFAULTS = {
  MERCURY_IT_PROJECT: "mercury-drill-rotate",
  MERCURY_IT_PORT: "18104",
  MERCURY_IT_PROBES_SUBNET: "172.30.57.0/24",
  MERCURY_IT_EDGE_SUBNET: "172.30.56.0/24",
};
for (const [name, value] of Object.entries(DEFAULTS)) process.env[name] ??= value;

const { PEOPLE, admin, ask, composeCommand, http, leaked, mask, root, rows, secrets, withStack } = await import("../it/stack.mjs");

/** A question a recording answers, judged and written by the (replayed) models. */
const QUESTION = "what was merged this week?";
/** Within how long of a restart every project must answer again, in seconds (the core reads its secrets at start). */
const WITHIN = 60;

const failures = [];
const outputs = [];
const work = join(root, "target", process.env.MERCURY_IT_PROJECT);

function check(name, holds, seen) {
  const line = holds ? `  ok    ${name}` : `  FAIL  ${name}${seen === undefined ? "" : `: saw ${seen}`}`;
  console.log(mask(line));
  if (!holds) failures.push(name);
  return holds;
}

const say = (text) => console.log(mask(text));
const phase = (title) => say(`\n== ${title}`);
const seconds = (ms) => `${(ms / 1000).toFixed(1)} s`;
const wait = (ms) => new Promise((done) => setTimeout(done, ms));

/** `docker compose <args>` on this drill's project, with the files and variables stack.mjs uses. */
function compose(args) {
  const composeDir = join(root, "deploy/compose");
  const run = spawnSync("docker", [...composeCommand.split(" ").slice(1), ...args], {
    cwd: composeDir,
    encoding: "utf8",
    env: { ...process.env, MERCURY_IT_WORK: relative(composeDir, work).split(sep).join("/") },
    timeout: 120_000,
  });
  if (run.error) throw new Error(`docker compose ${args[0]} can't be run: ${run.error.message}`);
  if (run.status !== 0) throw new Error(`docker compose ${args.join(" ")} failed: ${mask(run.stderr.trim())}`);
  return run.stdout;
}

/** Replaces a secret file's content in place, as `tee` does: the container's bind mount sees the new bytes. */
function replace(file, value) {
  if (!value) throw new Error(`a new ${file} must not be empty`);
  const path = join(work, "secrets", file);
  const before = readFileSync(path, "utf8");
  writeFileSync(path, value);
  secrets.push(value);
  say(`      secrets/${file}: replaced (${before.length} → ${value.length} characters)`);
  return before;
}

const fresh = (bytes) => randomBytes(bytes).toString("base64url");

/** One question to delta as `token`: what it ended as, its judge and its rows. */
async function asked(token, label) {
  const run = ask(token, ["--project", "delta", QUESTION]);
  outputs.push(run.stdout, run.stderr);
  const found = run.summary?.id ? await rows(token, run.summary.id) : [];
  outputs.push(JSON.stringify(found));
  say(`      ${label}: mercury ask --project delta "${QUESTION}" → exit ${run.code}, ${run.summary?.status}, judge ${run.summary?.judge_mode}, composer ${run.summary?.composer}`);
  return run;
}

/** After a restart: the core answers through Caddy and delta is registered; then a question is answered. */
async function answersAgain(token, restartedAt, label, baseline) {
  let healthy = false;
  while (Date.now() - restartedAt < WITHIN * 1000) {
    const health = await http("GET", "/v1/health").catch(() => ({ status: 0 }));
    if (health.status === 200 && /^delta\s+healthy\b/m.test(admin(["probe", "list"]))) {
      healthy = true;
      break;
    }
    await wait(500);
  }
  check(`${label}: the core answers and delta is registered`, healthy, `not within ${WITHIN} s`);
  const run = await asked(token, label);
  const took = Date.now() - restartedAt;
  check(`${label}: delta answers within ${WITHIN} s of the restart`, run.code === 0 && took < WITHIN * 1000, `exit ${run.code} after ${seconds(took)}`);
  check(`${label}: the same answer as before`, run.summary?.answer?.text === baseline.summary?.answer?.text, run.summary?.answer?.text);
  check(`${label}: judged and written by the models, not the rules`, run.summary?.judge_mode === "chat" && run.summary?.composer === "model", `${run.summary?.judge_mode}, ${run.summary?.composer}`);
  say(`      answering ${seconds(took)} after the restart`);
  return took;
}

function auditHolds(after) {
  const verified = admin(["audit", "verify"]).trim();
  say(`      admin audit verify → ${verified}`);
  check(`audit verify after ${after}`, /^ok: \d+ row\(s\), the chain holds$/.test(verified), verified);
}

await withStack(async (tokens) => {
  say(`the drill's stack: ${composeCommand}`);
  const timings = {};

  phase("1. baseline");
  const baseline = await asked(tokens.pm, "before any rotation");
  check("baseline: answered by the models", baseline.code === 0 && baseline.summary?.judge_mode === "chat", `exit ${baseline.code}, ${baseline.summary?.judge_mode}`);
  auditHolds("the baseline");

  phase("2. the model provider's key: a new file, then `docker compose restart core`");
  replace("replay_key", fresh(24));
  say("      docker compose restart core");
  let restartedAt = Date.now();
  compose(["restart", "core"]);
  timings.modelKey = await answersAgain(tokens.pm, restartedAt, "after the model key's rotation", baseline);
  auditHolds("the model key's rotation");

  phase("3. a probe's token: a new file, then the core and that probe restarted");
  replace("probes/delta.token", fresh(32));
  say("      docker compose restart core probe-delta");
  restartedAt = Date.now();
  compose(["restart", "core", "probe-delta"]);
  timings.probeToken = await answersAgain(tokens.pm, restartedAt, "after the probe token's rotation", baseline);
  const listed = admin(["probe", "list"]);
  for (const line of listed.trimEnd().split("\n")) say(`        ${line}`);
  auditHolds("the probe token's rotation");

  phase("4. a person's token: a new one, then the old one revoked");
  const next = admin(["token", "create", "--email", PEOPLE.pm.email, "--label", "rotated"]).match(/mcy_[A-Za-z0-9_-]+/)?.[0];
  if (next) secrets.push(next);
  check("token create printed the new token", Boolean(next));
  admin(["token", "revoke", tokens.pm.slice(4, 16)]);
  const old = await http("GET", "/v1/me", { headers: { authorization: `Bearer ${tokens.pm}` } });
  check("the old token: 401", old.status === 401, old.status);
  const fresher = await asked(next, "with the new token");
  check("the new token answers, no restart", fresher.code === 0, `exit ${fresher.code}`);
  auditHolds("the person's token's rotation");

  phase("5. what the drill saw");
  say(`      every project answered again ${seconds(timings.modelKey)} after the model key's restart and ${seconds(timings.probeToken)} after the probe token's`);
  check("no secret in the CLI's outputs or the streams", leaked(outputs.join("\n")) === 0, "a secret");
  say(failures.length === 0 ? "\nthe drill passed" : `\nthe drill failed: ${failures.length} check(s)\n  ${failures.join("\n  ")}`);
  return failures.length === 0;
});

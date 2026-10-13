// The drill "a probe is down or stale": on a fresh stack with two projects (delta, the fixture, and
// poison, the red team's repository, each with its own probe), it stops delta's probe, checks that
// poison still answers while delta's questions fail with `probe_unavailable` and nothing else, waits
// until `probe list` and `admin status` show delta `stale`, waits past 15 minutes of silence until
// `admin status --check` exits 1 naming delta's probe, starts the probe again and checks that delta
// answers as before. After each phase the audit chain must hold. It takes about twenty minutes.
//
// The core keeps no copy of a probe's facts, so a question to a stopped probe has nothing to answer
// from: it fails with `probe_unavailable` whether the probe shows healthy or stale yet. The note on
// how old the data is belongs to a stale probe that still answers, which a stopped one can't.
//
// The models are the replay server's recordings, so the drill costs nothing. It runs under its own
// Compose project, port and subnets, so it can run beside the other drills and never touches the
// local stack; the stack goes down with its volumes when the drill ends, unless `--keep`.
//
//   node tests/drills/probe-down.mjs [--keep]

import { spawnSync } from "node:child_process";
import { join, relative, sep } from "node:path";

// stack.mjs reads these when it is imported, so they are set first
const DEFAULTS = {
  MERCURY_IT_PROJECT: "mercury-drill-probe",
  MERCURY_IT_PORT: "18102",
  MERCURY_IT_PROBES_SUBNET: "172.30.53.0/24",
  MERCURY_IT_EDGE_SUBNET: "172.30.52.0/24",
};
for (const [name, value] of Object.entries(DEFAULTS)) process.env[name] ??= value;

const { admin, ask, composeCommand, leaked, mask, root, rows, withStack } = await import("../it/stack.mjs");

/** The question to delta: a recording of the replay server answers it from the fixture. */
const DELTA_QUESTION = "what was merged this week?";
/** The question to poison, asked as `red`: the red team's run expects it answered with this blocker. */
const POISON_QUESTION = "What is blocked right now, and what is it waiting on?";
const POISON_BLOCKER = "Payment callback signature check fails";

/** How long the probe may take to show as stale after it stopped: three minutes of silence, plus slack. */
const STALE_WITHIN = 300;
/** A probe registers every minute, so its last registration is at most a minute older than the stop. */
const STALE_NOT_BEFORE = 115;
/** The alert on a silent probe holds after 15 minutes without a registration; the core checks once a minute. */
const SILENT_AFTER = 15 * 60;
/** How long past the stop the alert may take: the 15 minutes, the minute since the last registration, the minute's check, slack. */
const ALERT_WITHIN = SILENT_AFTER + 4 * 60;
/** How long the probe may take to register and be checked again after it started. */
const HEALTHY_WITHIN = 120;

const failures = [];
const outputs = [];

function check(name, holds, seen) {
  const line = holds ? `  ok    ${name}` : `  FAIL  ${name}${seen === undefined ? "" : `: saw ${seen}`}`;
  console.log(mask(line));
  if (!holds) failures.push(name);
  return holds;
}

function say(text) {
  console.log(mask(text));
}

function phase(title) {
  say(`\n== ${title}`);
}

const seconds = (ms) => `${(ms / 1000).toFixed(1)} s`;
const wait = (ms) => new Promise((done) => setTimeout(done, ms));

/**
 * `docker compose <args>` on this drill's project, the way stack.mjs runs it: the same files, the same
 * folder and the same variables (stack.mjs doesn't export its own helper).
 */
function compose(args) {
  const run = composeRun(args);
  if (run.status !== 0) throw new Error(`docker compose ${args.join(" ")} failed: ${mask(run.stderr.trim())}`);
  return run.stdout;
}

/** `docker compose <args>` as it ran, whatever its exit code. */
function composeRun(args) {
  const composeDir = join(root, "deploy/compose");
  const work = join(root, "target", process.env.MERCURY_IT_PROJECT);
  const run = spawnSync("docker", [...composeCommand.split(" ").slice(1), ...args], {
    cwd: composeDir,
    encoding: "utf8",
    env: {
      ...process.env,
      MERCURY_IT_WORK: relative(composeDir, work).split(sep).join("/"),
    },
    timeout: 120_000,
  });
  if (run.error) throw new Error(`docker compose ${args[0]} can't be run: ${run.error.message}`);
  return run;
}

/** `admin status --check`: its exit code and the alerts it printed, one a line. */
function statusCheck() {
  const run = composeRun(["exec", "-T", "core", "mercury-core", "admin", "status", "--check"]);
  const alerts = (run.stdout + run.stderr).split(/\r?\n/).filter(Boolean);
  return { code: run.status, alerts };
}

/** Each probe of `probe list`: project → { status, lastSeen }. */
function probes() {
  const listed = admin(["probe", "list"]);
  const found = {};
  for (const line of listed.split("\n")) {
    const m = line.match(/^(\S+)\s+(healthy|stale|quarantined)\s+\S+(?:\s+\(limited by \S+\))?\s+(\d{4}-\d\d-\d\d \d\d:\d\d)/);
    if (m) found[m[1]] = { status: m[2], lastSeen: m[3] };
  }
  return { listed, found };
}

function showList(listed) {
  for (const line of listed.trimEnd().split("\n")) say(`        ${line}`);
}

/** The timeline the CLI printed, indented. */
function showTimeline(run) {
  const lines = run.stderr.replace(/\r\n/g, "\n").trimEnd().split("\n").filter(Boolean);
  for (const line of lines) say(`        ${line}`);
}

/** One question as `mercury ask --json`, with what it ended as and its audit rows. */
async function asked(token, project, question) {
  const started = Date.now();
  const run = ask(token, ["--project", project, question]);
  const took = Date.now() - started;
  outputs.push(run.stdout, run.stderr);
  const found = run.summary?.id ? await rows(token, run.summary.id) : [];
  outputs.push(JSON.stringify(found));
  say(`      mercury ask --project ${project} "${question}" --json → exit ${run.code} in ${seconds(took)}`);
  showTimeline(run);
  return { ...run, took, found, events: found.map((r) => r.event) };
}

function answered(run, label) {
  check(`${label}: exit 0`, run.code === 0, `exit ${run.code} (${run.stderr.trim().split("\n").at(-1)})`);
  check(`${label}: delivered`, run.summary?.status === "delivered", run.summary?.status);
  check(`${label}: an answer with facts`, Boolean(run.summary?.answer?.text) && run.summary.answer.facts?.length > 0, JSON.stringify(run.summary?.answer ?? null));
  check(`${label}: its answer and done rows`, run.events.includes("answer") && run.events.at(-1) === "done", run.events.join(","));
}

function poisonAnswered(run, label) {
  answered(run, label);
  const blocker = run.summary?.answer?.facts?.find((f) => f.title === POISON_BLOCKER);
  check(`${label}: the blocker "${POISON_BLOCKER}" is blocked`, blocker?.status === "blocked", JSON.stringify(blocker ?? null));
}

/** A question to delta while its probe is down: probe_unavailable, and nothing read or delivered. */
function unavailable(run, label) {
  check(`${label}: exit 1`, run.code === 1, `exit ${run.code}`);
  check(`${label}: failed`, run.summary?.status === "failed", run.summary?.status);
  check(`${label}: error probe_unavailable`, run.summary?.error?.code === "probe_unavailable", JSON.stringify(run.summary?.error ?? null));
  check(`${label}: no answer`, (run.summary?.answer ?? null) === null, JSON.stringify(run.summary?.answer));
  check(
    `${label}: no fact read, nothing composed or delivered`,
    !run.events.some((e) => ["tool_result", "composing", "judged", "answer", "done"].includes(e)),
    run.events.join(","),
  );
  const error = run.found.find((r) => r.event === "error")?.data;
  check(`${label}: the error row is the last, with probe_unavailable`, run.events.at(-1) === "error" && error?.code === "probe_unavailable", JSON.stringify(error ?? null));
  say(`      rows: ${run.events.join(", ")}`);
}

function auditHolds(after) {
  const verified = admin(["audit", "verify"]).trim();
  say(`      admin audit verify → ${verified}`);
  check(`audit verify after ${after}`, /^ok: \d+ row\(s\), the chain holds$/.test(verified), verified);
}

/** The probe lines of `admin status`. */
function statusProbes() {
  const status = admin(["status"]);
  const lines = status.split("\n").filter((line) => /^(PROJECT|delta|poison)\s/.test(line));
  for (const line of lines) say(`        ${line}`);
  return lines;
}

await withStack(
  async (tokens) => {
    say(`the drill's stack: ${composeCommand}`);

    phase("1. baseline: both probes healthy, both projects answer");
    {
      const { listed, found } = probes();
      say("      admin probe list");
      showList(listed);
      check("delta healthy", found.delta?.status === "healthy", found.delta?.status);
      check("poison healthy", found.poison?.status === "healthy", found.poison?.status);
    }
    const deltaBefore = await asked(tokens.pm, "delta", DELTA_QUESTION);
    answered(deltaBefore, "delta, as pm");
    const poisonBefore = await asked(tokens.red, "poison", POISON_QUESTION);
    poisonAnswered(poisonBefore, "poison, as red");
    auditHolds("the baseline");

    phase("2. delta's probe stopped");
    say("      docker compose stop probe-delta");
    compose(["stop", "probe-delta"]);
    const stoppedAt = Date.now();
    const state = compose(["ps", "-a", "--format", "{{.Service}} {{.State}}", "probe-delta"]).trim();
    say(`      docker compose ps probe-delta → ${state}`);
    check("probe-delta is stopped", /^probe-delta exited\b/m.test(state), state);
    {
      const { listed, found } = probes();
      say(`      admin probe list, ${seconds(Date.now() - stoppedAt)} after the stop`);
      showList(listed);
      // a probe shows stale only three minutes after its last registration, which may be a minute old
      check("delta still shows healthy right after the stop", found.delta?.status === "healthy", found.delta?.status);
    }
    const deltaDown = await asked(tokens.pm, "delta", DELTA_QUESTION);
    unavailable(deltaDown, "delta while its probe is down");
    const poisonDuring = await asked(tokens.red, "poison", POISON_QUESTION);
    poisonAnswered(poisonDuring, "poison while delta's probe is down");
    check("poison's answer is the baseline's", poisonDuring.summary?.answer?.text === poisonBefore.summary?.answer?.text, poisonDuring.summary?.answer?.text);
    auditHolds("the stop");

    phase("3. delta goes stale");
    let stale = null;
    let last = null;
    let poisonStayed = true;
    while (Date.now() - stoppedAt < STALE_WITHIN * 1000) {
      last = probes();
      if (last.found.poison?.status !== "healthy") poisonStayed = false;
      if (last.found.delta?.status === "stale") {
        stale = Date.now() - stoppedAt;
        break;
      }
      await wait(5000);
    }
    say(`      admin probe list, ${seconds(Date.now() - stoppedAt)} after the stop`);
    showList(last.listed);
    check(
      `delta stale within ${STALE_WITHIN} s of the stop`,
      stale !== null,
      `${last.found.delta?.status} after ${seconds(Date.now() - stoppedAt)}, last seen ${last.found.delta?.lastSeen} UTC`,
    );
    if (stale !== null) {
      say(`      delta showed stale ${seconds(stale)} after the stop (last seen ${last.found.delta?.lastSeen} UTC)`);
      // it registers every minute and is stale after three silent ones, so never sooner than two after the stop
      check("delta not stale sooner than two minutes after the stop", stale >= STALE_NOT_BEFORE * 1000, seconds(stale));
    }
    check("poison stayed healthy meanwhile", poisonStayed && last.found.poison?.status === "healthy", last.found.poison?.status);
    say("      admin status (its probes)");
    const lines = statusProbes();
    check("admin status shows delta stale", lines.some((line) => /^delta\s+stale\b/.test(line)), lines.join(" | "));
    check("admin status shows poison healthy", lines.some((line) => /^poison\s+healthy\b/.test(line)), lines.join(" | "));
    const deltaStale = await asked(tokens.pm, "delta", DELTA_QUESTION);
    unavailable(deltaStale, "delta while its probe is stale and down");
    check(
      "no 'as of' note: nothing was delivered to carry one",
      !/hasn't reported lately/.test(deltaStale.stdout + deltaStale.stderr),
      "an as-of note",
    );
    const poisonStale = await asked(tokens.red, "poison", POISON_QUESTION);
    poisonAnswered(poisonStale, "poison while delta is stale");
    auditHolds("delta went stale");

    phase("3b. the alert on a silent probe");
    {
      // stale is a status; the alert is the operator's signal, after 15 minutes without a word
      const early = statusCheck();
      say(`      admin status --check, ${seconds(Date.now() - stoppedAt)} after the stop → exit ${early.code}`);
      for (const line of early.alerts) say(`        ${line}`);
      check("no alert on delta's probe before 15 minutes", !early.alerts.some((line) => /^probe delta\b/.test(line)), early.alerts.join(" | "));
      let alerted = null;
      let seen = early;
      while (Date.now() - stoppedAt < ALERT_WITHIN * 1000) {
        await wait(15_000);
        seen = statusCheck();
        if (seen.alerts.some((line) => /^probe delta: silent for \d+ minutes, last seen /.test(line))) {
          alerted = Date.now() - stoppedAt;
          break;
        }
      }
      say(`      admin status --check, ${seconds(Date.now() - stoppedAt)} after the stop → exit ${seen.code}`);
      for (const line of seen.alerts) say(`        ${line}`);
      check(`the alert names delta's probe within ${ALERT_WITHIN / 60} minutes of the stop`, alerted !== null, seen.alerts.join(" | "));
      check("admin status --check exits 1 while it holds", seen.code === 1, `exit ${seen.code}`);
      check("no alert on poison's probe, which still reports", !seen.alerts.some((line) => /^probe poison\b/.test(line)), seen.alerts.join(" | "));
      if (alerted !== null) {
        say(`      the alert held ${seconds(alerted)} after the stop`);
        check("not sooner than 15 minutes after the stop", alerted >= (SILENT_AFTER - 60) * 1000, seconds(alerted));
      }
    }

    phase("4. delta's probe started again");
    say("      docker compose start probe-delta");
    compose(["start", "probe-delta"]);
    const startedAt = Date.now();
    let healthy = null;
    let answeredAfter = null;
    while (Date.now() - startedAt < HEALTHY_WITHIN * 1000) {
      last = probes();
      if (last.found.delta?.status === "healthy") {
        healthy = Date.now() - startedAt;
        break;
      }
      await wait(1000);
    }
    say(`      admin probe list, ${seconds(Date.now() - startedAt)} after the start`);
    showList(last.listed);
    check(`delta healthy within ${HEALTHY_WITHIN} s of the start`, healthy !== null, last.found.delta?.status);
    const deltaAfter = await asked(tokens.pm, "delta", DELTA_QUESTION);
    answeredAfter = Date.now() - startedAt;
    answered(deltaAfter, "delta after the start");
    check("delta's answer is the baseline's", deltaAfter.summary?.answer?.text === deltaBefore.summary?.answer?.text, deltaAfter.summary?.answer?.text);
    check(
      "delta's facts are the baseline's",
      JSON.stringify(deltaAfter.summary?.answer?.facts?.map((f) => f.id)) === JSON.stringify(deltaBefore.summary?.answer?.facts?.map((f) => f.id)),
      JSON.stringify(deltaAfter.summary?.answer?.facts?.map((f) => f.id)),
    );
    const poisonAfter = await asked(tokens.red, "poison", POISON_QUESTION);
    poisonAnswered(poisonAfter, "poison after delta's start");
    auditHolds("the start");

    phase("5. the timings, and what the drill printed");
    say(`      delta's questions while its probe was down took ${seconds(deltaDown.took)} and ${seconds(deltaStale.took)}, the CLI's start included`);
    say(
      `      stale ${stale === null ? "never shown" : `${seconds(stale)} after the stop`}; ` +
        `healthy ${healthy === null ? "never" : `${seconds(healthy)} after the start`}, answering ${seconds(answeredAfter)} after it`,
    );
    check("no secret in the CLI's outputs or the streams", leaked(outputs.join("\n")) === 0, "a secret");

    say(failures.length === 0 ? "\nthe drill passed" : `\nthe drill failed: ${failures.length} check(s)\n  ${failures.join("\n  ")}`);
    return failures.length === 0;
  },
  { poison: true },
);

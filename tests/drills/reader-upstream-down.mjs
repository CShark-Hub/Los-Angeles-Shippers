// The drill "the code reader's upstream is down": on a fresh stack with the red team's repository and
// its code reader (`code_model`), it asks an open question that the reader answers, cuts the reader off
// from its model, asks again, makes the model refuse, and puts it back. Expected each time the reader
// can't read: the answer is still delivered, from the project's data alone, with the figure's sentence
// kept, the findings' sentence stripped for citing what was never read, and the note that the code
// couldn't be read; the reader's call has no result, the probe stays healthy with `code_model` and is
// not quarantined, the reader itself still answers its health check, and the fixture's project answers
// as before. After each phase the audit chain must hold. It takes about two minutes.
//
// How the upstream is cut: on this stack the reader's upstream is the replay server, a host inside the
// company's networks, which the reader calls directly (never through its egress proxy) on the probes
// network. Stopping `reader-egress` alone therefore changes nothing for the recorded reader, so the
// drill disconnects the replay container from the stack's probes network: the reader can no longer
// resolve or reach it, while the core still reaches the replay server over `egress`, so the judge, the
// planner and the composer keep their models. Nothing else on the probes network talks to the replay
// server: the probes ask their judge through the core. `reader-egress` is stopped too, as an outside
// upstream would lose it, though for a private upstream the disconnect is what counts. The first read
// after the cut goes into the connection the reader kept open from the baseline, to an address that is
// gone, and waits until the reader gives up (about 30 s, under the probe's 40 and the core's 45); the
// next one opens a new connection, finds no address and fails at once. Both are printed with their
// times. The second way down is an upstream that answers but refuses: a recording whose reader answers 503.
//
// Without `--live` the models are the replay server's recordings, so the drill costs nothing. It runs
// under its own Compose project, port and subnets, so it can run beside the other drills and never
// touches the local stack; the stack goes down with its volumes when the drill ends, unless `--keep`.
//
// `--live` runs it with the real models: Jev and the chat models on OpenRouter, and the reader on
// OpenRouter too, an upstream outside your network that the reader reaches only through `reader-egress`
// (its HTTPS_PROXY). There stopping `reader-egress` is the real cut: the reader finds no proxy and fails
// at once. The second way down is the reader's own daily cap: the reader recreated with
// READER_DAILY_USD at 0.00 refuses with `daily_cap` before it sends anything. The answers are the live
// models' own, so the live run checks what must hold whatever they say (the reader's call read or not,
// the note, the probe's state, the audit, the logs) and prints the wording. It asks four questions
// (five if the reader's return needs a second try), under 10 cents, on a project, port and subnets
// of its own.
//
//   node tests/drills/reader-upstream-down.mjs [--live] [--keep]

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const ARGS = process.argv.slice(2);
if (ARGS.some((arg) => !["--live", "--keep"].includes(arg))) {
  console.error("usage: node tests/drills/reader-upstream-down.mjs [--live] [--keep]");
  process.exit(2);
}

// stack.mjs reads these when it is imported, so they are set first
const DEFAULTS = ARGS.includes("--live")
  ? {
      MERCURY_IT_PROJECT: "mercury-drill-reader-live",
      MERCURY_IT_PORT: "18107",
      MERCURY_IT_PROBES_SUBNET: "172.30.63.0/24",
      MERCURY_IT_EDGE_SUBNET: "172.30.62.0/24",
    }
  : {
      MERCURY_IT_PROJECT: "mercury-drill-reader",
      MERCURY_IT_PORT: "18106",
      MERCURY_IT_PROBES_SUBNET: "172.30.61.0/24",
      MERCURY_IT_EDGE_SUBNET: "172.30.60.0/24",
    };
for (const [name, value] of Object.entries(DEFAULTS)) process.env[name] ??= value;

const { LIVE, admin, ask, composeCall, composeCommand, http, leaked, mask, root, rows, withStack } = await import("../it/stack.mjs");

const recording = (name) => JSON.parse(readFileSync(join(root, "tests/it/replay/recordings/questions", name), "utf8"));
/** The open question the reader answers, asked as `red` on poison. */
const READ = recording("open-invoicing-more-than-one-currency.json");
/** The open question whose recorded upstream refuses with 503. */
const REFUSED = recording("open-invoicing-paid-in-two-currencies.json");
/** The question to delta, the fixture's project, which has no reader: its answer must not move. */
const DELTA_QUESTION = "what was merged this week?";

const FINDING = "From the code, Invoicing looks like it works in a single currency, and none of its tests covers a second one.";
const FIGURE = "Invoicing has 2 tests, and they are failing: the last CI run failed on 2026-09-29.";
const NOT_READ = "The code couldn't be read this time, so this answer rests on the project's data alone.";
const AS_OF = / These figures are as of \d{4}-\d{2}-\d{2} \d{2}:\d{2} UTC\.$/;
/** The plan of an open question: every fact tool, then the reader. */
const TOOLS = ["progress", "tested", "blocked", "health", "changes", "ask_code"];
const READER_CALL = TOOLS.indexOf("ask_code");
/** A refused or unresolvable connection fails at once; a hang takes the reader's own wait for its upstream. */
const FAST = 10_000;
/** The longest a hang may take: the probe's wait for the reader, under the core's 45 s for the call. */
const HANG = 40_000;
/** How long after the upstream is back the baseline answer must return. */
const BACK_WITHIN = 60_000;

const planted = JSON.parse(readFileSync(join(root, "tests/redteam/planted.json"), "utf8"));
const PLANTED = [...Object.values(planted.never_in_a_reply).flat(), ...planted.withheld_titles];

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

/** A command as it ran, whatever its exit code. */
function run(command, args, options = {}) {
  const done = spawnSync(command, args, { encoding: "utf8", timeout: 120_000, maxBuffer: 64 * 1024 * 1024, ...options });
  if (done.error) throw new Error(`${command} ${args[0]} can't be run: ${done.error.message}`);
  return done;
}

/** `docker compose <args>` on this drill's project, as stack.mjs runs it. */
function composeRun(args, env = {}) {
  const call = composeCall(args);
  return run(call.command, call.args, { cwd: call.cwd, env: { ...process.env, ...call.env, ...env } });
}

function compose(args, env = {}) {
  const done = composeRun(args, env);
  if (done.status !== 0) throw new Error(`docker compose ${args.join(" ")} failed: ${mask(done.stderr.trim())}`);
  return done.stdout;
}

function docker(args) {
  const done = run("docker", args);
  if (done.status !== 0) throw new Error(`docker ${args.join(" ")} failed: ${mask(done.stderr.trim())}`);
  return done.stdout;
}

const PROBES_NETWORK = `${process.env.MERCURY_IT_PROJECT}_probes`;

/** The networks a service's container is on. */
function networksOf(container) {
  return Object.keys(JSON.parse(docker(["inspect", "-f", "{{json .NetworkSettings.Networks}}", container])));
}

/** `mercury-probe reach` inside a service's container: whether any HTTP answer came, and its status line. */
function reaches(service, url) {
  const done = composeRun(["exec", "-T", service, "mercury-probe", "reach", url, "--timeout", "5"]);
  return { ok: done.status === 0, line: done.stdout.trim(), error: done.stderr.trim().split("\n").at(-1) || `exit ${done.status}` };
}

/** The code reader's own health check in its container (`mercury-probe healthcheck 7100`): whether /health answered 200. */
function readerAnswers() {
  const done = composeRun(["exec", "-T", "code-reader", "mercury-probe", "healthcheck", "7100"]);
  return { ok: done.status === 0, error: done.stderr.trim().split("\n").at(-1) || `exit ${done.status}` };
}

/** Each probe of `probe list`: project → { status, access }. */
function probes() {
  const listed = admin(["probe", "list"]);
  const found = {};
  for (const line of listed.split("\n")) {
    const m = line.match(/^(\S+)\s+(healthy|stale|quarantined)\s+(\S+)/);
    if (m) found[m[1]] = { status: m[2], access: m[3] };
  }
  return { listed, found };
}

function showList(listed) {
  for (const line of listed.trimEnd().split("\n")) say(`        ${line}`);
}

async function quarantines(adminToken) {
  const page = await http("GET", "/v1/audit?project=poison&limit=500", { headers: { authorization: `Bearer ${adminToken}` } });
  outputs.push(page.text);
  if (page.status !== 200) throw new Error(`GET /v1/audit answered ${page.status}`);
  return (page.json?.items ?? []).filter((row) => row.type === "probe_quarantined").length;
}

/** One question as `mercury ask --json`, with its audit rows. */
async function asked(token, project, question) {
  const started = Date.now();
  const done = ask(token, ["--project", project, question]);
  const took = Date.now() - started;
  outputs.push(done.stdout, done.stderr);
  const found = done.summary?.id ? await rows(token, done.summary.id) : [];
  outputs.push(JSON.stringify(found));
  say(`      mercury ask --project ${project} "${question}" --json → exit ${done.code} in ${seconds(took)}`);
  return { ...done, took, found, events: found.map((r) => r.event) };
}

const rowOfCall = (q, event, call) => q.found.find((r) => r.event === event && r.data?.call === call)?.data;
const judged = (q) => q.found.find((r) => r.event === "judged")?.data;
const text = (q) => q.summary?.answer?.text ?? "";

function delivered(q, label) {
  check(`${label}: exit 0`, q.code === 0, `exit ${q.code} (${q.stderr.trim().split("\n").at(-1)})`);
  check(`${label}: delivered, open, judged by the chat judge`, [q.summary?.status, q.summary?.class, q.summary?.judge_mode].join() === "delivered,open,chat", [q.summary?.status, q.summary?.class, q.summary?.judge_mode].join());
  check(`${label}: the plan holds every fact tool, then ask_code`, JSON.stringify(q.summary?.plan?.map((c) => c.tool)) === JSON.stringify(TOOLS), JSON.stringify(q.summary?.plan?.map((c) => c.tool)));
  check(`${label}: its answer and done rows`, q.events.includes("answer") && q.events.at(-1) === "done", q.events.join(","));
  say(`      answer: ${text(q)}`);
  const j = judged(q);
  say(`      judged: ${j?.sentences ?? "?"} sentence(s), ${j?.stripped ?? "?"} stripped, ${j?.held ?? "?"} held`);
}

/** The reader read: its result with the two findings, both sentences in the answer. */
function read(q, label) {
  delivered(q, label);
  const result = rowOfCall(q, "tool_result", READER_CALL);
  check(`${label}: ask_code's tool_result, two findings, code_model`, result?.items === 2 && result?.access === "code_model", JSON.stringify(result ?? null));
  check(`${label}: the answer is the findings' sentence and the figure's`, text(q).replace(AS_OF, "") === `${FINDING} ${FIGURE}`, text(q));
  check(`${label}: no note on the code`, !text(q).includes(NOT_READ), text(q));
  check(`${label}: nothing stripped`, judged(q)?.stripped === 0, judged(q)?.stripped);
}

/** The reader couldn't read: no result for its call, the figure kept, the findings stripped, the note. */
function unread(q, label, limit = FAST) {
  delivered(q, label);
  say(`      it took ${seconds(q.took)}, the CLI's start included (the question's own duration_ms ${q.summary?.duration_ms})`);
  check(`${label}: no tool_result for the reader's call (${READER_CALL})`, rowOfCall(q, "tool_result", READER_CALL) === undefined, JSON.stringify(rowOfCall(q, "tool_result", READER_CALL)));
  check(`${label}: the five fact tools' results came`, TOOLS.slice(0, READER_CALL).every((_, call) => rowOfCall(q, "tool_result", call)), q.events.join(","));
  check(`${label}: the figure's sentence kept, then the note, then the as-of line`, text(q).replace(AS_OF, "") === `${FIGURE} ${NOT_READ}` && AS_OF.test(text(q)), text(q));
  check(`${label}: the findings' sentence stripped (one of two, not more than half)`, judged(q)?.stripped === 1, judged(q)?.stripped);
  check(
    limit === FAST ? `${label}: it failed fast, not on a timeout (under ${FAST / 1000} s)` : `${label}: the wait ended under the probe's ${HANG / 1000} s`,
    q.took < limit,
    seconds(q.took),
  );
}

function auditHolds(after) {
  const verified = admin(["audit", "verify"]).trim();
  say(`      admin audit verify → ${verified}`);
  check(`audit verify after ${after}`, /^ok: \d+ row\(s\), the chain holds$/.test(verified), verified);
}

/** The poisoned probe still healthy with code_model, and not quarantined since the baseline. */
async function probeStays(label, adminToken, quarantinedBefore) {
  const { listed, found } = probes();
  say("      admin probe list");
  showList(listed);
  check(`${label}: poison healthy with code_model`, found.poison?.status === "healthy" && found.poison?.access === "code_model", JSON.stringify(found.poison ?? null));
  const now = await quarantines(adminToken);
  check(`${label}: no new probe_quarantined row`, now === quarantinedBefore, `${now} against ${quarantinedBefore}`);
}

function readerHealth(label) {
  const health = readerAnswers();
  say(`      code-reader's /health → ${health.ok ? "200" : health.error}`);
  check(`${label}: the code reader's own health check answers`, health.ok, health.error);
}

/** The code reader's lines on each read, from `since` (an RFC 3339 time): status, sizes, cost, time; no text. */
function readerReads(since) {
  return compose(["logs", "--no-color", "--no-log-prefix", "--since", since, "code-reader"])
    .split("\n")
    .filter((line) => line.includes(": read: "));
}

/** The replay server's log lines for the reader, from `since` (an RFC 3339 time). */
function replayReaderLines(since) {
  return compose(["logs", "--no-color", "--no-log-prefix", "--since", since, "replay"])
    .split("\n")
    .filter((line) => line.includes('"model":"replay/reader"'));
}

// ---- the live variant ----------------------------------------------------------------------------

/** The most a live question may take: the core's 45 s for the reader's call, and the models around it. */
const LIVE_QUESTION = 120_000;

/** What a live question ended with, printed: its status, judge, plan, the reader's rows, its answer and cost. */
function liveShown(q, label) {
  const s = q.summary;
  say(`      ${label}: ${s?.status ?? "no summary"}${s?.refusal ? ` (${s.refusal.code})` : ""}${s?.error ? ` (error ${s.error.code})` : ""}, class ${s?.class ?? "?"}, judged by ${s?.judge_mode ?? "?"}, $${(s?.cost_usd ?? 0).toFixed(4)}, duration_ms ${s?.duration_ms ?? "?"}`);
  say(`      plan: ${(s?.plan ?? []).map((c) => c.tool).join(", ") || "none"}`);
  const reader = q.found.filter((r) => r.data?.call === READER_CALL);
  for (const r of reader) say(`      call ${READER_CALL}: ${r.event} ${JSON.stringify(r.data)}`);
  if (reader.length === 0) say(`      call ${READER_CALL}: no row`);
  say(`      events: ${q.events.join(",")}`);
  const j = judged(q);
  if (j) say(`      judged: ${j.sentences ?? "?"} sentence(s), ${j.stripped ?? "?"} stripped, ${j.held ?? "?"} held, p_min ${j.p_min ?? "?"}`);
  say(`      answer: ${text(q) || s?.refusal?.message || s?.error?.message || ""}`);
}

/** The checks every live question must pass, whatever the models wrote. */
function liveCommon(q, label) {
  check(`${label}: the question finished with a summary`, Boolean(q.summary?.id), `exit ${q.code} (${q.stderr.trim().split("\n").at(-1)})`);
  check(`${label}: ask_code in the plan, after the fact tools`, JSON.stringify(q.summary?.plan?.map((c) => c.tool)) === JSON.stringify(TOOLS), JSON.stringify(q.summary?.plan?.map((c) => c.tool)));
  check(`${label}: it ended with a done row`, q.events.at(-1) === "done", q.events.join(","));
  check(`${label}: within ${LIVE_QUESTION / 1000} s`, q.took < LIVE_QUESTION, seconds(q.took));
  if (q.summary?.status !== "delivered") say(`      NOTE  ${label}: the live models ended it ${q.summary?.status}, not delivered`);
  if (q.summary?.judge_mode !== "jev") say(`      NOTE  ${label}: judged by ${q.summary?.judge_mode}, not jev`);
}

/** The reader read: its call has a result. */
function liveRead(q, label) {
  liveShown(q, label);
  liveCommon(q, label);
  const result = rowOfCall(q, "tool_result", READER_CALL);
  check(`${label}: ask_code's tool_result came, code_model`, result !== undefined && result.access === "code_model", JSON.stringify(result ?? null));
  say(`      the reader's findings: ${result?.items ?? "none"}`);
  if (q.summary?.status === "delivered") check(`${label}: no note on the code`, !text(q).includes(NOT_READ), text(q));
}

/** The reader couldn't read: no result for its call, and a delivered answer carries the note. */
function liveUnread(q, label) {
  liveShown(q, label);
  liveCommon(q, label);
  say(`      it took ${seconds(q.took)}, the CLI's start and the live models included`);
  check(`${label}: no tool_result for the reader's call (${READER_CALL})`, rowOfCall(q, "tool_result", READER_CALL) === undefined, JSON.stringify(rowOfCall(q, "tool_result", READER_CALL)));
  check(`${label}: the five fact tools' results came`, TOOLS.slice(0, READER_CALL).every((_, call) => rowOfCall(q, "tool_result", call)), q.events.join(","));
  if (q.summary?.status === "delivered") {
    check(`${label}: delivered from the facts with the note that the code couldn't be read`, text(q).includes(NOT_READ), text(q));
  } else {
    say(`      NOTE  ${label}: not delivered, so no note to look for`);
  }
}

/** The reader's read lines since `since`, printed; the time (in seconds) each one gives. */
function liveReaderLines(since, label) {
  const lines = readerReads(since);
  for (const line of lines) say(`        code-reader: ${line}`);
  check(`${label}: the reader logged one read`, lines.length === 1, `${lines.length} line(s)`);
  return lines;
}

const readTime = (line) => Number(line.match(/, (\d+(?:\.\d+)?) s(?:,|$)/)?.[1] ?? NaN);

/** reader-egress's log lines since `since` that name the upstream host. */
function egressUpstreamLines(since) {
  return compose(["logs", "--no-color", "--no-log-prefix", "--since", since, "reader-egress"])
    .split("\n")
    .filter((line) => line.includes("openrouter.ai"));
}

/** Recreates only the code reader, with READER_DAILY_USD at `cap` (the overlay's default when null), and waits for it. */
async function recreateReader(cap) {
  const env = cap === null ? {} : { MERCURY_IT_READER_DAILY_USD: cap };
  say(`      ${cap === null ? "" : `MERCURY_IT_READER_DAILY_USD=${cap} `}docker compose up -d --no-build --no-deps --force-recreate code-reader`);
  const since = new Date(Date.now() - 1000).toISOString();
  compose(["up", "-d", "--no-build", "--no-deps", "--force-recreate", "code-reader"], env);
  const end = Date.now() + 60_000;
  while (!readerAnswers().ok) {
    if (Date.now() > end) throw new Error("the code reader didn't answer its health check within 60 s of its recreation");
    await wait(1000);
  }
  const started = compose(["logs", "--no-color", "--no-log-prefix", "--since", since, "code-reader"])
    .split("\n")
    .find((line) => line.includes("a cap of $"));
  say(`        code-reader: ${started ?? "no start line"}`);
  return started ?? "";
}

async function liveDrill(tokens) {
  say(`the drill's stack (live: Jev, the chat models and the reader on OpenRouter): ${composeCommand}`);
  const costs = [];
  const live = async () => {
    const q = await asked(tokens.red, "poison", READ.question);
    costs.push(q.summary?.cost_usd ?? 0);
    return q;
  };

  phase("1. baseline: the reader reads, through reader-egress");
  const quarantinedBefore = await quarantines(tokens.admin);
  say(`      probe_quarantined rows for poison so far: ${quarantinedBefore}`);
  {
    const { listed, found } = probes();
    say("      admin probe list");
    showList(listed);
    check("poison healthy with code_model", found.poison?.status === "healthy" && found.poison?.access === "code_model", JSON.stringify(found.poison ?? null));
  }
  const baseAt = new Date().toISOString();
  const baseline = await live();
  liveRead(baseline, "the baseline");
  liveReaderLines(baseAt, "the baseline");
  {
    const via = egressUpstreamLines(baseAt);
    for (const line of via) say(`        reader-egress: ${line}`);
    check("the baseline: the reader's request went through reader-egress", via.length > 0, "no line naming the upstream");
  }
  readerHealth("the baseline");
  auditHolds("the baseline");

  phase("2. the upstream down: reader-egress stopped, the reader's only way out");
  say("      docker compose stop reader-egress");
  compose(["stop", "reader-egress"]);
  const cutAt = new Date().toISOString();
  {
    const tried = reaches("code-reader", "http://reader-egress:8888/");
    say(`      from code-reader, GET http://reader-egress:8888/ → ${tried.ok ? "answered" : tried.error}`);
    check("the code reader can't reach its proxy", !tried.ok, tried.line);
  }
  const down = await live();
  liveUnread(down, "upstream down");
  {
    const lines = liveReaderLines(cutAt, "upstream down");
    const took = readTime(lines[0] ?? "");
    check(`upstream down: the reader's read failed, not 200`, lines.length > 0 && !lines.some((line) => line.includes(": read: 200")), lines.join(" | "));
    check(`upstream down: the refused proxy failed at once (the reader's own time under ${FAST / 1000} s)`, took < FAST / 1000, `${took} s`);
  }
  await probeStays("upstream down", tokens.admin, quarantinedBefore);
  readerHealth("upstream down");
  auditHolds("the upstream went down");

  phase("3. the daily cap: reader-egress back, the reader recreated with READER_DAILY_USD at 0.00");
  say("      docker compose start reader-egress");
  compose(["start", "reader-egress"]);
  const capStart = await recreateReader("0.00");
  check("the reader started with a cap of $0.00 a day", capStart.includes("a cap of $0.00 a day"), capStart);
  const capAt = new Date().toISOString();
  const capped = await live();
  liveUnread(capped, "daily cap");
  {
    const lines = liveReaderLines(capAt, "daily cap");
    check("daily cap: the reader's line names daily_cap", lines.length > 0 && lines.every((line) => line.includes("daily_cap")), lines.join(" | "));
    const sent = egressUpstreamLines(capAt);
    for (const line of sent) say(`        reader-egress: ${line}`);
    check("daily cap: nothing was sent upstream (reader-egress logged no request to it)", sent.length === 0, sent.join(" | "));
  }
  await probeStays("daily cap", tokens.admin, quarantinedBefore);
  readerHealth("daily cap");
  auditHolds("the daily cap");

  phase("4. back: the reader recreated with the normal cap");
  const normalStart = await recreateReader(null);
  check("the reader started with its normal cap of $1.00 a day", normalStart.includes("a cap of $1.00 a day"), normalStart);
  const backAt = Date.now();
  const backSince = new Date().toISOString();
  let again = null;
  // live questions cost money: two tries at most within the minute
  for (let attempt = 1; attempt <= 2 && Date.now() - backAt < BACK_WITHIN; attempt += 1) {
    again = await live();
    if (rowOfCall(again, "tool_result", READER_CALL)) break;
    await wait(3000);
  }
  const backIn = Date.now() - backAt;
  say(`      the reader read again ${seconds(backIn)} after it came back`);
  check(`the reader's result within ${BACK_WITHIN / 1000} s of its return (the live question's own time on top)`, rowOfCall(again, "tool_result", READER_CALL) !== undefined && backIn <= BACK_WITHIN + LIVE_QUESTION, seconds(backIn));
  liveRead(again, "back");
  for (const line of readerReads(backSince)) say(`        code-reader: ${line}`);
  await probeStays("back", tokens.admin, quarantinedBefore);
  auditHolds("the reader came back");

  phase("5. what the drill printed and what the reader logged");
  const readerLogs = compose(["logs", "--no-color", "code-reader", "reader-egress"]);
  if (!readerLogs.trim()) throw new Error("docker compose logs returned nothing for code-reader and reader-egress");
  say(`      code-reader and reader-egress logged ${readerLogs.trim().split("\n").length} line(s)`);
  check("no question in the logs of code-reader and reader-egress", !readerLogs.includes(READ.question), READ.question);
  const plantedFound = PLANTED.filter((p) => readerLogs.includes(p));
  check("no planted text in the logs of code-reader and reader-egress", plantedFound.length === 0, `${plantedFound.length} planted text(s)`);
  check("no secret in the CLI's outputs, the streams or those logs", leaked([...outputs, readerLogs].join("\n")) === 0, "a secret");
  const total = costs.reduce((sum, cost) => sum + cost, 0);
  say(`      ${costs.length} live question(s), $${total.toFixed(4)} as the core counted them`);

  say(failures.length === 0 ? "\nthe drill passed" : `\nthe drill failed: ${failures.length} check(s)\n  ${failures.join("\n  ")}`);
  return failures.length === 0;
}

await withStack(
  async (tokens) => {
    if (LIVE) return liveDrill(tokens);
    say(`the drill's stack: ${composeCommand}`);
    const replay = compose(["ps", "-q", "replay"]).trim();
    if (!replay) throw new Error("the replay server's container isn't running");

    phase("1. baseline: the reader reads");
    const quarantinedBefore = await quarantines(tokens.admin);
    say(`      probe_quarantined rows for poison so far: ${quarantinedBefore}`);
    {
      const { listed, found } = probes();
      say("      admin probe list");
      showList(listed);
      check("poison healthy with code_model", found.poison?.status === "healthy" && found.poison?.access === "code_model", JSON.stringify(found.poison ?? null));
    }
    say(`      the replay server is on ${networksOf(replay).join(", ")}`);
    const baseline = await asked(tokens.red, "poison", READ.question);
    read(baseline, "the baseline");
    const deltaBefore = await asked(tokens.pm, "delta", DELTA_QUESTION);
    check("delta's baseline: delivered", deltaBefore.code === 0 && deltaBefore.summary?.status === "delivered", deltaBefore.summary?.status);
    readerHealth("the baseline");
    auditHolds("the baseline");

    phase("2. the upstream down: the replay server cut off from the probes network, reader-egress stopped");
    say("      docker compose stop reader-egress");
    compose(["stop", "reader-egress"]);
    say(`      docker network disconnect ${PROBES_NETWORK} <replay>`);
    docker(["network", "disconnect", PROBES_NETWORK, replay]);
    const cutAt = new Date().toISOString();
    say(`      the replay server is on ${networksOf(replay).join(", ")}`);
    check("the replay server is off the probes network", !networksOf(replay).includes(PROBES_NETWORK), networksOf(replay).join(","));
    {
      const tried = reaches("code-reader", "http://replay:9000/health");
      say(`      from code-reader, GET http://replay:9000/health → ${tried.ok ? "answered" : tried.error}`);
      check("the code reader can't reach the replay server", !tried.ok, tried.line);
    }
    // the reader still holds the baseline's open connection to the replay server's old address: the first
    // read after the cut is written into it and waits until the reader gives up; that connection is then
    // dropped, and the next read finds no address and fails at once
    const hung = await asked(tokens.red, "poison", READ.question);
    unread(hung, "upstream down, first read on the reader's kept connection", HANG);
    const down = await asked(tokens.red, "poison", READ.question);
    unread(down, "upstream down, a new connection");
    for (const line of readerReads(cutAt)) say(`        code-reader: ${line}`);
    check("upstream down: no reader request reached the replay server", replayReaderLines(cutAt).length === 0, replayReaderLines(cutAt).join(" | "));
    await probeStays("upstream down", tokens.admin, quarantinedBefore);
    readerHealth("upstream down");
    const deltaDown = await asked(tokens.pm, "delta", DELTA_QUESTION);
    check("delta's answer is the baseline's", deltaDown.code === 0 && text(deltaDown) === text(deltaBefore), text(deltaDown));
    auditHolds("the upstream went down");

    phase("3. the upstream refusing: reconnected, its recording answers 503");
    say("      docker compose start reader-egress");
    compose(["start", "reader-egress"]);
    say(`      docker network connect --alias replay ${PROBES_NETWORK} <replay>`);
    docker(["network", "connect", "--alias", "replay", PROBES_NETWORK, replay]);
    const backAt = Date.now();
    const refusedSince = new Date().toISOString();
    {
      const tried = reaches("code-reader", "http://replay:9000/health");
      check("the code reader reaches the replay server again", tried.ok, tried.error);
    }
    const refused = await asked(tokens.red, "poison", REFUSED.question);
    unread(refused, "upstream refusing");
    {
      const lines = replayReaderLines(refusedSince);
      for (const line of lines) say(`        replay: ${line}`);
      check(
        "upstream refusing: the replay server answered the reader 503 from the recording",
        lines.some((line) => line.includes('"status":503') && line.includes("open-invoicing-paid-in-two-currencies.json")),
        lines.join(" | "),
      );
    }
    for (const line of readerReads(refusedSince)) say(`        code-reader: ${line}`);
    await probeStays("upstream refusing", tokens.admin, quarantinedBefore);
    readerHealth("upstream refusing");
    const deltaRefusing = await asked(tokens.pm, "delta", DELTA_QUESTION);
    check("delta's answer is the baseline's", deltaRefusing.code === 0 && text(deltaRefusing) === text(deltaBefore), text(deltaRefusing));
    auditHolds("the upstream refused");

    phase("4. back: the baseline answer again");
    let again = null;
    while (Date.now() - backAt < BACK_WITHIN) {
      again = await asked(tokens.red, "poison", READ.question);
      if (rowOfCall(again, "tool_result", READER_CALL) && text(again).replace(AS_OF, "") === `${FINDING} ${FIGURE}`) break;
      await wait(3000);
    }
    const backIn = Date.now() - backAt;
    say(`      the reader read again ${seconds(backIn)} after the upstream came back`);
    check(`the baseline answer within ${BACK_WITHIN / 1000} s of the upstream's return`, backIn <= BACK_WITHIN + FAST, seconds(backIn));
    read(again, "back");
    check("back: the answer is the baseline's, findings cited", text(again).replace(AS_OF, "") === text(baseline).replace(AS_OF, ""), text(again));
    await probeStays("back", tokens.admin, quarantinedBefore);
    auditHolds("the upstream came back");

    phase("5. what the drill printed and what the reader logged");
    const readerLogs = compose(["logs", "--no-color", "code-reader", "reader-egress"]);
    if (!readerLogs.trim()) throw new Error("docker compose logs returned nothing for code-reader and reader-egress");
    say(`      code-reader and reader-egress logged ${readerLogs.trim().split("\n").length} line(s)`);
    const questions = [READ.question, REFUSED.question].filter((q) => readerLogs.includes(q));
    check("no question in the logs of code-reader and reader-egress", questions.length === 0, questions.join(" | "));
    const plantedFound = PLANTED.filter((p) => readerLogs.includes(p));
    check("no planted text in the logs of code-reader and reader-egress", plantedFound.length === 0, `${plantedFound.length} planted text(s)`);
    check("no secret in the CLI's outputs, the streams or those logs", leaked([...outputs, readerLogs].join("\n")) === 0, "a secret");

    say(failures.length === 0 ? "\nthe drill passed" : `\nthe drill failed: ${failures.length} check(s)\n  ${failures.join("\n  ")}`);
    return failures.length === 0;
  },
  { poison: true },
);

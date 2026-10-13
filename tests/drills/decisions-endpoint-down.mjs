// Drill: the judge's Decisions endpoint is down and nothing else is. Mercury must keep answering
// with the chat judge in its place, on thresholds 0.2 stricter than Jev's, say so on the timeline
// (`judge: chat`), in the audit (`judge_mode: chat` in the `done` row) and in `mctl status`, and go
// back to Jev once the endpoint is back.
//
// How it breaks it: on a stack of its own, the run's copy of models.yaml (target/<project>/config,
// mounted read-only into the core) gets a Decisions address that refuses every connection, and only
// the core is restarted. The chat endpoint, the planner and the composer are untouched, so the cut
// is the Decisions path alone. Putting the address back and restarting the core restores it.
//
// The phases, each ending with `mctl audit verify`:
//   0. the run's daily caps lowered to $0.10 (the core's own budget bounds the drill), a restart
//   1. a baseline question: Jev judges
//   2. the Decisions address refused, a restart; the same question: the chat judge judges every
//      stage, the answer is still checked before it is shown, a code question is still refused,
//      and the core's log names the failed connection, not a missing key
//   2b. the circuit breaker: Jev was asked three times and then skipped; a minute after it opened
//      the next question asks Jev again, with no restart: the breaker closed by itself
//   2c. the alert: `mctl status --check` is silent until the first degraded question is 5 minutes
//      old, then exits 1 and names the judge; `mctl status` lists it; the core's log says it once
//   3. the address put back, a restart; the same question: Jev judges again, and the alert is gone
//
//   node tests/drills/decisions-endpoint-down.mjs --live      the real models and the key in
//                                                            deploy/compose/secrets/openrouter_api_key;
//                                                            four questions, a few cents
//   node tests/drills/decisions-endpoint-down.mjs --dry-run   the replay server, $0: every step and
//                                                            check except those that need Jev
//   --keep                                                   leave the stack up afterwards
//
// The dry run gives the core Jev on openrouter without a key (so Jev fails at once) and the chat
// judge, the planner and the composer on the replay server, so the chain falls from Jev to the chat
// judge in every phase. It can't show Jev answering, nor that the refused address (and not the
// missing key) is what stopped it: those are the live run's.

import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { request } from "node:http";
import { join, relative, sep } from "node:path";

// the stack of this drill, beside the others; stack.mjs reads these when it is imported
const DEFAULTS = {
  MERCURY_IT_PROJECT: "mercury-drill-decisions",
  MERCURY_IT_PORT: "18101",
  MERCURY_IT_PROBES_SUBNET: "172.30.51.0/24",
  MERCURY_IT_EDGE_SUBNET: "172.30.50.0/24",
};
for (const [name, value] of Object.entries(DEFAULTS)) {
  if (!process.env[name]) process.env[name] = value;
}

const args = process.argv.slice(2);
const DRY = args.includes("--dry-run");
const unknown = args.filter((arg) => !["--live", "--dry-run", "--keep"].includes(arg));
if (unknown.length > 0 || DRY === args.includes("--live")) {
  console.error(
    [
      "usage: node tests/drills/decisions-endpoint-down.mjs --live | --dry-run [--keep]",
      "  --live     the point of the drill: Jev, then the chat judge while the Decisions endpoint is cut",
      "             off, then Jev again (the key in deploy/compose/secrets/openrouter_api_key; a few cents)",
      "  --dry-run  the same steps on the replay server, for $0; the checks that need Jev are skipped",
    ].join("\n"),
  );
  process.exit(2);
}
const PROJECT = process.env.MERCURY_IT_PROJECT;
// a drill takes its stack down with its volumes: never the local stack's, never the integration runs'
if (PROJECT === "mercury" || PROJECT === "mercury-it") {
  console.error(`a drill runs on a stack of its own; MERCURY_IT_PROJECT=${PROJECT} is not one`);
  process.exit(2);
}

const stack = await import("../it/stack.mjs");
const { LIVE, URL: BASE, admin, allLogs, ask, composeCommand, leaked, mask, root, rows, waitForProbe, withStack } =
  stack;

const work = join(root, "target", PROJECT);
const composeDir = join(root, "deploy/compose");
const MODELS = join(work, "config/models.yaml");
/** An address that refuses every connection: nothing listens on port 9 inside the core's container. */
const REFUSED_URL = "https://127.0.0.1:9/api/alpha/decisions";
/** The drill's daily caps: what the operator allows it to spend. */
const CAP_USD = 0.1;

const recorded = (name) =>
  JSON.parse(readFileSync(join(root, "tests/it/replay/recordings/questions", name), "utf8")).question;
// live: a question the fixture answers with facts, so the composer writes; dry: one the replay server
// has recordings for, so the chat judge answers every stage
const QUESTION = LIVE
  ? "What is blocked right now, and what is it waiting on?"
  : recorded("what-was-merged-this-week.json");
const CODE_QUESTION = recorded("show-me-the-export-code.json");

// --- what the drill prints -----------------------------------------------------------------------

let failures = 0;
const outputs = [];

function check(what, pass, seen) {
  if (!pass) failures += 1;
  const detail = seen === undefined ? "" : `: ${typeof seen === "string" ? seen : JSON.stringify(seen)}`;
  console.log(mask(`${pass ? "ok  " : "FAIL"} ${what}${detail}`));
}

function skip(what, why) {
  console.log(`skip ${what} (${why})`);
}

function show(text) {
  for (const line of String(text).replace(/\r\n/g, "\n").trimEnd().split("\n")) console.log(mask(`     ${line}`));
}

function phase(title) {
  console.log(`\n== ${title}`);
}

// --- the stack -------------------------------------------------------------------------------------

/** `docker compose` on this drill's project, as stack.mjs calls it. */
function compose(composeArgs, seconds = 120) {
  const run = spawnSync("docker", [...composeCommand.split(" ").slice(1), ...composeArgs], {
    cwd: composeDir,
    encoding: "utf8",
    env: {
      ...process.env,
      MERCURY_IT_WORK: relative(composeDir, work).split(sep).join("/"),
    },
    timeout: seconds * 1000,
    maxBuffer: 64 * 1024 * 1024,
  });
  if (run.error) throw new Error(`docker compose ${composeArgs[0]} can't be run: ${run.error.message}`);
  return run;
}

const pause = (ms) => new Promise((done) => setTimeout(done, ms));

function health() {
  return new Promise((done) => {
    const req = request(`${BASE}/v1/health`, { method: "GET" }, (res) => {
      res.resume();
      res.on("end", () => done(res.statusCode));
    });
    req.on("error", () => done(0));
    req.setTimeout(5000, () => req.destroy());
    req.end();
  });
}

/** Restarts the core alone and waits for its health through Caddy and the fixture's probe. */
async function restartCore() {
  const started = Date.now();
  const run = compose(["restart", "core"]);
  if (run.status !== 0) throw new Error(`docker compose restart core failed: ${mask(run.stderr.trim())}`);
  const end = Date.now() + 120_000;
  while ((await health()) !== 200) {
    if (Date.now() > end) throw new Error("the core didn't answer /v1/health within 120 s of its restart");
    await pause(500);
  }
  const healthy = await waitForProbe("delta", "healthy", 120);
  const took = `${((Date.now() - started) / 1000).toFixed(1)} s`;
  check("the core restarted, healthy, and the delta probe healthy", healthy, took);
  if (!healthy) throw new Error("the delta probe didn't become healthy after the core's restart");
}

/**
 * The core's log so far, as lines, each starting with the time the container logged it; a phase
 * reads the lines after the count it started at.
 */
function coreLog() {
  const run = compose(["logs", "--no-color", "--no-log-prefix", "--timestamps", "core"], 60);
  if (run.status !== 0) throw new Error(`docker compose logs failed: ${mask(run.stderr.trim())}`);
  return run.stdout.replace(/\r\n/g, "\n").split("\n");
}

// --- models.yaml -----------------------------------------------------------------------------------

const read = (path) => readFileSync(path, "utf8").replace(/\r\n/g, "\n");
const decisionsUrl = (text) => text.match(/^\s*decisions_url:\s*(\S+)/m)?.[1] ?? null;

/** For the dry run: Jev on openrouter as config-examples has it, and everything else on the replay server. */
function dryRunModels() {
  const examples = read(join(root, "config-examples/models.yaml"));
  const replay = read(join(root, "tests/it/config/models.yaml"));
  const take = (text, pattern, what) => {
    const found = text.match(pattern)?.[0];
    if (!found) throw new Error(`no ${what} to build the dry run's models.yaml from`);
    return found.replace(/\n*$/, "\n");
  };
  return [
    "# The decisions drill's dry run: Jev on openrouter without a key (it fails at once), the chat",
    "# judge, the planner and the composer on the replay server.",
    "backends:",
    take(examples, /^ {2}openrouter:\n(?: {4}.*\n)+/m, "openrouter backend in config-examples/models.yaml"),
    take(replay, /^ {2}openai_compatible:\n(?: {4}.*\n)+/m, "replay backend in tests/it/config/models.yaml"),
    "roles:",
    take(examples, /^ {2}judge: .*$/m, "Jev judge in config-examples/models.yaml"),
    take(replay, /^ {2}judge: .*$/m, "replay judge").replace(/^ {2}judge: /, "  judge_fallback: "),
    take(replay, /^ {2}planner: .*$/m, "replay planner"),
    take(replay, /^ {2}composer: .*$/m, "replay composer"),
    take(replay, /^budgets:\n(?: {2}.*\n?)+/m, "budgets in tests/it/config/models.yaml"),
  ].join("\n").replace(/\n{2,}/g, "\n");
}

/** Writes models.yaml and has the core check the directory it will start from; a bad file is put back. */
function writeModels(text, what) {
  const before = read(MODELS);
  writeFileSync(MODELS, text);
  let report;
  try {
    report = admin(["config", "check"]);
  } catch (err) {
    writeFileSync(MODELS, before);
    throw new Error(`${what}: the configuration check failed, so models.yaml was put back: ${err.message}`);
  }
  const fine = /^ok: 4 files in \/config, no problem$/m.test(report);
  check(`${what}: the core's configuration check`, fine, report.trim().split("\n").at(-1));
  if (!fine) {
    writeFileSync(MODELS, before);
    throw new Error(`${what}: the configuration check found a problem, so models.yaml was put back`);
  }
}

/** Checks that the core's container reads `expected` as the Decisions address. */
function checkDecisionsInCore(what, expected) {
  const run = compose(["exec", "-T", "core", "grep", "-h", "decisions_url", "/config/models.yaml"], 60);
  const seen = run.status === 0 ? decisionsUrl(run.stdout) : null;
  check(what, seen === expected, seen ?? `can't be read in the core: ${run.stderr.trim()}`);
}

// --- the checks' catalogue, to see the thresholds each verdict was given -------------------------

/** The catalogue of the run's checks.yaml: id → { type, polarity, thresholds }. */
function catalogue() {
  const checks = {};
  for (const block of read(join(work, "config/checks.yaml")).split(/^- id: /m).slice(1)) {
    const id = block.split("\n")[0].trim();
    const thresholds = {};
    const t = block.match(/^\s+thresholds:\s*\{([^}]*)\}/m)?.[1] ?? "";
    for (const pair of t.split(",")) {
      const [key, value] = pair.split(":").map((s) => s.trim());
      if (key && value !== undefined) thresholds[key] = Number(value);
    }
    checks[id] = {
      type: block.match(/^\s+type:\s*(\w+)/m)?.[1],
      polarity: block.match(/^\s+polarity:\s*(\w+)/m)?.[1],
      thresholds,
    };
  }
  return checks;
}

// How the judge crate tightens a check for a judge that isn't Jev: 0.2 toward refusing, never past
// 0.05 or 0.95 (crates/mercury-judge, the catalogue's `thresholds`).
const TIGHTEN_BY = 0.2;
const FLOOR = 0.05;
const CEILING = 0.95;

function thresholdsFor(check, mode) {
  const t = check.thresholds;
  if (mode === "jev") return t;
  const shift = (r, by) =>
    r === undefined ? undefined : by < 0 ? Math.max(r + by, Math.min(FLOOR, r)) : Math.min(r + by, Math.max(CEILING, r));
  if (check.polarity === "risk") {
    return { ...t, block_at: shift(t.block_at, -TIGHTEN_BY), review_at: shift(t.review_at, -TIGHTEN_BY) };
  }
  return {
    ...t,
    pass_at: shift(t.pass_at, TIGHTEN_BY),
    block_at: shift(t.block_at, TIGHTEN_BY),
    clarify_below: shift(t.clarify_below, TIGHTEN_BY),
  };
}

/** A shown probability is rounded to four places: one this close to a threshold isn't judged here. */
const EDGE = 1e-4;

/** The verdict a `noul` check's probability gets under a mode's thresholds; `null` at an edge. */
function verdictFor(check, p, mode) {
  const t = thresholdsFor(check, mode);
  if (Object.values(t).some((v) => v !== undefined && Math.abs(p - v) < EDGE)) return null;
  if (check.polarity === "risk") {
    if (p >= (t.block_at ?? 0)) return "block";
    if (t.review_at !== undefined && p >= t.review_at) return "review";
    return "pass";
  }
  if (p >= (t.pass_at ?? Infinity)) return "pass";
  if (p <= (t.block_at ?? Infinity)) return "block";
  return "review";
}

// --- one question -----------------------------------------------------------------------------------

const row = (found, event) => found.find((r) => r.event === event)?.data;

/** Asks as `token` and reads the question's audit rows back; prints the timeline. */
async function askOnce(token, question) {
  const run = ask(token, ["--project", "delta", question]);
  outputs.push(run.stdout, run.stderr);
  const stderr = run.stderr.replace(/\r\n/g, "\n");
  console.log(`  mercury ask --project delta "${question}"`);
  show(stderr);
  const found = run.summary?.id ? await rows(token, run.summary.id) : [];
  outputs.push(JSON.stringify(found));
  return { ...run, stderr, rows: found };
}

/** Every judge's check of the input and tool stages: by `mode`, on `mode`'s thresholds. */
function checkJudged(checks, found, mode) {
  const results = [
    ...(row(found, "classified")?.checks ?? []),
    ...found.filter((r) => r.event === "tool_checked").flatMap((r) => r.data?.checks ?? []),
  ];
  const judged = results.filter((c) => checks[c.id]?.type !== "rule");
  check(
    `every judge's check of the input and tool stages by ${mode}`,
    judged.length > 0 && judged.every((c) => c.source === mode),
    judged.map((c) => `${c.id} ${c.verdict} (${c.source}${c.p === undefined ? "" : `, p ${c.p}`})`).join(" · "),
  );
  let compared = 0;
  const wrong = [];
  const looser = [];
  for (const c of judged) {
    const known = checks[c.id];
    if (!known || known.type !== "noul" || typeof c.p !== "number") continue;
    const expected = verdictFor(known, c.p, mode);
    if (expected === null) continue;
    compared += 1;
    if (expected !== c.verdict) wrong.push(`${c.id}: p ${c.p} got ${c.verdict}, ${mode}'s thresholds give ${expected}`);
    if (mode !== "jev" && verdictFor(known, c.p, "jev") !== c.verdict) looser.push(c.id);
  }
  const stricter = looser.length > 0 ? `; Jev's thresholds would have let more through for ${looser.join(", ")}` : "";
  check(
    `each verdict as ${mode}'s thresholds give it${mode === "jev" ? "" : " (0.2 stricter than Jev's)"}`,
    compared > 0 && wrong.length === 0,
    wrong.length > 0 ? wrong.join("; ") : `${compared} check(s) with a probability compared${stricter}`,
  );
  const classified = row(found, "classified");
  const classCheck = checks.class;
  if (classified && classCheck?.thresholds.clarify_below !== undefined) {
    const needed = thresholdsFor(classCheck, mode).clarify_below;
    check(
      `the class passed ${mode}'s threshold`,
      classified.p >= needed - EDGE,
      `${classified.class}, p ${classified.p} against ${needed.toFixed(2)}`,
    );
  }
}

/** A delivered question, judged by `mode`, its answer checked before it was shown. */
function checkDelivered(checks, run, mode) {
  const { summary, rows: found } = run;
  check("delivered, exit 0", run.code === 0 && summary?.status === "delivered", `exit ${run.code}, ${summary?.status}`);
  if (!summary) return;
  const events = found.map((r) => r.event);
  check(
    "the answer was judged before it was shown",
    events.includes("judged") && events.indexOf("judged") < events.indexOf("answer") && events.at(-1) === "done",
    events.join(" → "),
  );
  const done = row(found, "done");
  check(`judge_mode: ${mode} in the done row`, done?.judge_mode === mode, done);
  check(`judge_mode: ${mode} in the CLI's JSON`, summary.judge_mode === mode, summary.judge_mode);
  check(
    `the timeline's done line says judge: ${mode} · composer: model`,
    new RegExp(`^done .* · judge: ${mode} · composer: model · audit \\d+$`, "m").test(run.stderr),
    run.stderr.match(/^done .*$/m)?.[0] ?? "no done line",
  );
  checkJudged(checks, found, mode);
  check("the output stage judged the answer's sentences", row(found, "judged")?.sentences > 0, row(found, "judged"));
  if (summary.answer?.text) show(`answer: ${summary.answer.text.slice(0, 300)}`);
}

// --- mctl status ------------------------------------------------------------------------------------

/** The endings the drill's questions left, by judge: what `mctl status` must count. */
const endings = { jev: 0, chat: 0, rules: 0, none: 0 };

function ended(found) {
  const last = [...found].reverse().find((r) => ["done", "refused", "clarify", "error"].includes(r.event));
  endings[last?.data?.judge_mode ?? "none"] += 1;
}

/** What the logged line says it was logged at, in ms. */
const timeOf = (line) => Date.parse(line.slice(0, line.indexOf(" ")));

/** `mctl status --check`: its exit code and its lines (the command fails while an alert holds). */
function statusCheck() {
  const run = compose(["exec", "-T", "core", "mercury-core", "admin", "status", "--check"], 60);
  outputs.push(run.stdout, run.stderr);
  const lines = run.stdout.replace(/\r\n/g, "\n").split("\n").filter(Boolean);
  return { code: run.status, lines, judge: lines.find((line) => line.startsWith("judge: ")), stderr: run.stderr.trim() };
}

function status() {
  const text = admin(["status"]).replace(/\r\n/g, "\n");
  console.log("  mctl status (the alerts, the roles and the judge modes):");
  const shown = /^(?:(?:ROLE|judge|judge_fallback|MODE|jev|chat|rules|none)\s|Judge modes|Degraded|Alerts|No alert|judge: |spend: |probe \S+: )/;
  show(
    text
      .split("\n")
      .filter((line) => shown.test(line))
      .join("\n"),
  );
  const counted = { jev: 0, chat: 0, rules: 0, none: 0 };
  for (const [, mode, n] of text.matchAll(/^(jev|chat|rules|none)\s+(\d+)\s+\d+%/gm)) counted[mode] = Number(n);
  check(
    "mctl status counts the drill's questions under the judge each had",
    JSON.stringify(counted) === JSON.stringify(endings),
    counted,
  );
  check(
    "mctl status still lists Jev as the judge and the chat model as its fallback",
    /^judge\s+openrouter\s+jev\s/m.test(text) && /^judge_fallback\s+\S+\s+chat\s/m.test(text),
    text.match(/^judge\s.*$/m)?.[0] ?? "no judge line",
  );
}

function verify() {
  const verified = admin(["audit", "verify"]).trim();
  check("mctl audit verify", /^ok: \d+ row\(s\), the chain holds$/.test(verified), verified);
}

// --- the drill --------------------------------------------------------------------------------------

const how = LIVE ? "live: Jev and the chat models on OpenRouter" : "a dry run on the replay server ($0)";
console.log(`the decisions drill, ${how}, on ${PROJECT} at ${BASE}`);

await withStack(async (tokens) => {
  const checks = catalogue();
  const token = tokens.pm;
  const expected = (mode) => (LIVE ? mode : "chat");
  const original = read(MODELS);
  let restored = original;
  let spent = 0;
  const asked = (run) => {
    spent += run.summary?.cost_usd ?? 0;
    ended(run.rows);
  };
  try {
    phase("0. the drill's caps, and the core restarted on them");
    const cap = CAP_USD.toFixed(2);
    const base = (LIVE ? original : dryRunModels())
      .replace(/per_user_daily_usd: [\d.]+/, `per_user_daily_usd: ${cap}`)
      .replace(/global_daily_usd: [\d.]+/, `global_daily_usd: ${cap}`);
    if (!base.includes(`per_user_daily_usd: ${cap}`) || !base.includes(`global_daily_usd: ${cap}`)) {
      throw new Error("models.yaml has no daily caps to lower");
    }
    if (!decisionsUrl(base)) throw new Error("models.yaml has no decisions_url");
    writeModels(base, `daily caps of $${cap}${LIVE ? "" : ", Jev without a key and the replay server"}`);
    restored = base;
    await restartCore();
    const normal = decisionsUrl(base);
    checkDecisionsInCore("the core reads the normal Decisions address", normal);

    phase("1. baseline: the Decisions endpoint up");
    if (!LIVE) skip("Jev judges", "needs --live; without a key Jev fails at once and the chat judge answers");
    const baseline = await askOnce(token, QUESTION);
    asked(baseline);
    // when the judge's degraded run began: the dry run's baseline is already the chat judge's
    let degradedAt = LIVE ? 0 : Date.now();
    checkDelivered(checks, baseline, expected("jev"));
    status();
    verify();

    phase(`2. the Decisions endpoint down: ${normal} → ${REFUSED_URL}`);
    const blocked = base.replace(/^(\s*decisions_url:\s*)\S+/m, `$1${REFUSED_URL}`);
    writeModels(blocked, "the refused Decisions address");
    await restartCore();
    checkDecisionsInCore("the core reads the refused Decisions address", REFUSED_URL);
    const from = coreLog().length;
    const down = await askOnce(token, QUESTION);
    asked(down);
    // live, this is the first degraded question: the alert counts its five minutes from the row it just left
    if (LIVE) degradedAt = Date.now();
    checkDelivered(checks, down, "chat");
    check(
      "the chat endpoint still answers: the composer model wrote the answer",
      down.summary?.composer === "model",
      down.summary?.composer,
    );
    // nothing lets more through: a code question is still refused, by the chat judge
    const code = await askOnce(token, CODE_QUESTION);
    asked(code);
    const codeCheck = row(code.rows, "classified")?.checks?.find((c) => c.id === "asks_for_code");
    check(
      "a code question is still refused: asks_for_code, exit 2",
      code.code === 2 && code.summary?.refusal?.code === "asks_for_code",
      `exit ${code.code}, ${code.summary?.status} ${code.summary?.refusal?.code ?? ""}`.trim(),
    );
    check(
      "the refusal judged by chat",
      row(code.rows, "refused")?.judge_mode === "chat" && codeCheck?.source === "chat" && codeCheck?.verdict === "block",
      codeCheck,
    );
    check(
      "the timeline's refused line says judge: chat",
      /^refused\s+asks_for_code · \$[\d.]+ · judge: chat$/m.test(code.stderr),
      code.stderr.match(/^refused .*$/m)?.[0] ?? "no refused line",
    );
    // what the core logged while the endpoint was down (its own words, never a question's text)
    const log = coreLog().slice(from).join("\n");
    const count = (text) => log.split(text).length - 1;
    const jevFailed = count("Jev failed; the next judge answers");
    check("the core logged Jev's failure and went on to the next judge", jevFailed > 0, `${jevFailed} time(s)`);
    if (LIVE) {
      const refused = count("the request to the judge failed before an answer arrived");
      const noKey = count("no key is configured for the judge");
      check(
        "the failure is the refused connection, not a missing key",
        refused > 0 && noKey === 0,
        `${refused} failed request(s), ${noKey} missing key(s)`,
      );
    } else {
      skip("the failure is the refused connection, not a missing key", "needs --live; the dry run has no key");
    }
    const toRules = count("no judge answered; the rules decide the stage");
    check("no stage fell through to the rules: the chat judge answered each", toRules === 0, `${toRules} time(s)`);
    const phaseTwo = coreLog().slice(from);
    status();
    verify();

    phase("2b. the circuit breaker: open for a minute, then closed again, with no restart");
    const failedLines = phaseTwo.filter((line) => line.includes("Jev failed; the next judge answers"));
    const openedLines = phaseTwo.filter((line) => line.includes("Jev failed too often; it is skipped for a while"));
    check("the breaker opened: 3 failures within 30 s", openedLines.length === 1, `${openedLines.length} time(s)`);
    if (openedLines.length === 1) {
      const openedAt = timeOf(openedLines[0]);
      const before = failedLines.filter((line) => timeOf(line) <= openedAt).length;
      const during = failedLines.filter((line) => timeOf(line) > openedAt && timeOf(line) < openedAt + 60_000).length;
      check("Jev was asked 3 times, then no more while the breaker was open", before === 3 && during === 0, `${before} before, ${during} during its minute`);
      // the docker daemon's clock stamps the lines, this one reads the host's: a few seconds of margin
      const wait = openedAt + 60_000 + 5_000 - Date.now();
      if (wait > 0) {
        show(`waiting ${Math.ceil(wait / 1000)} s for the breaker's minute to pass`);
        await pause(wait);
      }
      const closedFrom = coreLog().length;
      const again = await askOnce(token, QUESTION);
      asked(again);
      checkDelivered(checks, again, "chat");
      const asksJev = coreLog().slice(closedFrom).filter((line) => line.includes("Jev failed; the next judge answers")).length;
      check("Jev was asked again after its minute: the breaker closed by itself, the core not restarted", asksJev > 0, `${asksJev} request(s) to Jev failed`);
      if (LIVE) show("the endpoint is still cut, so the breaker opens again after three more failures; Jev answering again is phase 3");
    } else {
      skip("the breaker's minute", "it never opened");
    }

    phase("2c. the alert: the judge degraded for more than 5 minutes");
    show(`the first degraded question ended ${Math.round((Date.now() - degradedAt) / 1000)} s ago`);
    const early = statusCheck();
    if (Date.now() - degradedAt < 280_000) {
      check("not yet: mctl status --check has no line about the judge", !early.judge, `exit ${early.code}`);
    } else {
      skip("not yet: no line about the judge", "the questions took more than 5 minutes");
    }
    let held = null;
    const alertBy = Date.now() + 9 * 60_000;
    while (Date.now() < alertBy) {
      const now = statusCheck();
      if (now.judge) {
        held = now;
        break;
      }
      await pause(15_000);
    }
    const seconds = Math.round((Date.now() - degradedAt) / 1000);
    check("the alert holds 5 minutes after the first degraded question", held !== null && seconds >= 290 && seconds <= 420, `${seconds} s`);
    if (held) {
      show(held.lines.join("\n"));
      check("mctl status --check exits 1 and names the judge", held.code === 1 && /^judge: answering in a degraded mode \(chat\) for \d+ minutes, since \d{4}-\d\d-\d\d \d\d:\d\d UTC; no question has ended on Jev since$/.test(held.judge), `exit ${held.code}`);
      const listed = admin(["status"]).replace(/\r\n/g, "\n").split("\n\n")[0];
      check("mctl status lists it first, under Alerts", listed.startsWith("Alerts\n") && listed.includes("\njudge: answering in a degraded mode"), listed.split("\n")[0]);
      status();
    }
    // the core's own minute: one warning in fixed words, and not another on the next minute
    const warning = "alert: the judge has been degraded for more than 5 minutes";
    const warned = () => coreLog().filter((line) => line.includes(warning));
    let first = warned();
    for (let waited = 0; first.length === 0 && waited < 90; waited += 10) {
      await pause(10_000);
      first = warned();
    }
    check("the core logged the alert once", first.length === 1, `${first.length} line(s)`);
    if (first.length === 1) {
      show(first[0].slice(first[0].indexOf(" ") + 1));
      check("in fixed words: the kind, the judge and the minutes, no question text", /"alert":"judge"/.test(first[0]) && /"mode":"chat"/.test(first[0]) && !first[0].includes(QUESTION), first[0]);
      await pause(65_000);
      check("and not again on the next minute: once an hour", warned().length === 1, `${warned().length} line(s)`);
    }
    verify();

    phase(`3. the Decisions endpoint back: ${normal}`);
    writeModels(base, "the normal Decisions address");
    await restartCore();
    checkDecisionsInCore("the core reads the normal Decisions address again", normal);
    const back = coreLog().length;
    const after = await askOnce(token, QUESTION);
    asked(after);
    if (!LIVE) skip("Jev judges again", "needs --live");
    checkDelivered(checks, after, expected("jev"));
    if (LIVE) {
      const failed = coreLog().slice(back).join("\n").split("Jev failed; the next judge answers").length - 1;
      check("Jev answered the first question after the restart: no failure logged", failed === 0, `${failed} failure(s)`);
      show("the breaker lives in the core's memory, so the restart cleared it: no minute to wait");
    }
    if (LIVE) {
      const cleared = statusCheck();
      check("the alert is gone: the last question ended on Jev", !cleared.judge, cleared.lines.join(" | ") || "nothing said");
    } else {
      skip("the alert is gone", "needs --live; the chat judge still answers");
    }
    status();
    verify();

    phase("the run as a whole");
    const logged = allLogs();
    const inLogs = leaked(logged);
    check("no secret in the services' logs", inLogs === 0, `${inLogs} found`);
    check("no question text in the services' logs", ![QUESTION, CODE_QUESTION].some((q) => logged.includes(q)));
    const printed = leaked(outputs.join("\n"));
    check("no secret in the CLI's outputs and the streams", printed === 0, `${printed} found`);
    check(
      `the drill's questions spent at most $${CAP_USD.toFixed(2)}${LIVE ? "" : " (the replay server's made-up costs)"}`,
      spent <= CAP_USD,
      `$${spent.toFixed(4)} over ${endings.jev + endings.chat + endings.rules + endings.none} question(s)`,
    );
  } finally {
    // the stack goes down after this, unless --keep: leave it on the address it started with
    if (read(MODELS) !== restored) writeFileSync(MODELS, restored);
  }
  console.log(`\n${failures === 0 ? "the drill passed" : `${failures} check(s) failed`}`);
  return failures === 0;
});

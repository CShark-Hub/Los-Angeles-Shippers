// The disk-full drill: the core can't write an audit row because its disk is full. On the
// integration runs' stack under a project of its own, the core is recreated with its /data on a
// 16 MB tmpfs (disk-full.override.yml), so the drill fills a disk of the core's own in memory and
// never one Docker shares with other stacks. What docs/deploy.md says is checked: the question
// fails with `internal` and nothing is delivered, `mctl status` says the disk is full and shows
// nothing; once the space is back the audit chain verifies and questions are answered again without
// a restart (a restart afterwards is fine too).
// Then the same with a little room left, so that a question starts and runs out of space part-way.
// The models are the replay server's: the drill costs nothing.
//
//   node tests/drills/disk-full.mjs            # --keep leaves the stack up, with the space freed

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { answerOf } from "../it/replay/citations.mjs";

// the stack's own project, port and subnets, so the drill runs beside the others; stack.mjs reads
// them when it is imported
const DEFAULTS = {
  MERCURY_IT_PROJECT: "mercury-drill-disk",
  MERCURY_IT_PORT: "18103",
  MERCURY_IT_PROBES_SUBNET: "172.30.55.0/24",
  MERCURY_IT_EDGE_SUBNET: "172.30.54.0/24",
};
for (const [name, value] of Object.entries(DEFAULTS)) process.env[name] ??= value;

const PROJECT = process.env.MERCURY_IT_PROJECT;
if (PROJECT === "mercury") {
  // the local stack's project holds a database worth keeping, and the drill ends with `down -v`
  console.error("the disk-full drill never runs on the project `mercury`; name another in MERCURY_IT_PROJECT");
  process.exit(2);
}
if (process.argv.includes("--live")) {
  console.error("the disk-full drill runs on the replay server only; it takes no --live");
  process.exit(2);
}
const KEEP = process.argv.includes("--keep");

const { admin, ask, composeCommand, http, mask, mercury, people, root, rows, waitForProbe, withStack } = await import(
  "../it/stack.mjs"
);

const here = dirname(fileURLToPath(import.meta.url));
const composeDir = join(root, "deploy/compose");
const work = join(root, "target", PROJECT === "mercury-it" ? "it" : PROJECT);
const OVERRIDE = relative(composeDir, join(here, "disk-full.override.yml")).split(sep).join("/");
/** The run's files as stack.mjs passes them, and the override that puts /data on a tmpfs. */
const FILES = [
  "docker-compose.yml",
  "docker-compose.fixture.yml",
  "docker-compose.local.yml",
  "docker-compose.it.yml",
  OVERRIDE,
];
/** How long the CLI waits for the end of a question cut off part-way, in seconds. */
const DEADLINE = "15";

/** A question the replay server has a recording for, and the answer it composes. */
const RECORDING = JSON.parse(
  readFileSync(join(root, "tests/it/replay/recordings/questions/what-was-merged-this-week.json"), "utf8"),
);
const QUESTION = RECORDING.question;
/** The answer as the core delivers it, read by the core's own citation rules. */
const ANSWER = answerOf(RECORDING.composer).text;
/** The answer's sentences: an output that holds any of them delivered the answer. */
const SENTENCES = ANSWER.split(/(?<=[.!?])\s+/).filter((sentence) => sentence.length >= 20);
const holdsAnswer = (text) => SENTENCES.some((sentence) => text.includes(sentence));
const ASK = ["--project", "delta", QUESTION];

// ---------------------------------------------------------------------------------------------
// What the drill prints

const failures = [];
function check(ok, what, seen) {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}${seen === undefined ? "" : ` (${mask(String(seen))})`}`);
  if (!ok) failures.push(what);
  return ok;
}
function show(label, text) {
  console.log(`     ${label}`);
  for (const line of mask(String(text)).trim().split(/\r?\n/)) console.log(`       ${line}`);
}
const step = (title) => console.log(`\n== ${title}`);
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

// ---------------------------------------------------------------------------------------------
// The stack, with the override

function compose(args, seconds = 120) {
  for (let attempt = 1; ; attempt += 1) {
    const run = spawnSync("docker", ["compose", "-p", PROJECT, ...FILES.flatMap((f) => ["-f", f]), ...args], {
      cwd: composeDir,
      encoding: "utf8",
      env: { ...process.env, MERCURY_IT_WORK: relative(composeDir, work).split(sep).join("/") },
      timeout: seconds * 1000,
      maxBuffer: 64 * 1024 * 1024,
    });
    if (run.error) throw new Error(`docker compose ${args[0]} can't be run: ${run.error.message}`);
    // Docker Desktop on Windows sometimes holds its context file for a moment
    if (run.status !== 0 && /being used by another process/.test(run.stderr) && attempt < 5) continue;
    return run;
  }
}

/** A shell command in the core's container, as the core's user. */
function inCore(script) {
  const run = compose(["exec", "-T", "core", "sh", "-c", script], 60);
  return { code: run.status, out: mask(`${run.stdout}${run.stderr}`).trim() };
}

/** The core container's id, state, restart count and start, to see it never died. */
function coreContainer() {
  const id = compose(["ps", "-q", "core"]).stdout.trim();
  if (!id) return "missing";
  const inspected = spawnSync("docker", ["inspect", "-f", "{{.State.Status}} {{.RestartCount}} {{.State.StartedAt}}", id], {
    encoding: "utf8",
  });
  return `${id.slice(0, 12)} ${inspected.stdout.trim()}`;
}

async function healthy(seconds) {
  const end = Date.now() + seconds * 1000;
  while (Date.now() < end) {
    try {
      if ((await http("GET", "/v1/health")).status === 200) return true;
    } catch {
      // the core is still starting
    }
    await sleep(500);
  }
  return false;
}

/** The core restarted on the same database, answering, and the probe healthy. */
async function restartCore() {
  const restarted = compose(["restart", "core"], 120);
  if (restarted.status !== 0) throw new Error(`docker compose restart core failed: ${mask(restarted.stderr)}`);
  check(await healthy(120), "the restarted core answers /v1/health");
  check(await waitForProbe("delta", "healthy", 90), "the fixture's probe is healthy");
}

/** The disk's line of `df -k /data`: size, used, available, use%. */
const usage = () => inCore("df -k /data | tail -n 1").out;
/** The disk filled to the last byte, then `room` KB given back; gives dd's and df's lines. */
const fill = (room = 0) =>
  inCore(
    `rm -f /data/fill; dd if=/dev/zero of=/data/fill bs=64k 2>&1 | grep -v records; ` +
      (room > 0 ? `truncate -s -${room}K /data/fill; ` : "") +
      "df -k /data | tail -n 1",
  ).out;
const full = (df) => /\s0\s+100%\s+\/data\s*$/m.test(df);
const free = () => inCore("rm -f /data/fill; df -k /data | tail -n 1").out;
/** The size of the database's write-ahead log, where every commit lands first. */
const walBytes = () => Number(inCore("stat -c %s /data/mercury.db-wal 2>/dev/null || echo 0").out) || 0;
const coreTime = () => inCore("date -u +%Y-%m-%dT%H:%M:%SZ").out;

/** The core's warnings and errors logged since `since`, each once with how often it came. */
function coreLog(since) {
  const run = compose(["logs", "--no-color", "--since", since, "core"]);
  const seen = new Map();
  for (const line of run.stdout.split(/\r?\n/)) {
    const start = line.indexOf("{");
    if (start < 0) continue;
    let entry;
    try {
      entry = JSON.parse(line.slice(start));
    } catch {
      continue;
    }
    if (!["WARN", "ERROR"].includes(entry.level)) continue;
    const { message = "", error } = entry.fields ?? {};
    const text = `${entry.level} ${message}${error ? `: ${error}` : ""}`;
    seen.set(text, (seen.get(text) ?? 0) + 1);
  }
  return [...seen].map(([text, count]) => (count > 1 ? `${text} (x${count})` : text)).join("\n") || "none";
}

/** The audit rows `admin audit export` gives for these arguments: `{ seq, type, question, data }`. */
function exported(args) {
  return admin(["audit", "export", ...args])
    .split(/\r?\n/)
    .filter((line) => line.trim())
    .map((line) => JSON.parse(line))
    .map((row) => ({ seq: row.seq, type: row.type, question: row.question_id ?? null, data: row.payload ?? null }));
}

function verify() {
  try {
    return admin(["audit", "verify"]).trim();
  } catch (err) {
    return mask(err.message);
  }
}
const rowCount = (verified) => Number(verified.match(/^ok: (\d+) row/)?.[1] ?? NaN);

/** What a `--json` ask ended with: the question's status, or the error the server answered. */
const ending = (run) =>
  run.summary?.id
    ? `${run.summary.status}${run.summary.error ? ` ${run.summary.error.code}` : ""}`
    : run.summary?.error
      ? `no question: ${run.summary.error.code}`
      : `no JSON (exit ${run.code})`;

/** One question asked and checked as delivered, with its answer and its rows. */
async function delivered(token, what) {
  const run = ask(token, ASK);
  check(run.code === 0 && run.summary?.status === "delivered", what, ending(run));
  check(run.summary?.answer?.text === ANSWER, "with the recorded answer");
  const found = run.summary?.id ? (await rows(token, run.summary.id)).map((r) => r.event) : [];
  check(found.includes("answer") && found.at(-1) === "done", "its rows end with answer and done", found.join(" ") || "none");
  return run;
}

/** The question's state as `GET /v1/questions/{id}` gives it. */
async function questionState(token, id) {
  const got = await http("GET", `/v1/questions/${id}`, { headers: { authorization: `Bearer ${token}` } });
  const { status, cost_usd, judge_mode, answer } = got.json ?? {};
  return { http: got.status, status, answer: answer ?? null, cost_usd, judge_mode };
}

// ---------------------------------------------------------------------------------------------

await withStack(async () => {
  step("the core's /data on a 16 MB tmpfs");
  const recreated = compose(["up", "-d", "--no-build", "--no-deps", "disk-holder", "core"], 300);
  if (recreated.status !== 0) throw new Error(`recreating the core failed:\n${mask(recreated.stderr)}`);
  check(await healthy(120), "the recreated core answers /v1/health through Caddy");
  const mount = inCore("grep ' /data ' /proc/mounts").out;
  check(/^tmpfs \/data tmpfs .*size=16384k/m.test(mount), "/data is the tmpfs, not the named volume", mount);
  // a fresh database: the people again, and the probe registers with it at its next minute
  const tokens = people();
  check(await waitForProbe("delta", "healthy", 90), "the fixture's probe registered with the recreated core");

  step("a question with room on the disk");
  // the token's first use writes its last-used time; within a minute nothing does again
  const used = Date.now();
  await delivered(tokens.pm, "delivered");
  const verifiedBefore = verify();
  check(/^ok: /.test(verifiedBefore), "audit verify holds", verifiedBefore);
  show("the disk", `${usage()}\n${inCore("ls -l /data").out}`);

  // the asking token must not be due its last-used write, so that the question's own writes fail
  if (Date.now() - used > 40_000) {
    await sleep(Math.max(0, used + 61_000 - Date.now()));
    await http("GET", "/v1/me", { headers: { authorization: `Bearer ${tokens.pm}` } });
  }

  let filled = false;
  try {
    // -----------------------------------------------------------------------------------------
    step("fill the disk");
    const since = coreTime();
    const container = coreContainer();
    const df = fill();
    filled = true;
    show("dd, then df", df);
    check(full(df), "/data is full: nothing available");

    step("ask while the disk is full");
    const plain = mercury(tokens.pm, ["ask", ...ASK]);
    show(`mercury ask, exit ${plain.code}`, `${plain.stdout}${plain.stderr}`);
    check(plain.code === 1, "the question fails: exit 1", plain.code);
    check(!holdsAnswer(`${plain.stdout}${plain.stderr}`), "no answer text in the CLI's output");
    const json = ask(tokens.pm, ASK);
    show(`mercury ask --json, exit ${json.code}`, json.stdout || json.stderr);
    check(json.summary?.error?.code === "internal", "the error is internal", ending(json));
    check(!json.summary?.answer && !holdsAnswer(`${json.stdout}${json.stderr}`), "nothing delivered");
    // a person whose token is due its last-used write: the token check itself can't write
    const due = mercury(tokens.tech_lead, ["ask", ...ASK]);
    show(`another person, the token's first use: mercury ask, exit ${due.code}`, `${due.stdout}${due.stderr}`);
    check(due.code === 1 && /could not handle the request/.test(due.stderr), "fails the same way", due.code);
    check(!holdsAnswer(`${due.stdout}${due.stderr}`), "no answer text in the CLI's output");

    const health = await http("GET", "/v1/health");
    show("/v1/health while the disk is full", `${health.status} ${health.text}`);
    check(health.status === 200, "the core keeps serving: /v1/health answers", health.status);
    show("audit verify while the disk is full", verify());
    let status;
    try {
      status = admin(["status"]);
    } catch (err) {
      status = mask(err.message);
    }
    show("admin status while the disk is full", status);
    check(/the disk is full/.test(status) && !/internal/.test(status), "mctl status says the disk is full", status);
    check(!/^Alerts/m.test(status) && !/Probes/.test(status), "and shows nothing it couldn't audit", status);
    check(coreContainer() === container, "the core never died or restarted", coreContainer());

    step("free the space");
    show("rm, then df", free());
    filled = false;
    const verifiedAfter = verify();
    check(/^ok: /.test(verifiedAfter), "audit verify holds", verifiedAfter);
    check(rowCount(verifiedAfter) === rowCount(verifiedBefore), "no row was written or lost while the disk was full", `${rowCount(verifiedBefore)} before, ${rowCount(verifiedAfter)} after`);
    // after the row count above: the status writes a row of its own
    const shownAgain = admin(["status"]);
    check(/^Alerts\r?\n/.test(shownAgain), "mctl status shows again once the space is back, without a restart", shownAgain.split(/\r?\n/)[0]);
    const window = exported(["--from", since]);
    show(`the audit rows since ${since}`, window.map((r) => `${r.seq} ${r.type}`).join("\n") || "none");

    step("questions with the space back, the core not restarted");
    // a commit that failed for the full disk must leave nothing behind that fails later questions
    const endings = [];
    for (let i = 0; i < 3; i += 1) {
      const run = ask(tokens.pm, ASK);
      const theirs = run.summary?.id ? exported(["--question", run.summary.id]).map((r) => r.type) : [];
      endings.push({ ending: ending(run), output: `${run.stdout}${run.stderr}`, answer: run.summary?.answer, rows: theirs });
    }
    show("three questions, one after the other: the ending, then the question's audit rows", endings.map((e) => `${e.ending}: ${e.rows.join(" ") || "no rows"}`).join("\n"));
    check(endings.every((e) => e.ending === "delivered"), "each is delivered again without a restart", endings.map((e) => e.ending).join(", "));
    check(
      endings.every((e) => e.ending === "delivered" || (!e.answer && !holdsAnswer(e.output) && !e.rows.includes("answer"))),
      "none that failed delivered anything or has an answer row",
    );
    show(`the core's warnings and errors since ${since}`, coreLog(since));

    // the core needs no restart once the space is back; one now is fine too
    step("restart the core");
    await restartCore();
    const verifiedRestart = verify();
    check(/^ok: /.test(verifiedRestart), "audit verify holds", verifiedRestart);
    const walBefore = walBytes();
    await delivered(tokens.pm, "a question is delivered");
    // how much one whole question writes, for the room the next step leaves
    const perQuestion = walBytes() - walBefore;
    show("one question's writes to the write-ahead log", `${perQuestion} bytes`);

    // -----------------------------------------------------------------------------------------
    step("a little room: a question starts, then runs out of space part-way");
    const room = Math.max(8, Math.floor(perQuestion / 2 / 4096) * 4);
    const sinceCut = coreTime();
    const containerCut = coreContainer();
    show(`fill, then give back ${room} KB`, fill(room));
    filled = true;
    // a question cut off part-way may never write its end; the CLI gives up after DEADLINE seconds
    process.env.MERCURY_ASK_DEADLINE = DEADLINE;
    const cut = ask(tokens.pm, ASK);
    delete process.env.MERCURY_ASK_DEADLINE;
    const output = `${cut.stdout}${cut.stderr}`;
    const id = cut.summary?.id;
    show(`mercury ask --json, exit ${cut.code}`, JSON.stringify({ status: cut.summary?.status, error: cut.summary?.error ?? null, answer: cut.summary?.answer ?? null, stderr: cut.stderr.trim() }));
    check(Boolean(id) && cut.summary?.status !== "delivered", "the question started and didn't finish", ending(cut));
    check(!cut.summary?.answer && !holdsAnswer(output), "nothing delivered");
    check(cut.code === 1, "the CLI says the question failed", `exit ${cut.code}`);
    // the write that ran out of space fails alone: the question's error is written after it
    check(cut.summary?.error?.code === "internal", "it ends with internal", `exit ${cut.code}, ${ending(cut)}`);
    check(coreContainer() === containerCut, "the core never died or restarted", coreContainer());

    step("free the space");
    show("rm, then df", free());
    filled = false;
    check(/^ok: /.test(verify()), "audit verify holds", verify());
    if (id) {
      const state = await questionState(tokens.pm, id);
      show("GET /v1/questions/<the question>", JSON.stringify(state));
      const theirs = exported(["--question", id]);
      show("its audit rows", theirs.map((r) => `${r.seq} ${r.type}`).join("\n") || "none");
      const types = theirs.map((r) => r.type);
      check(!types.includes("answer") && !types.includes("done"), "no answer or done row", types.join(" "));
      check(state.status === "failed", "it reads failed once the space is back", state.status);
    }
    show(`the core's warnings and errors since ${sinceCut}`, coreLog(sinceCut));

    step("restart the core");
    await restartCore();
    if (id) {
      // a question whose row says failed isn't one the restart ends again: when its error row
      // couldn't be written, its rows stop where the space ran out
      const state = await questionState(tokens.pm, id);
      show("GET /v1/questions/<the question>", JSON.stringify(state));
      const theirs = exported(["--question", id]);
      show("its audit rows", theirs.map((r) => `${r.seq} ${r.type}${r.type === "error" ? ` ${JSON.stringify(r.data)}` : ""}`).join("\n"));
      check(state.status === "failed", "it still reads failed", state.status);
      check(!theirs.some((r) => ["answer", "done"].includes(r.type)), "still no answer or done row");
    }
    const verifiedEnd = verify();
    check(/^ok: /.test(verifiedEnd), "audit verify holds", verifiedEnd);
    await delivered(tokens.pm, "a question is delivered");
  } finally {
    // the space back even when a step threw, for a stack left up with --keep
    if (filled) free();
  }

  console.log(`\n${failures.length === 0 ? "the drill passed" : `the drill failed: ${failures.map((f) => `"${f}"`).join(", ")}`}`);
  if (KEEP) console.log(`the core runs with ${OVERRIDE}: add \`-f ${OVERRIDE}\` to ${composeCommand}`);
  return failures.length === 0;
});

// stack.mjs took the run's services down with the run's files; the holder and the tmpfs volume
// are the override's
if (!KEEP) {
  const down = compose(["down", "-v", "--remove-orphans"], 120);
  if (down.status !== 0) console.error(`docker compose down with the override failed: ${mask(down.stderr)}`);
}

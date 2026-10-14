// `just bench`: load and cost. Five askers at once (by default), each with a token of its own and ten
// questions in a fixed seeded order, each question asked with `mercury ask --json` as a script would
// ask it. From each question's state (`GET /v1/questions/{id}`, the `done` row's `duration_ms`,
// `cost_usd` and `judge_mode`) it reports the server's p50 and p95, the client's wall time, the cost
// per question, the endings and the degraded share (`chat` or `rules` over every ending, the way
// `mctl status` counts it), with the probes' GitHub requests per hour and snapshot RAM
// (tests/bench/probes.mjs) → tests/bench/reports/<date>-<mode>.md and the same summary on stdout.
//
// Modes:
//   (default)          a fresh stack of its own on the replay server (Compose project `mercury-bench`,
//                      port 18121), the recorded allowed questions; costs nothing
//   --live             the same stack with the real models (the OpenRouter key in
//                      deploy/compose/secrets/openrouter_api_key) and the golden set's allowed questions
//   --url URL --token TOKEN [--token TOKEN …] [--project ID]
//                      an existing stack (a laptop stack's, say), never touching Docker but to read the
//                      probes with --compose-project; tokens can also come from MERCURY_BENCH_TOKENS
//                      (separated by commas or spaces), so they stay out of the shell's history
//
// Options: --askers N (5), --per N (10 questions each), --seed N (13), --label NAME (the report's
// name in place of the mode), --questions FILE (JSON lines like tests/golden/questions.jsonl; the
// allowed ones are asked), --compose-project NAME and --compose-file FILE (with --url: the Compose
// project whose probes are read, from deploy/compose; its .env names the files unless --compose-file
// does), --keep (leave the own stack up).
//
// The report holds no token and no answer's text. It exits 1 when a token shows in an output, when a
// question's state can't be read, or when its degraded share and `mctl status`'s disagree; the
// targets are measured and compared, not enforced.

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";

// stack.mjs reads these when it is imported, so they are set first: a stack of its own beside the others
const DEFAULTS = {
  MERCURY_IT_PROJECT: "mercury-bench",
  MERCURY_IT_PORT: "18121",
  MERCURY_IT_PROBES_SUBNET: "172.30.71.0/24",
  MERCURY_IT_EDGE_SUBNET: "172.30.70.0/24",
};
for (const [name, value] of Object.entries(DEFAULTS)) process.env[name] ??= value;

const stack = await import("../it/stack.mjs");
const { LIVE, admin, composeCall, leaked, mask, root, secrets, withStack } = stack;

/** The targets the readings are compared with: five concurrent askers, and the cost of a question. */
const TARGETS = { p50Ms: 8000, p95Ms: 15000, meanUsd: 0.02 };
/** How long one `mercury ask` may run before it is stopped; the CLI's own deadline is 120 s. */
const ASK_LIMIT_MS = 180_000;
/** How long one question may be held back by the per-token rate limit, in all, before it counts as not started. */
const MAX_HELD_S = 20 * 60;
/** How long a question still running after its `mercury ask` ended is waited for. */
const SETTLE_S = 60;

const CLI = process.env.MERCURY_CLI
  ? resolve(process.env.MERCURY_CLI)
  : join(root, "target/debug", process.platform === "win32" ? "mercury.exe" : "mercury");

// ---------------------------------------------------------------------------------------------
// The options

function usage(problem) {
  console.error(`bench: ${problem}`);
  console.error(
    "usage: node tests/bench/run.mjs [--live | --url URL --token TOKEN … [--project ID] [--compose-project NAME]] [--askers N] [--per N] [--seed N] [--label NAME] [--questions FILE] [--keep]",
  );
  process.exit(2);
}

function options(argv) {
  const o = { askers: 5, per: 10, seed: 13, project: "delta", tokens: [], composeFiles: [], url: null, label: null, questions: null, composeProject: null };
  const whole = (text, name) => {
    if (!/^[1-9]\d*$/.test(text)) usage(`${name} takes a whole number above 0`);
    return Number(text);
  };
  for (let i = 0; i < argv.length; i += 1) {
    const name = argv[i];
    const value = () => {
      const v = argv[(i += 1)];
      if (v === undefined || v.startsWith("--")) usage(`${name} needs a value`);
      return v;
    };
    switch (name) {
      case "--askers": o.askers = whole(value(), name); break;
      case "--per": o.per = whole(value(), name); break;
      case "--seed": o.seed = whole(value(), name); break;
      case "--url": o.url = value().replace(/\/+$/, ""); break;
      case "--token": o.tokens.push(value()); break;
      case "--project": o.project = value(); break;
      case "--label": o.label = value(); break;
      case "--questions": o.questions = value(); break;
      case "--compose-project": o.composeProject = value(); break;
      case "--compose-file": o.composeFiles.push(value()); break;
      // stack.mjs reads these two itself
      case "--live":
      case "--keep":
        break;
      default:
        usage(`no such option: ${name}`);
    }
  }
  if (o.label !== null && !/^[a-z0-9][a-z0-9-]*$/.test(o.label)) usage("--label takes lower-case letters, digits and dashes");
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(o.project)) usage("--project takes a project id, like delta");
  if (o.url) {
    if (LIVE) usage("--live starts a stack of its own; with --url the stack's own models answer");
    if (!/^https?:\/\/[^/\s]+$/.test(o.url)) usage("--url takes the server's address, like http://localhost:8080");
    o.tokens.push(...(process.env.MERCURY_BENCH_TOKENS ?? "").split(/[\s,]+/).filter(Boolean));
    if (o.tokens.length === 0 && process.env.MERCURY_TOKEN) o.tokens.push(process.env.MERCURY_TOKEN);
    if (o.tokens.length === 0) usage("--url needs a token: --token, MERCURY_BENCH_TOKENS or MERCURY_TOKEN");
    if (o.tokens.some((t) => !t.startsWith("mcy_"))) usage("a token starts with mcy_");
  } else if (o.tokens.length > 0 || o.composeProject || o.composeFiles.length > 0) {
    usage("--token, --compose-project and --compose-file go with --url; the own stack makes its own");
  } else if (o.project !== "delta") {
    usage("the own stack holds the fixture's project, delta, only; --project goes with --url");
  }
  if (o.composeFiles.length > 0 && !o.composeProject) usage("--compose-file goes with --compose-project");
  return o;
}

const opts = options(process.argv.slice(2));
const MODE = opts.url ? "url" : LIVE ? "live" : "replay";
secrets.push(...opts.tokens);
if (!existsSync(CLI)) {
  console.error(`bench: no mercury CLI at ${CLI}: build it (cargo build -p mercury-cli) or name one with MERCURY_CLI`);
  process.exit(2);
}

// ---------------------------------------------------------------------------------------------
// The questions

/**
 * The recorded questions the replay server answers all the way: the input judge, the planner and the
 * composer each have a recording. Left out: the red team's project (`poison-…`, not on this stack),
 * recordings that end at the input or the policy (no planner), or after the plan (no composer), and
 * the one whose composer overspends on purpose (`cost`): it ends budget_exceeded and spends the asker's day.
 */
function recordedQuestions() {
  const dir = join(root, "tests/it/replay/recordings/questions");
  const all = readdirSync(dir)
    .filter((name) => name.endsWith(".json"))
    .sort()
    .map((name) => ({ name: name.replace(/\.json$/, ""), ...JSON.parse(readFileSync(join(dir, name), "utf8")) }));
  const picked = all.filter(
    (r) => !r.name.startsWith("poison-") && r.judge && r.planner && typeof r.composer === "string" && r.cost === undefined,
  );
  return {
    questions: picked.map((r) => ({ id: r.name, text: r.question })),
    source: `the replay server's recordings that answer a question all the way (input judge, planner and composer recorded): ${picked.length} of ${all.length} in tests/it/replay/recordings/questions; left out are the red team's project, those that end at the input, the policy or the plan, and the one that overspends on purpose`,
  };
}

/**
 * The allowed questions (`expect.refuse: false`) of a labelled set for the project, as a project manager
 * or a tech lead asks them (the two roles may ask the same classes in config-examples/policies.yaml; the
 * board's questions are left out, since a manager asking them measures a refusal). A project the set
 * has no question for gets delta's, asked of it.
 */
function labelledQuestions(file, project) {
  const lines = readFileSync(file, "utf8")
    .split(/\r?\n/)
    .filter((line) => line.trim())
    .map((line) => JSON.parse(line));
  const allowed = lines.filter((q) => q.expect?.refuse === false && (q.role === undefined || ["pm", "tech_lead"].includes(q.role)));
  const own = allowed.filter((q) => q.project === undefined || q.project === project);
  // a file outside the repository by its name only: its folder may name the person who runs this
  const named = file.startsWith(root) ? file.slice(root.length + 1).replace(/\\/g, "/") : basename(file);
  if (own.length > 0) {
    return {
      questions: own.map((q, i) => ({ id: q.id ?? `q${i + 1}`, text: q.text })),
      source: `the allowed questions of ${named} for ${project}, labelled for a project manager or a tech lead: ${own.length} of ${lines.length}`,
    };
  }
  const delta = allowed.filter((q) => q.project === "delta");
  if (delta.length === 0) throw new Error(`${named} has no allowed question for ${project} or delta`);
  return {
    questions: delta.map((q, i) => ({ id: q.id ?? `q${i + 1}`, text: q.text })),
    source: `${named} has no allowed question for ${project}, so delta's ${delta.length} allowed ones (a project manager's or a tech lead's) are asked of it: they name delta's modules, so more of them may end in a clarify or a refusal than the project's own would`,
  };
}

function questionSet() {
  if (opts.questions) return labelledQuestions(resolve(opts.questions), opts.project);
  if (MODE === "replay") return recordedQuestions();
  return labelledQuestions(join(root, "tests/golden/questions.jsonl"), opts.project);
}

/** `list` in an order fixed by `seed` (mulberry32 and Fisher-Yates), so two runs ask the same questions. */
function shuffled(list, seed) {
  let state = seed >>> 0;
  const next = () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const out = [...list];
  for (let i = out.length - 1; i > 0; i -= 1) {
    const j = Math.floor(next() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

// ---------------------------------------------------------------------------------------------
// Asking

const wait = (ms) => new Promise((done) => setTimeout(done, ms));

/** `mercury ask --project <project> <text> --json` without blocking the other askers: the exit code, the outputs, the parsed JSON and the wall time. */
function askOnce(base, token, project, text) {
  return new Promise((done) => {
    const started = performance.now();
    const child = spawn(CLI, ["ask", "--project", project, text, "--json"], {
      env: { ...process.env, MERCURY_URL: base, MERCURY_TOKEN: token },
      windowsHide: true,
    });
    let stdout = "";
    let stderr = "";
    let settled = false;
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    const timer = setTimeout(() => child.kill(), ASK_LIMIT_MS);
    const finish = (code, problem) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      let json = null;
      try {
        json = JSON.parse(stdout);
      } catch {
        // an error before the question started may leave stdout empty
      }
      done({ code, stdout, stderr: problem ? `${stderr}\n${problem}` : stderr, json, wallMs: performance.now() - started });
    };
    child.on("error", (err) => finish(null, `the mercury CLI can't be run: ${err.message}`));
    child.on("close", (code) => finish(code, null));
  });
}

/**
 * One question, asked again after the wait the server names when the token's rate limit holds it back
 * (429 before any question starts): the waits are counted apart and are in no time measured.
 */
async function askHeld(base, token, project, text) {
  let held = 0;
  let heldS = 0;
  for (;;) {
    const startedAt = Date.now();
    const run = await askOnce(base, token, project, text);
    const refusedRequest = run.json && !run.json.id ? run.json.error : null;
    if (refusedRequest?.code === "rate_limited" && heldS < MAX_HELD_S) {
      const seconds = Number(refusedRequest.hint?.match(/Wait (\d+) seconds?/)?.[1] ?? 10);
      held += 1;
      heldS += seconds;
      await wait(seconds * 1000 + 250);
      continue;
    }
    return { ...run, startedAt, endedAt: Date.now(), held, heldS };
  }
}

/** The askers at once, each through its plan in order: one record per question. */
async function askAll(base, askers, project) {
  const results = [];
  await Promise.all(
    askers.map(async (asker) => {
      for (const q of asker.plan) {
        const run = await askHeld(base, asker.token, project, q.text);
        results.push({ asker: asker.name, token: asker.token, q, ...run });
        process.stdout.write(run.code === 0 ? "." : run.code === 2 ? "r" : "x");
      }
    }),
  );
  process.stdout.write("\n");
  return results;
}

/** `GET /v1/questions/{id}` with the asker's token, waited for while the question still runs. */
async function stateOf(base, token, id) {
  const end = Date.now() + SETTLE_S * 1000;
  for (;;) {
    let state = null;
    let problem = null;
    try {
      const res = await fetch(`${base}/v1/questions/${encodeURIComponent(id)}`, {
        headers: { authorization: `Bearer ${token}`, accept: "application/json" },
        signal: AbortSignal.timeout(30_000),
      });
      if (res.ok) state = await res.json();
      else problem = `the state answered ${res.status}`;
    } catch (err) {
      problem = `the state can't be read: ${err.message}`;
    }
    if (state && state.status !== "running") return { state };
    if (Date.now() > end) return { state, problem: problem ?? "still running" };
    await wait(1000);
  }
}

// ---------------------------------------------------------------------------------------------
// The figures

/** The nearest-rank percentile, as the golden report reads its times. */
function percentile(values, p) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1))];
}

const secs = (ms) => (ms === null || ms === undefined ? "—" : `${(ms / 1000).toFixed(2)} s`);
const usd = (n) => (n === null || n === undefined ? "—" : `$${n.toFixed(5)}`);
const cell = (text) => String(text ?? "—").replace(/\|/g, "\\|");
/** A share as a whole percentage rounded down, the way `mctl status` writes it. */
const floorShare = (part, whole) => (whole === 0 ? "-" : `${Math.floor((part / whole) * 100)}%`);
const sum = (values) => values.reduce((a, b) => a + b, 0);
const mib = (bytes) => (bytes === null || bytes === undefined ? "not measured" : `${(bytes / (1024 * 1024)).toFixed(1)} MiB`);

/** The endings `mctl status` counts: the questions whose last row is `done`, `refused`, `clarify` or `error`. */
const ENDED = new Set(["delivered", "refused", "clarify", "failed"]);

/** What became of one question, from its state (the server's) and the CLI's JSON. */
function settle(run, read) {
  const summary = run.json?.id ? run.json : null;
  const state = read?.state ?? null;
  const status = state?.status ?? summary?.status ?? null;
  let ending;
  if (!summary) {
    const code = run.json?.error?.code;
    const said = mask((run.stderr.split(/\r?\n/).find((line) => line.startsWith("mercury: ")) ?? run.stderr.trim().split(/\r?\n/).at(-1) ?? "").trim());
    ending = code ? `not started (${code})` : `not started (the CLI: ${said || `exit ${run.code}`})`;
  } else if (status === "delivered") ending = "delivered";
  else if (status === "refused") ending = `refused (${state?.refusal?.code ?? summary.refusal?.code ?? "?"})`;
  else if (status === "clarify") ending = `clarify (${summary.clarify?.kind ?? "?"})`;
  else if (status === "pending_signoff") ending = "waiting for sign-off";
  else if (status === "failed") ending = `failed (${summary.error?.code ?? "no error event"})`;
  else ending = "still running";
  const judge = state ? state.judge_mode : summary?.judge_mode ?? null;
  const cost = state ? state.cost_usd : summary?.cost_usd ?? null;
  const ms = state ? state.duration_ms : summary?.duration_ms ?? null;
  // the CLI's JSON and the server's state say the same of a question that ended (one held for
  // sign-off has no ending yet, so the CLI knows neither its cost nor its judge)
  const agrees =
    !summary || !state || !ENDED.has(state.status)
      ? null
      : summary.status === state.status && (summary.judge_mode ?? null) === (state.judge_mode ?? null) &&
        Math.abs((summary.cost_usd ?? 0) - (state.cost_usd ?? 0)) < 1e-9 && (summary.duration_ms ?? null) === (state.duration_ms ?? null);
  return {
    asker: run.asker,
    q: run.q,
    id: summary?.id ?? null,
    status,
    ending,
    ended: summary !== null && ENDED.has(status),
    judge: judge ?? "none",
    cost,
    ms,
    wallMs: run.wallMs,
    startedAt: run.startedAt,
    endedAt: run.endedAt,
    held: run.held,
    heldS: run.heldS,
    agrees,
    unread: summary !== null && !state ? read?.problem ?? "not read" : null,
    answer: summary?.answer?.text ?? null,
  };
}

function figures(records) {
  const started = records.filter((r) => r.id !== null);
  const delivered = records.filter((r) => r.status === "delivered");
  const withDuration = records.filter((r) => typeof r.ms === "number");
  const serverMs = withDuration.map((r) => r.ms);
  const wallAll = started.map((r) => r.wallMs);
  const wallDelivered = delivered.map((r) => r.wallMs);
  const costs = started.filter((r) => typeof r.cost === "number").map((r) => r.cost);
  const deliveredCosts = delivered.filter((r) => typeof r.cost === "number").map((r) => r.cost);
  const endings = {};
  for (const r of records) endings[r.ending] = (endings[r.ending] ?? 0) + 1;
  const ended = records.filter((r) => r.ended);
  const modes = { jev: 0, chat: 0, rules: 0, none: 0 };
  for (const r of ended) modes[r.judge in modes ? r.judge : "none"] += 1;
  // the most questions in flight at once, from each ask's start and end
  const edges = records
    .flatMap((r) => [{ at: r.startedAt, step: 1 }, { at: r.endedAt, step: -1 }])
    .sort((a, b) => a.at - b.at || a.step - b.step);
  let inFlight = 0;
  let mostAtOnce = 0;
  for (const { step } of edges) mostAtOnce = Math.max(mostAtOnce, (inFlight += step));
  return {
    asked: records.length,
    started: started.length,
    delivered: delivered.length,
    server: { n: serverMs.length, p50: percentile(serverMs, 50), p95: percentile(serverMs, 95), max: serverMs.length ? Math.max(...serverMs) : null },
    wallAll: { n: wallAll.length, p50: percentile(wallAll, 50), p95: percentile(wallAll, 95), max: wallAll.length ? Math.max(...wallAll) : null },
    wallDelivered: { n: wallDelivered.length, p50: percentile(wallDelivered, 50), p95: percentile(wallDelivered, 95), max: wallDelivered.length ? Math.max(...wallDelivered) : null },
    cost: {
      n: costs.length,
      mean: costs.length ? sum(costs) / costs.length : null,
      max: costs.length ? Math.max(...costs) : null,
      total: sum(costs),
      deliveredMean: deliveredCosts.length ? sum(deliveredCosts) / deliveredCosts.length : null,
      deliveredN: deliveredCosts.length,
    },
    endings,
    modes,
    ended: ended.length,
    degraded: modes.chat + modes.rules,
    held: sum(records.map((r) => r.held)),
    heldS: sum(records.map((r) => r.heldS)),
    mostAtOnce,
    agree: records.filter((r) => r.agrees === true).length,
    disagree: records.filter((r) => r.agrees === false),
    unread: records.filter((r) => r.unread !== null),
  };
}

// ---------------------------------------------------------------------------------------------
// `mctl status` on the own stack

/** The judge modes, the day's spend of everyone and the per-question cap, as `admin status` printed them. */
function readStatus(text) {
  const total = Number(text.match(/Judge modes of the questions that ended in the last 24 hours: (\d+)/)?.[1] ?? NaN);
  const modes = {};
  for (const mode of ["jev", "chat", "rules", "none"]) {
    const m = text.match(new RegExp(`^${mode}\\s+(\\d+)\\s+\\S+%$`, "m"));
    modes[mode] = m ? Number(m[1]) : 0;
  }
  return {
    total,
    modes,
    degraded: text.match(/Degraded \(chat or rules\): (\S+?);/)?.[1] ?? (total === 0 ? "-" : null),
    everyone: Number(text.match(/^everyone\s+\$([\d.]+)/m)?.[1] ?? NaN),
    cap: Number(text.match(/A question may cost at most \$([\d.]+)\./)?.[1] ?? NaN),
  };
}

function compareStatus(f, shown) {
  const mine = { ...f.modes };
  const same = Number.isFinite(shown.total) && shown.total === f.ended && ["jev", "chat", "rules", "none"].every((m) => shown.modes[m] === mine[m]);
  const line = `degraded share: this run ${floorShare(f.degraded, f.ended)} (${f.degraded} of ${f.ended} endings), mctl status ${shown.degraded ?? "unread"} (${shown.modes.chat + shown.modes.rules} of ${Number.isFinite(shown.total) ? shown.total : "?"} in its 24-hour window)`;
  let why = null;
  if (!same) {
    why =
      Number.isFinite(shown.total) && shown.total > f.ended
        ? `mctl status counts ${shown.total - f.ended} more ending(s) in its window than this run asked: on a fresh stack there are none, so the two disagree`
        : "the two count different endings";
  }
  return { same, line, why };
}

// ---------------------------------------------------------------------------------------------
// The probes

/** A `docker compose` call on a stack as the probes' reading needs it: the exit code and the outputs. */
function composeRunner(call) {
  return (args) => {
    const c = call(args);
    const run = spawnSync(c.command, c.args, {
      cwd: c.cwd,
      encoding: "utf8",
      env: { ...process.env, ...c.env },
      timeout: 120_000,
      maxBuffer: 64 * 1024 * 1024,
      windowsHide: true,
    });
    return { code: run.status, stdout: run.stdout ?? "", stderr: run.error ? `${run.stderr ?? ""}${run.error.message}` : run.stderr ?? "" };
  };
}

/** The probes' services of a stack: every running service whose name starts with `probe-`. */
function probeServices(compose) {
  const listed = compose(["ps", "--services"]);
  if (listed.code !== 0) return [];
  return listed.stdout.split(/\r?\n/).map((s) => s.trim()).filter((s) => s.startsWith("probe-"));
}

/** The probes' metrics from tests/bench/probes.mjs, or what says they weren't measured and why. */
async function readProbes(compose, services, since) {
  const none = (why) => ({
    githubRequestsPerHour: null,
    githubNote: why,
    snapshotRam: services.map((service) => ({ service, bytes: null, how: why })),
    notes: [],
  });
  if (!compose) return none("no Compose project to read the probes from (give --compose-project with --url)");
  if (services.length === 0) return none("no probe service is running in the Compose project");
  let module;
  try {
    module = await import("./probes.mjs");
  } catch (err) {
    return none(`tests/bench/probes.mjs can't be loaded (${err.code ?? mask(err.message)})`);
  }
  if (typeof module.probeMetrics !== "function") return none("tests/bench/probes.mjs exports no probeMetrics");
  try {
    const read = await module.probeMetrics({ compose, since, services });
    return {
      githubRequestsPerHour: typeof read?.githubRequestsPerHour === "number" ? read.githubRequestsPerHour : null,
      githubNote: String(read?.githubNote ?? ""),
      snapshotRam: Array.isArray(read?.snapshotRam) ? read.snapshotRam : [],
      notes: Array.isArray(read?.notes) ? read.notes.map(String) : [],
    };
  } catch (err) {
    return none(`the probes' reading failed: ${mask(err.message)}`);
  }
}

// ---------------------------------------------------------------------------------------------
// The report

function git(args) {
  const run = spawnSync("git", args, { cwd: root, encoding: "utf8", windowsHide: true });
  return run.status === 0 ? run.stdout.trim() : null;
}

/** The address of a stack as the report may name it: a local one as it is, any other not at all. */
function addressShown(url) {
  return /^https?:\/\/(localhost|127\.0\.0\.1|\[::1\])(:\d+)?$/.test(url) ? url : "an address given with --url (not written down)";
}

/** The command line as the report shows it: no token, no address that isn't local, no folder outside the repository. */
function commandShown() {
  const argv = process.argv.slice(2);
  const shown = argv.map((arg, i) => {
    if (arg.startsWith("mcy_") || argv[i - 1] === "--token") return "[token]";
    if (argv[i - 1] === "--url") return addressShown(arg) === arg ? arg : "[url]";
    if (argv[i - 1] === "--questions") return basename(arg);
    return arg;
  });
  return ["node tests/bench/run.mjs", ...shown].join(" ");
}

function report({ context, records, f, status, probes }) {
  const date = new Date().toISOString().slice(0, 10);
  const name = opts.label ?? MODE;
  const commit = git(["rev-parse", "--short", "HEAD"]) ?? "unknown";
  const dirty = (git(["status", "--porcelain", "--untracked-files=no"]) ?? "") !== "";
  const cap = context.cap;
  const vs = (value, target) => (value === null ? "—" : value <= target ? "within" : "over");
  const lines = [
    `# Load and cost, ${date} (${name})`,
    "",
    `\`${commandShown()}\`: ${opts.askers} askers at once, each with its own token, each asking ${opts.per} questions one after another with \`mercury ask --json\`; the figures come from each question's state (\`GET /v1/questions/{id}\`: the \`done\` row's \`duration_ms\`, \`cost_usd\` and \`judge_mode\`).`,
    "",
    "| | |",
    "|---|---|",
    `| Date | ${context.startedIso.replace("T", " ").slice(0, 19)} UTC |`,
    `| Commit | ${commit}${dirty ? " (with uncommitted changes)" : ""} |`,
    `| Mode | ${context.modeText} |`,
    `| Stack | ${context.stackText} |`,
    `| Askers × questions | ${opts.askers} × ${opts.per} = ${f.asked} (seed ${opts.seed}); at most ${f.mostAtOnce} in flight at once |`,
    `| Question set | ${context.source}; ${context.distinct} distinct asked |`,
    `| Tokens and the rate limit | ${context.tokensText} Held back by the limit: ${f.held} time(s), ${f.heldS} s of waiting, in no time below. |`,
    `| Run time | ${(context.askingMs / 1000).toFixed(1)} s of asking (${(f.asked / (context.askingMs / 60000)).toFixed(1)} questions a minute); ${(context.wholeMs / 1000).toFixed(1)} s in all${context.ownStack ? ", the stack's start and its people included" : ""} |`,
    "",
    "## Latency",
    "",
    "| Figure | p50 | p95 | Max | Questions | Target (five concurrent askers) |",
    "|---|---|---|---|---|---|",
    `| Server: the \`done\` row's \`duration_ms\` | ${secs(f.server.p50)} | ${secs(f.server.p95)} | ${secs(f.server.max)} | ${f.server.n} | p50 ≤ 8 s (${vs(f.server.p50, TARGETS.p50Ms)}), p95 ≤ 15 s (${vs(f.server.p95, TARGETS.p95Ms)}) |`,
    `| Client: wall time of \`mercury ask\`, every question that started | ${secs(f.wallAll.p50)} | ${secs(f.wallAll.p95)} | ${secs(f.wallAll.max)} | ${f.wallAll.n} | — |`,
    `| Client: wall time of \`mercury ask\`, the delivered ones | ${secs(f.wallDelivered.p50)} | ${secs(f.wallDelivered.p95)} | ${secs(f.wallDelivered.max)} | ${f.wallDelivered.n} | — |`,
    "",
    "Only a question that reaches `done` has a `duration_ms`: a refusal, a clarify or an error ends before it. The client's wall time adds the CLI's start, the request and the event stream to the server's time.",
    "",
    "## Cost",
    "",
    "| Figure | Value | Questions | Compared with |",
    "|---|---|---|---|",
    `| Mean per question (every ending carries its \`cost_usd\`) | ${usd(f.cost.mean)} | ${f.cost.n} | mean ≤ $0.02 (${vs(f.cost.mean, TARGETS.meanUsd)}) |`,
    `| Mean per delivered question | ${usd(f.cost.deliveredMean)} | ${f.cost.deliveredN} | — |`,
    `| Highest | ${usd(f.cost.max)} | | the cap per question, \`per_question_usd\` ${cap.value === null ? "unknown" : `$${cap.value.toFixed(2)}`} (${cap.from})${cap.value === null || f.cost.max === null ? "" : `: ${f.cost.max <= cap.value ? "within" : "over"}`} |`,
    `| Total of the run | ${usd(f.cost.total)} | ${f.cost.n} | ${status ? `the day's spend of everyone in mctl status: $${Number.isFinite(status.shown.everyone) ? status.shown.everyone.toFixed(4) : "?"} (it also counts what the probes' judge requests cost)` : "—"} |`,
    "",
    "## Endings",
    "",
    "| Ending | Questions |",
    "|---|---|",
    ...Object.entries(f.endings)
      .sort((a, b) => b[1] - a[1])
      .map(([ending, n]) => `| ${cell(ending)} | ${n} |`),
    "",
    "## Judge modes and the degraded share",
    "",
    "Counted the way `mctl status` counts them: each question that ended (`done`, `refused`, `clarify` or `error`) by the `judge_mode` its ending names, `none` when no judge answered; degraded is `chat` or `rules` over all of them.",
    "",
    "| Mode | Questions | Share |",
    "|---|---|---|",
    ...["jev", "chat", "rules", "none"].map((m) => `| ${m} | ${f.modes[m]} | ${floorShare(f.modes[m], f.ended)} |`),
    "",
    `Degraded share: **${floorShare(f.degraded, f.ended)}** (${f.degraded} of ${f.ended}).${status ? ` mctl status, read after the run: ${status.shown.degraded ?? "unread"} (${status.shown.modes.chat + status.shown.modes.rules} of ${Number.isFinite(status.shown.total) ? status.shown.total : "?"}): ${status.compared.same ? "the two agree" : `they disagree: ${status.compared.why}`}.` : " Not compared with mctl status here: an existing stack's 24-hour window counts other questions too, so run `mctl status` on it and read its judge modes beside these."}`,
    ...(MODE === "replay"
      ? ["", "On the replay stack the judge is a chat model behind the replay server (tests/it/config/models.yaml), so every judged question reads `chat`: the share is degraded by construction and says nothing of Jev; the live and `--url` readings measure it."]
      : []),
    "",
    `The CLI's JSON and the question's state agree on the status, the judge mode, the cost and \`duration_ms\` for ${f.agree} of the ${f.ended} questions that ended.${f.unread.length ? ` ${f.unread.length} question(s) had no state to read.` : ""}`,
    "",
    "## Probes",
    "",
    `- GitHub API requests per hour: ${probes.githubRequestsPerHour === null ? "not measured" : probes.githubRequestsPerHour.toFixed(1)}${probes.githubNote ? ` (${cell(probes.githubNote)})` : ""}; the limit per token is 5,000 an hour.`,
    ...(probes.snapshotRam.length === 0
      ? ["- Snapshot RAM: no probe service to read."]
      : ["", "| Probe service | Snapshot RAM | How |", "|---|---|---|", ...probes.snapshotRam.map((s) => `| ${cell(s.service)} | ${mib(s.bytes)} | ${cell(s.how)} |`)]),
    ...(probes.notes.length ? ["", ...probes.notes.map((n) => `- ${n}`)] : []),
    "",
    "## Each question",
    "",
    "No answer's text: what each question ended as, who answered and what it took.",
    "",
    "| # | Asker | Question | Ending | Judge | `duration_ms` | Client | Cost | Held back |",
    "|---|---|---|---|---|---|---|---|---|",
    ...records.map(
      (r, i) =>
        `| ${i + 1} | ${r.asker} | ${cell(r.q.id)} | ${cell(r.ending)} | ${r.id ? r.judge : "—"} | ${r.ms ?? "—"} | ${Math.round(r.wallMs)} ms | ${usd(r.cost)} | ${r.held ? `${r.held}× (${r.heldS} s)` : "—"} |`,
    ),
    "",
  ];
  const path = `tests/bench/reports/${date}-${name}.md`;
  return { path, text: mask(lines.join("\n")) };
}

function summaryLines({ context, f, status, probes, path }) {
  return [
    `${opts.askers} askers × ${opts.per} questions (${MODE}${opts.label ? `, ${opts.label}` : ""}), at most ${f.mostAtOnce} at once, ${(context.askingMs / 1000).toFixed(1)} s of asking`,
    `server duration_ms: p50 ${secs(f.server.p50)}, p95 ${secs(f.server.p95)} (${f.server.n} done); target p50 ≤ 8 s, p95 ≤ 15 s`,
    `client wall time: p50 ${secs(f.wallAll.p50)}, p95 ${secs(f.wallAll.p95)} (${f.wallAll.n} started); delivered p50 ${secs(f.wallDelivered.p50)}, p95 ${secs(f.wallDelivered.p95)}`,
    `cost: mean ${usd(f.cost.mean)}, highest ${usd(f.cost.max)}, total ${usd(f.cost.total)} (${f.cost.n}); cap per question ${context.cap.value === null ? "unknown" : `$${context.cap.value.toFixed(2)}`}, target mean ≤ $0.02`,
    `endings: ${Object.entries(f.endings).map(([e, n]) => `${e} ${n}`).join(", ")}`,
    `judge modes: ${Object.entries(f.modes).map(([m, n]) => `${m} ${n}`).join(", ")}`,
    status ? status.compared.line : `degraded share: this run ${floorShare(f.degraded, f.ended)} (${f.degraded} of ${f.ended}); mctl status not read on an existing stack`,
    ...(status && !status.compared.same ? [`  DISAGREE: ${status.compared.why}`] : []),
    `held back by the rate limit: ${f.held} time(s), ${f.heldS} s`,
    `probes: GitHub requests per hour ${probes.githubRequestsPerHour === null ? "not measured" : probes.githubRequestsPerHour.toFixed(1)}${probes.githubNote ? ` (${probes.githubNote})` : ""}; snapshot RAM ${probes.snapshotRam.map((s) => `${s.service} ${mib(s.bytes)}`).join(", ") || "no probe"}`,
    `the report: ${path}`,
  ].map((line) => mask(line));
}

// ---------------------------------------------------------------------------------------------
// The run

/** The askers: their names, tokens and the questions each asks, in the seeded order. */
function plans(askers, set) {
  const order = shuffled(set.questions, opts.seed);
  return askers.map((asker, a) => ({
    ...asker,
    plan: Array.from({ length: opts.per }, (_, j) => order[(a * opts.per + j) % order.length]),
  }));
}

/** The figure `per_question_usd` of a models.yaml, or null. */
function capIn(file) {
  try {
    const m = readFileSync(file, "utf8").match(/^\s*per_question_usd:\s*([\d.]+)/m);
    return m ? Number(m[1]) : null;
  } catch {
    return null;
  }
}

async function bench({ base, askers, context, compose, services }) {
  const set = questionSet();
  if (set.questions.length === 0) throw new Error("no question to ask");
  const planned = plans(askers, set);
  context.source = set.source;
  context.distinct = new Set(planned.flatMap((a) => a.plan.map((q) => q.id))).size;
  console.log(mask(`\n${opts.askers} askers × ${opts.per} questions on ${MODE === "url" ? addressShown(base) : base}; ${set.source}`));

  const since = new Date().toISOString();
  const began = performance.now();
  const runs = await askAll(base, planned, opts.project);
  context.askingMs = performance.now() - began;

  const records = [];
  for (const run of runs) {
    const read = run.json?.id ? await stateOf(base, run.token, run.json.id) : null;
    records.push(settle(run, read));
  }
  records.sort((a, b) => a.startedAt - b.startedAt);
  const f = figures(records);

  let status = null;
  if (context.ownStack) {
    const shown = readStatus(admin(["status"]));
    if (Number.isFinite(shown.cap)) context.cap = { value: shown.cap, from: "mctl status" };
    status = { shown, compared: compareStatus(f, shown) };
  }

  const probes = await readProbes(compose, services, since);
  context.wholeMs = performance.now() - context.t0;
  const written = report({ context, records, f, status, probes });

  // no token in anything the run printed or wrote, and no answer's text in the report
  const outputs = runs.map((r) => r.stdout + r.stderr).join("\n");
  const leaks = leaked(outputs) + leaked(written.text);
  const answersShown = records.filter((r) => r.answer && r.answer.length > 20 && written.text.includes(r.answer)).length;
  const writes = leaks === 0 && answersShown === 0 && f.started > 0;
  if (writes) {
    mkdirSync(join(root, "tests/bench/reports"), { recursive: true });
    writeFileSync(join(root, written.path), written.text);
  }
  console.log(`\n${summaryLines({ context, f, status, probes, path: writes ? written.path : "not written" }).join("\n")}`);

  const problems = [];
  if (f.started === 0) problems.push("no question started, so nothing was measured");
  if (leaks > 0) problems.push(`a token is in the CLI's output or the report (${leaks}); the report was not written`);
  if (answersShown > 0) problems.push(`an answer's text is in the report (${answersShown}); the report was not written`);
  if (f.unread.length > 0) problems.push(`${f.unread.length} question(s) had no state to read: ${[...new Set(f.unread.map((r) => r.unread))].join("; ")}`);
  if (f.disagree.length > 0) problems.push(`the CLI's JSON and the state disagree for ${f.disagree.length} question(s)`);
  if (status && !status.compared.same) problems.push(`the degraded share disagrees with mctl status: ${status.compared.why}`);
  for (const problem of problems) console.error(mask(`bench: ${problem}`));
  return problems.length === 0;
}

const t0 = performance.now();
const startedIso = new Date().toISOString();

if (MODE === "url") {
  // an existing stack: its own tokens, its own limits; Docker only to read the probes of a named project
  const askers = Array.from({ length: opts.askers }, (_, a) => ({ name: `asker${a + 1}`, token: opts.tokens[a % opts.tokens.length] }));
  if (opts.tokens.length < opts.askers) {
    console.warn(
      `bench: ${opts.askers} askers share ${opts.tokens.length} token(s); the per-token rate limit (300 an hour, burst 50 in config-examples/policies.yaml; 30 and 5 in the private and strict presets) may hold them back. Each held-back question waits as the server says and is asked again; the waits are in no time measured, but the load is lower than ${opts.askers} at once.`,
    );
  }
  let compose = null;
  let services = [];
  if (opts.composeProject) {
    compose = composeRunner((args) => ({
      command: "docker",
      args: ["compose", "-p", opts.composeProject, ...opts.composeFiles.flatMap((file) => ["-f", file]), ...args],
      cwd: join(root, "deploy/compose"),
      env: {},
    }));
    services = probeServices(compose);
  }
  const context = {
    t0,
    startedIso,
    ownStack: false,
    modeText: `an existing stack (\`--url\`), project \`${opts.project}\`, with the stack's own models and configuration`,
    stackText: `${addressShown(opts.url)}${opts.composeProject ? `; its probes read from the Compose project \`${opts.composeProject}\`` : ""}`,
    tokensText: `${opts.tokens.length} token(s) given for ${opts.askers} askers${opts.tokens.length < opts.askers ? " (shared round the askers)" : " (one each)"}; the stack's own per-token limit applies (config-examples has 300 an hour, burst 50; the private and strict presets 30 and 5).`,
    cap: { value: capIn(join(root, "config-examples/models.yaml")), from: "config-examples/models.yaml; the stack's own models.yaml decides" },
  };
  let ok = false;
  try {
    ok = await bench({ base: opts.url, askers, context, compose, services });
  } catch (err) {
    console.error(`\nbench: ${mask(err.message)}`);
  }
  process.exitCode = ok ? 0 : 1;
} else {
  const project = process.env.MERCURY_IT_PROJECT;
  const work = join(root, "target", project === "mercury-it" ? "it" : project);
  await withStack(async (tokens) => {
    // the project managers of the run's people, then more of them until every asker has a token of its own
    const askers = [
      { name: "pm", token: tokens.pm },
      { name: "spender", token: tokens.spender },
    ];
    for (let a = askers.length + 1; askers.length < opts.askers; a += 1) {
      const email = `asker${a}@example.com`;
      admin(["user", "add", "--email", email, "--name", `Bench Asker ${a}`, "--role", "pm"]);
      admin(["user", "grant", "--email", email, "--projects", opts.project]);
      const token = admin(["token", "create", "--email", email, "--label", "bench"]).match(/mcy_[A-Za-z0-9_-]+/)?.[0];
      if (!token) throw new Error(`token create printed no token for ${email}`);
      secrets.push(token);
      askers.push({ name: `asker${a}`, token });
    }
    askers.length = opts.askers;
    const policies = readFileSync(join(work, "config/policies.yaml"), "utf8");
    const perHour = policies.match(/questions_per_hour: (\d+)/)?.[1] ?? "?";
    const burst = policies.match(/burst: (\d+)/)?.[1] ?? "?";
    const images = [...new Set(composeRunner(composeCall)(["config", "--images"]).stdout.split(/\r?\n/).filter((i) => /\/mercury-/.test(i)))].sort();
    const context = {
      t0,
      startedIso,
      ownStack: true,
      modeText:
        MODE === "live"
          ? "live: a fresh stack of its own with the real models of config-examples/models.yaml (Jev and the chat models on OpenRouter)"
          : "replay: a fresh stack of its own with the replay server as the models (tests/it/replay); costs nothing",
      stackText: `Compose project \`${project}\` on ${stack.URL}, the fixture repository (delta) and its probe in dev mode; images ${images.join(", ") || "unknown"}`,
      tokensText: `${opts.askers} project managers, each with a token of its own (the run's \`pm\` and \`spender\`, then \`asker3\` on); the run's limits are raised to ${perHour} an hour, burst ${burst}, per token, so ${opts.per} questions each never meet them.`,
      cap: { value: capIn(join(work, "config/models.yaml")), from: "the stack's models.yaml" },
    };
    return bench({ base: stack.URL, askers, context, compose: composeRunner(composeCall), services: ["probe-delta"] });
  });
}

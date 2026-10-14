// The probes' side of the load benchmark: how many requests the sync probes send to GitHub in an hour,
// and how much memory each probe's code snapshot takes. It reads the probes' own logs, looks inside
// their containers and asks `docker compose stats`; it changes nothing on the stack.
//
//   import { probeMetrics } from "./probes.mjs";
//   const probes = await probeMetrics({ compose, since: startedAt });
//
// `compose(args)` runs `docker compose <args>` on the measured stack and returns `{ code, stdout, stderr }`
// (or a promise of one); `since` is the ISO time the run started; `services` names the probe services to
// measure, every `probe-*` service of the stack when it is left out.
//
// The GitHub figure is an estimate, not a count: the sync probe logs no line per request, no request
// count and no rate-limit figure, only one line per round with that round's counts. Each logged round is
// turned back into the requests the probe's code sends for it (`roundRequests`), and `githubNote` says
// what was counted and how it was scaled to an hour.
//
//   node tests/bench/probes.mjs --self-test     the parsers, on lines in the formats the probe writes

import { pathToFileURL } from "node:url";

/** Where a sync + serve pair keeps the code snapshot: a tmpfs volume, so RAM only. */
export const SNAPSHOT_MOUNT = "/repo";

/** The sync probe's own defaults, for a log that no longer holds its start line. */
const DEFAULT_CODE_INTERVAL_S = 60;
const DEFAULT_META_INTERVAL_S = 300;
/** How long the probe takes a token that couldn't read the code at its word before it asks for the head again. */
const HEAD_RECHECK_S = 600;
/** The GitHub client's page size, and the most pages it reads of one list. */
const PER_PAGE = 100;
const MAX_LIST_PAGES = 10;
/** The requests of a metadata round with nothing in it: open and closed pull requests, open and closed issues, runs, releases. */
const EMPTY_META_ROUND = 6;
/** GitHub's limit for one token, an hour. */
const GITHUB_LIMIT_PER_HOUR = 5000;

// ---------------------------------------------------------------------------------------------------
// Secrets: nothing this module returns may hold one, whatever a log or an error printed.

/** `text` with anything that looks like a secret replaced by `[secret]`. */
export function maskSecrets(text) {
  return (
    String(text)
      // credentials in a URL
      .replace(/\b([a-z][a-z0-9+.-]*:\/\/)[^\s/@]+@/gi, "$1[secret]@")
      // GitHub tokens, this project's tokens and model API keys
      .replace(/\b(?:gh[pousr]_|github_pat_|mcy_|sk-)[A-Za-z0-9_-]{8,}/g, "[secret]")
      .replace(/\b(bearer\s+|token[=:]\s*)[^\s"']{8,}/gi, "$1[secret]")
      // a long random-looking run: mixed case and digits (a commit id is lowercase, a name has no capitals)
      .replace(/[A-Za-z0-9+/_=-]{32,}/g, (run) =>
        /[A-Z]/.test(run) && /[a-z]/.test(run) && /\d/.test(run) ? "[secret]" : run,
      )
  );
}

// ---------------------------------------------------------------------------------------------------
// Log lines.

/**
 * One line of `docker compose logs`, with or without the service's prefix (`probe-x-1  | `) and the
 * timestamp `--timestamps` adds: `{ at: Date | null, text }`.
 */
export function splitLogLine(line) {
  let text = String(line).replace(/\r$/, "");
  const prefixed = text.match(/^\S+\s+\|\s?(.*)$/);
  if (prefixed) text = prefixed[1];
  const stamped = text.match(/^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(\.\d+)?(Z|[+-]\d\d:\d\d)\s(.*)$/);
  if (!stamped) return { at: null, text };
  // Date takes milliseconds; Docker writes nanoseconds
  const fraction = (stamped[2] ?? "").slice(0, 4);
  const at = new Date(`${stamped[1]}${fraction}${stamped[3]}`);
  return { at: Number.isNaN(at.getTime()) ? null : at, text: stamped[4] };
}

const HEAD = "([0-9a-f]{7,12})";
/** The lines of the probe that this module reads, in the probe's own formats (crates/mercury-probe). */
const PATTERNS = [
  // main.rs, `sync()`: the start line, with the intervals
  [
    "start",
    /^mercury-probe (\S+) \(sync\): project (\S+?), .*?the mode every (\d+) s, the metadata every (\d+) s, snapshots up to (\d+) MB/,
    (m) => ({ mode: "sync", version: m[1], project: m[2], codeInterval: +m[3], metaInterval: +m[4], snapshotMaxMb: +m[5] }),
  ],
  // main.rs, `serve()`: the start line of `dev` and `serve`
  ["start", /^mercury-probe (\S+) \((dev|serve)\): MCP at \/mcp/, (m) => ({ mode: m[2], version: m[1] })],
  // sync/mod.rs, `ModeReport`: logged when the mode or the manifest changed
  [
    "mode",
    new RegExp(
      `^mercury-probe: access (metadata|code|code_model)(?: \\(limited by ([^)]+)\\))?, the (repository's manifest|manifest's copy in projects\\.yaml)(?: at ${HEAD})?`,
    ),
    (m) => ({ access: m[1], limitedBy: m[2] ?? null, head: m[4] ?? null }),
  ],
  // sync/mod.rs, `CodeReport` (and `Unpacked` in sync/snapshot.rs): a new head
  [
    "snapshot",
    new RegExp(
      `^mercury-probe: snapshot ${HEAD} in use: (\\d+) files \\(([\\d.]+) MB\\), (\\d+) excluded, (\\d+) entries skipped, in ([\\d.]+) s; history: (\\d+) commits in 90 days`,
    ),
    (m) => ({ head: m[1], files: +m[2], mb: +m[3], commits: +m[7] }),
  ],
  ["history", new RegExp(`^mercury-probe: history at ${HEAD}: (\\d+) commits in 90 days`), (m) => ({ head: m[1], commits: +m[2] })],
  [
    "capped",
    new RegExp(`^mercury-probe: snapshot ${HEAD} is larger than PROBE_SNAPSHOT_MAX_MB \\((\\d+) MB\\)`),
    (m) => ({ head: m[1], maxMb: +m[2] }),
  ],
  ["cleared", /^mercury-probe: the mode is below code: the snapshots and the history are deleted/, () => ({})],
  // sync/mod.rs, `MetaReport`: one line per metadata round
  [
    "metadata",
    /^mercury-probe: metadata ready: (\d+) pull requests \((\d+) fetched one by one\), (\d+) issues, (\d+) runs, (\d+) releases, in ([\d.]+) s/,
    (m) => ({ pulls: +m[1], fetched: +m[2], issues: +m[3], runs: +m[4], releases: +m[5] }),
  ],
  // sync/mod.rs, `report_error`: a failed round, logged once until it changes
  [
    "error",
    /^mercury-probe: the round wrote nothing: (.*)$/,
    (m) => ({ message: m[1], rateLimited: /the rate limit is spent/.test(m[1]) }),
  ],
];

/** One probe log line as `{ at, kind, ... }`, or null for a line this module doesn't read. */
export function parseProbeLine(line) {
  const { at, text } = splitLogLine(line);
  for (const [kind, pattern, fields] of PATTERNS) {
    const m = text.match(pattern);
    if (m) return { at, kind, ...fields(m) };
  }
  return null;
}

/** Every line of a probe's log this module reads, in order. */
export function parseProbeLog(text) {
  return String(text)
    .split("\n")
    .map(parseProbeLine)
    .filter(Boolean);
}

// ---------------------------------------------------------------------------------------------------
// GitHub requests, rebuilt from the rounds (crates/mercury-probe/src/sync/mod.rs and sync/github.rs).

/**
 * The requests one logged round sent to the GitHub API, as the probe's code sends them. `previousCommits`
 * is the history's commit count at the code round before, null when this process logged none.
 *
 * - a metadata round: the open and the closed pull requests, the open and the closed issues (GitHub's
 *   issue list holds the pull requests too), the runs and the releases, one page each and one more per
 *   further 100 items; then one request per pull request "fetched one by one" (its file list in the code
 *   modes, its detail in `metadata`; a file list over 100 files takes more pages, not counted).
 * - a new head ("snapshot … in use"): the manifest at the head, the tarball (whose download from
 *   codeload.github.com is not an API request), and the history: a compare and one request per new
 *   commit. At a process's first code round the history may have been fetched whole: the commit list and
 *   one request per commit of the 90 days, an upper bound (a history left from the last run costs only
 *   the compare).
 * - "history at …": the history alone; "is larger than PROBE_SNAPSHOT_MAX_MB": the manifest and the
 *   tarball it stopped reading; "the mode is below code": nothing.
 */
export function roundRequests(event, previousCommits = null) {
  const pages = (items, lists) => Math.min(lists * MAX_LIST_PAGES, lists + Math.floor(items / PER_PAGE));
  const history = (commits) =>
    previousCommits == null
      ? Math.min(MAX_LIST_PAGES, 1 + Math.floor(commits / PER_PAGE)) + commits
      : 1 + Math.max(1, commits - previousCommits);
  switch (event.kind) {
    case "metadata":
      return (
        pages(event.pulls, 2) +
        pages(event.issues + event.pulls, 2) +
        Math.min(MAX_LIST_PAGES, Math.max(1, Math.ceil(event.runs / PER_PAGE))) +
        1 +
        event.fetched
      );
    case "snapshot":
      return 2 + history(event.commits);
    case "history":
      return history(event.commits);
    case "capped":
      return 2;
    default:
      return 0;
  }
}

const round1 = (n) => Math.round(n * 10) / 10;

/**
 * The GitHub API requests an hour of one sync probe, estimated from its log events (`parseProbeLog`) of
 * the run between `from` and `to`. The periodic work is scaled by the intervals of the probe's start
 * line: every tick asks for the repository and the head (the head every 10 minutes only while the token
 * can't read the code), every metadata round as many requests as the mean of the run's rounds. Head
 * moves happen when someone pushes: they are scaled by the run's length. The first rounds after a start
 * fetch everything and are left out of the hourly figure (`startup`).
 *
 * `{ perHour, ticksPerHour, headPerHour, metaPerHour, metaRequests, metaBasis, moves, moveRequests,
 *    hours, startup, downloads, errors, rateLimited, codeInterval, metaInterval, intervalsAssumed,
 *    lastSnapshot }`
 */
export function estimateGithubRequests(events, { from, to }) {
  let start = null;
  // a process's first rounds are known only after its start line; a log without one has none
  let firstMeta = false;
  let firstCode = false;
  let commits = null;
  let limitedByToken = false;
  let firstMetaEvent = null;
  let lastSteadyBefore = null;
  let lastSnapshot = null;
  const steady = [];
  const out = { moves: 0, moveRequests: 0, startup: 0, downloads: 0, errors: [], rateLimited: 0 };
  for (const event of events) {
    const inRun = !event.at || (event.at >= from && event.at <= to);
    if (event.at && event.at > to) break;
    switch (event.kind) {
      case "start":
        if (event.mode === "sync") {
          start = event;
          firstMeta = true;
          firstCode = true;
          commits = null;
        }
        break;
      case "mode":
        limitedByToken = event.limitedBy === "the token";
        break;
      case "error":
        if (inRun) {
          out.errors.push(event.message);
          if (event.rateLimited) out.rateLimited += 1;
        }
        break;
      case "metadata": {
        const requests = roundRequests(event);
        if (firstMeta) {
          firstMeta = false;
          firstMetaEvent = event;
          if (inRun) out.startup += requests;
        } else if (inRun) steady.push(requests);
        else lastSteadyBefore = requests;
        break;
      }
      case "snapshot":
      case "history":
      case "capped":
      case "cleared": {
        const requests = roundRequests(event, commits);
        commits = event.kind === "snapshot" || event.kind === "history" ? event.commits : null;
        if (event.kind === "snapshot") lastSnapshot = event;
        if (firstCode) {
          firstCode = false;
          if (inRun) out.startup += requests;
        } else if (inRun) {
          out.moves += 1;
          out.moveRequests += requests;
        }
        if (inRun && (event.kind === "snapshot" || event.kind === "capped")) out.downloads += 1;
        break;
      }
      default:
        break;
    }
  }
  const codeInterval = start?.codeInterval ?? DEFAULT_CODE_INTERVAL_S;
  const metaInterval = start?.metaInterval ?? DEFAULT_META_INTERVAL_S;
  // the run's steady part starts with the run, or with the probe when it started during the run
  const begins = start?.at && start.at > from ? start.at : from;
  const hours = Math.max(0, (to - begins) / 3_600_000);

  let metaRequests;
  let metaBasis;
  if (steady.length) {
    metaRequests = steady.reduce((a, b) => a + b, 0) / steady.length;
    metaBasis = steady.length === 1 ? "the one round logged during the run" : `the mean of the ${steady.length} rounds logged during the run`;
  } else if (lastSteadyBefore != null) {
    metaRequests = lastSteadyBefore;
    metaBasis = "the last round logged before the run (none during it)";
  } else if (firstMetaEvent) {
    metaRequests = roundRequests({ ...firstMetaEvent, fetched: 0 });
    metaBasis = "the lists of the first round after the start, without its one-by-one fetches (no later round logged)";
  } else {
    metaRequests = EMPTY_META_ROUND;
    metaBasis = "assumed: the six lists of an empty round (no metadata round logged)";
  }
  const ticksPerHour = 3600 / codeInterval;
  const headPerHour = limitedByToken ? Math.min(ticksPerHour, 3600 / HEAD_RECHECK_S) : ticksPerHour;
  const metaPerHour = (3600 / metaInterval) * metaRequests;
  const movesPerHour = hours > 0 ? out.moveRequests / hours : 0;
  return {
    perHour: ticksPerHour + headPerHour + metaPerHour + movesPerHour,
    ticksPerHour,
    headPerHour,
    metaPerHour,
    metaRequests,
    metaBasis,
    movesPerHour,
    hours,
    codeInterval,
    metaInterval,
    intervalsAssumed: !start,
    limitedByToken,
    lastSnapshot,
    ...out,
  };
}

/** One sync probe's estimate in a sentence. */
export function describeEstimate(service, e) {
  const interval = `PROBE_CODE_INTERVAL ${e.codeInterval} s${e.intervalsAssumed ? ", the default: no start line in the log" : ""}`;
  const ticks = e.limitedByToken
    ? `${round1(e.ticksPerHour)} ticks × 1 (the repository; ${interval}) + ${round1(e.headPerHour)} head polls (the token can't read the code: one every 10 minutes)`
    : `${round1(e.ticksPerHour)} ticks × 2 (the repository and the head; ${interval})`;
  const parts = [
    `${service}: about ${Math.round(e.perHour)} an hour = ${ticks}`,
    `${round1(3600 / e.metaInterval)} metadata rounds × ${round1(e.metaRequests)} (PROBE_META_INTERVAL ${e.metaInterval} s; ${e.metaBasis})`,
    `${e.moves} head move${e.moves === 1 ? "" : "s"} in the run's ${round1(e.hours * 60)} minutes (${e.moveRequests} requests, ${round1(e.movesPerHour)} an hour)`,
  ];
  let text = parts.join(" + ");
  if (e.startup) text += `; the first rounds after its start, left out of the hour: about ${e.startup} requests`;
  if (e.downloads) text += `; ${e.downloads} snapshot download${e.downloads === 1 ? "" : "s"} from codeload.github.com`;
  if (e.errors.length) {
    const distinct = [...new Set(e.errors)];
    text += `; ${e.errors.length} failed round${e.errors.length === 1 ? "" : "s"} logged (${distinct.join("; ")})`;
  }
  return text;
}

// ---------------------------------------------------------------------------------------------------
// Memory: mounts, `df`, `docker compose stats`.

/** A size as Docker prints it (`12.5MiB`, `5.73kB`, `0B`) in bytes; null when it isn't one. */
export function toBytes(text) {
  const m = String(text).trim().match(/^([\d.]+)\s*([kKMGTP]?i?B)$/);
  if (!m) return null;
  const unit = m[2];
  const power = { B: 0, K: 1, k: 1, M: 2, G: 3, T: 4, P: 5 }[unit[0]];
  const base = unit.includes("i") ? 1024 : 1000;
  return Math.round(Number(m[1]) * base ** (unit === "B" ? 0 : power));
}

const unescapeMount = (path) => path.replace(/\\([0-7]{3})/g, (_, octal) => String.fromCharCode(parseInt(octal, 8)));

/** The mount at `mount` in a `/proc/self/mountinfo`, the last one (it hides those under it): `{ device, fstype, source }` or null. */
export function parseMountinfo(text, mount) {
  let found = null;
  for (const line of String(text).split(/\r?\n/)) {
    const [left, right] = line.split(" - ");
    if (!right) continue;
    const fields = left.split(" ");
    if (unescapeMount(fields[4] ?? "") !== mount) continue;
    const [fstype, source] = right.split(" ");
    found = { device: fields[2], fstype, source };
  }
  return found;
}

/** `df -B1 <path>` (or `df -k` with `blockSize` 1024): `{ size, used, available, mountedOn }` in bytes, or null. */
export function parseDf(stdout, blockSize = 1) {
  const lines = String(stdout).trim().split(/\r?\n/).filter((line) => line.trim());
  if (lines.length < 2) return null;
  const fields = lines[lines.length - 1].trim().split(/\s+/);
  if (fields.length < 6) return null;
  const [size, used, available] = fields.slice(1, 4).map(Number);
  if (![size, used, available].every(Number.isFinite)) return null;
  return { size: size * blockSize, used: used * blockSize, available: available * blockSize, mountedOn: fields.slice(5).join(" ") };
}

/** `docker compose stats --no-stream --format json` (or `docker stats`): `[{ name, memBytes, limitBytes, memUsage }]`. */
export function parseStats(stdout) {
  return jsonRecords(stdout).map((row) => {
    const [used, limit] = String(row.MemUsage ?? "").split("/");
    return { name: row.Name ?? row.Container ?? "", memBytes: toBytes(used ?? ""), limitBytes: toBytes(limit ?? ""), memUsage: row.MemUsage ?? "" };
  });
}

/** `docker compose ps --format json`: one object per line (Compose 2.21 and later) or one array. */
export function parsePs(stdout) {
  return jsonRecords(stdout).map((row) => ({
    service: row.Service ?? "",
    name: row.Name ?? "",
    state: row.State ?? "",
    command: String(row.Command ?? "").replace(/^"|"$/g, ""),
  }));
}

function jsonRecords(stdout) {
  const text = String(stdout).trim();
  if (!text) return [];
  if (text.startsWith("[")) return JSON.parse(text);
  return text
    .split(/\r?\n/)
    .filter((line) => line.trim().startsWith("{"))
    .map((line) => JSON.parse(line));
}

/** The probe's mode from its container's command: `sync`, `serve`, `dev`, or null for another image. */
export function probeMode(command) {
  const m = String(command).match(/\bmercury-probe(?:\s+(sync|serve|dev)\b)?/);
  return m ? (m[1] ?? "serve") : null;
}

/** The egress proxy's tunnels to GitHub in its log between `from` and `to` (its lines naming `CONNECT host:443`). */
export function countProxyTunnels(text, from, to) {
  const counts = { "api.github.com": 0, "codeload.github.com": 0 };
  for (const line of String(text).split("\n")) {
    const { at, text: rest } = splitLogLine(line);
    if (at && (at < from || at > to)) continue;
    const m = rest.match(/\bCONNECT\s+(api\.github\.com|codeload\.github\.com):443\b/);
    if (m) counts[m[1]] += 1;
  }
  return counts;
}

const mib = (bytes) => `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
const firstLine = (text) => String(text).trim().split(/\r?\n/)[0] || "no output";

// ---------------------------------------------------------------------------------------------------
// The measurement.

/**
 * The probes' GitHub requests an hour and their snapshots' memory, on the stack `compose` reaches:
 * `{ githubRequestsPerHour, githubNote, snapshotRam: [{ service, bytes, how }], notes }`. `now` (a time)
 * ends the run's window; it is the present when left out.
 */
export async function probeMetrics({ compose, since, services, now } = {}) {
  if (typeof compose !== "function") throw new Error("probeMetrics needs compose(args), which runs docker compose on the stack");
  const to = now ? new Date(now) : new Date();
  const from = since ? new Date(since) : new Date(0);
  if (Number.isNaN(from.getTime()) || Number.isNaN(to.getTime())) throw new Error("since and now must be times");
  const notes = [];
  const run = async (args) => {
    try {
      const result = await compose(args);
      return { code: result?.code ?? 1, stdout: String(result?.stdout ?? ""), stderr: String(result?.stderr ?? "") };
    } catch (err) {
      return { code: 1, stdout: "", stderr: err.message };
    }
  };

  // the stack's containers, stopped ones too, with their commands
  let containers = [];
  const ps = await run(["ps", "-a", "--format", "json", "--no-trunc"]);
  if (ps.code !== 0) notes.push(`docker compose ps failed: ${firstLine(ps.stderr)}`);
  else {
    try {
      containers = parsePs(ps.stdout);
    } catch {
      notes.push("docker compose ps printed no JSON, so the probes' modes are unknown");
    }
  }
  const measured = services?.length
    ? [...services]
    : [...new Set(containers.map((c) => c.service).filter((s) => s.startsWith("probe-")))].sort();
  if (!measured.length) notes.push("the stack has no probe-* service");
  const container = (service) => containers.find((c) => c.service === service) ?? null;
  const modeOf = (service) => probeMode(container(service)?.command ?? "");

  // GitHub: the sync probes' logs
  const syncs = measured.filter((service) => modeOf(service) === "sync");
  const estimates = [];
  const syncEvents = {};
  for (const service of syncs) {
    const logs = await run(["logs", "--no-color", "--no-log-prefix", "--timestamps", service]);
    if (logs.code !== 0) {
      notes.push(`${service}: its log can't be read (${firstLine(logs.stderr)})`);
      continue;
    }
    const events = parseProbeLog(logs.stdout);
    syncEvents[service] = events;
    estimates.push([service, estimateGithubRequests(events, { from, to })]);
  }
  let githubRequestsPerHour = null;
  let githubNote;
  if (!syncs.length) {
    const modes = measured.map((service) => `${service} runs ${modeOf(service) ?? "an unknown command"}`).join(", ");
    githubNote = `No sync probe on this stack${modes ? ` (${modes})` : ""}: nothing on it asks GitHub, so there is no figure.`;
  } else if (!estimates.length) {
    githubNote = "The sync probes' logs can't be read, so there is no figure.";
  } else {
    githubRequestsPerHour = Math.round(estimates.reduce((sum, [, e]) => sum + e.perHour, 0));
    githubNote =
      "Estimated from the sync probes' round lines, not counted: the probe logs no line per GitHub request, no request count " +
      "and no rate-limit figure. Each round it logged is turned back into the requests its code sends (a tick: the repository " +
      "and the head; a metadata round: six lists, a page per further 100 items, one request per pull request fetched one by one; " +
      "a new head: the manifest, the tarball, a compare and one request per new commit). Ticks and metadata rounds are scaled to " +
      "an hour by the intervals in the probe's start line, head moves by the run's length. Replies answered 304 from the probe's " +
      `ETag cache are included, and GitHub doesn't charge those to its ${GITHUB_LIMIT_PER_HOUR.toLocaleString("en")}-an-hour ` +
      `limit, so this is an upper bound on the limit's use${estimates.length > 1 ? `; the figure is the sum over ${estimates.length} probes` : ""}. ` +
      `${estimates.map(([service, e]) => describeEstimate(service, e)).join(". ")}.`;
    for (const [service, e] of estimates) {
      if (e.rateLimited) notes.push(`${service}: ${e.rateLimited} round(s) during the run found the rate limit spent`);
      if (e.lastSnapshot) {
        notes.push(
          `${service}: its last snapshot unpacked to ${e.lastSnapshot.mb} MB in ${e.lastSnapshot.files} files` +
            (e.lastSnapshot.at ? ` (logged ${e.lastSnapshot.at.toISOString()})` : ""),
        );
      }
    }
  }

  // the egress proxy, when the stack has one: its tunnels are a floor, since one tunnel carries many requests
  const proxy = container("egress-proxy");
  if (proxy && syncs.length) {
    const logs = await run(["logs", "--no-color", "--no-log-prefix", "--timestamps", "egress-proxy"]);
    if (logs.code === 0) {
      const tunnels = countProxyTunnels(logs.stdout, from, to);
      notes.push(
        `egress-proxy: ${tunnels["api.github.com"]} tunnels to api.github.com and ${tunnels["codeload.github.com"]} to ` +
          "codeload.github.com during the run (a kept-alive tunnel carries many requests, so these are lower bounds; the " +
          "codeload ones are snapshot downloads)",
      );
    }
  }

  // memory: the tmpfs under /repo when there is one, the container's memory otherwise
  const snapshotRam = [];
  const tmpfsDevices = {};
  for (const service of measured) {
    const c = container(service);
    if (!c) {
      snapshotRam.push({ service, bytes: null, how: "no container of this service on the stack" });
      continue;
    }
    if (c.state !== "running") {
      snapshotRam.push({ service, bytes: null, how: `the container isn't running (${c.state || "state unknown"})` });
      continue;
    }
    const stats = await run(["stats", "--no-stream", "--format", "json", service]);
    let memory = null;
    if (stats.code === 0) {
      try {
        const rows = parseStats(stats.stdout).filter((row) => row.memBytes != null);
        if (rows.length) memory = { bytes: rows.reduce((sum, row) => sum + row.memBytes, 0), usage: rows.map((row) => row.memUsage).join(", ") };
      } catch {
        // no JSON: memory stays unknown
      }
    }
    const mountinfo = await run(["exec", "-T", service, "cat", "/proc/self/mountinfo"]);
    const mount = mountinfo.code === 0 ? parseMountinfo(mountinfo.stdout, SNAPSHOT_MOUNT) : null;
    if (mount?.fstype === "tmpfs") {
      let df = await run(["exec", "-T", service, "df", "-B1", SNAPSHOT_MOUNT]);
      let usage = df.code === 0 ? parseDf(df.stdout) : null;
      if (!usage) {
        df = await run(["exec", "-T", service, "df", "-k", SNAPSHOT_MOUNT]);
        usage = df.code === 0 ? parseDf(df.stdout, 1024) : null;
      }
      if (usage) {
        (tmpfsDevices[mount.device] ??= []).push(service);
        snapshotRam.push({
          service,
          bytes: usage.used,
          how:
            `df -B1 ${SNAPSHOT_MOUNT} inside the container: the tmpfs's used bytes (${mib(usage.used)} of ${mib(usage.size)})` +
            (memory ? `; the container itself uses ${mib(memory.bytes)} (docker compose stats)` : ""),
        });
        continue;
      }
      notes.push(`${service}: ${SNAPSHOT_MOUNT} is a tmpfs, but df can't read it (${firstLine(df.stderr)})`);
    }
    const why =
      mountinfo.code !== 0
        ? `its mounts can't be read (${firstLine(mountinfo.stderr)})`
        : mount
          ? `${SNAPSHOT_MOUNT} is a ${mount.fstype} mount, not a tmpfs`
          : `${SNAPSHOT_MOUNT} is no mount here`;
    const mode = modeOf(service);
    const reading =
      mode === "dev"
        ? " (a dev probe reads its repository from a mounted folder: no snapshot in RAM)"
        : mount?.fstype && mount.fstype !== "tmpfs"
          ? " (a snapshot here would be on a disk, not in RAM: check the Compose file)"
          : "";
    if (mode !== "dev" && mount && mount.fstype !== "tmpfs") notes.push(`${service}: ${SNAPSHOT_MOUNT} is not a tmpfs`);
    snapshotRam.push(
      memory
        ? {
            service,
            bytes: memory.bytes,
            how: `${why}${reading}: the container's whole memory from docker compose stats (${memory.usage}), its in-memory index included`,
          }
        : {
            service,
            bytes: null,
            how: `${why}${reading}, and docker compose stats gave no memory (${firstLine(stats.code === 0 ? "no JSON" : stats.stderr)})`,
          },
    );
  }
  for (const [device, sharing] of Object.entries(tmpfsDevices)) {
    if (sharing.length > 1) notes.push(`${sharing.join(" and ")} share one tmpfs (device ${device}): its bytes are in RAM once`);
  }

  return {
    githubRequestsPerHour,
    githubNote: maskSecrets(githubNote),
    snapshotRam: snapshotRam.map((entry) => ({ ...entry, how: maskSecrets(entry.how) })),
    notes: notes.map(maskSecrets),
  };
}

// ---------------------------------------------------------------------------------------------------
// The self-test: `node tests/bench/probes.mjs --self-test`.

async function selfTest() {
  const failures = [];
  const check = (name, got, want) => {
    const ok = JSON.stringify(got) === JSON.stringify(want);
    console.log(ok ? `  ok    ${name}` : `  FAIL  ${name}: expected ${JSON.stringify(want)}, got ${JSON.stringify(got)}`);
    if (!ok) failures.push(name);
  };
  const near = (name, got, want) => check(name, round1(got), round1(want));

  // the lines a sync probe writes, as `docker compose logs --no-log-prefix --timestamps` shows them
  const SYNC_LOG = [
    "2026-10-06T08:00:00.123456789Z mercury-probe 0.0.0 (sync): project delta, the GitHub API through HTTPS_PROXY, the mode every 60 s, the metadata every 300 s, snapshots up to 512 MB on the tmpfs /repo",
    "2026-10-06T08:00:01.000000000Z mercury-probe: access code, the repository's manifest at 1a2b3c4",
    "2026-10-06T08:00:05.000000000Z mercury-probe: snapshot 1a2b3c4 in use: 412 files (3.4 MB), 2 excluded, 0 entries skipped, in 2.1 s; history: 57 commits in 90 days",
    "2026-10-06T08:00:09.000000000Z mercury-probe: metadata ready: 31 pull requests (31 fetched one by one), 12 issues, 140 runs, 3 releases, in 4.0 s",
    "2026-10-06T08:05:00.000000000Z mercury-probe: metadata ready: 31 pull requests (2 fetched one by one), 12 issues, 141 runs, 3 releases, in 0.9 s",
    "2026-10-06T08:07:00.000000000Z mercury-probe: snapshot 5d6e7f8 in use: 415 files (3.5 MB), 2 excluded, 0 entries skipped, in 1.8 s; history: 59 commits in 90 days",
    "2026-10-06T08:10:00.000000000Z mercury-probe: metadata ready: 32 pull requests (1 fetched one by one), 12 issues, 142 runs, 3 releases, in 0.8 s",
    "2026-10-06T08:11:00.000000000Z mercury-probe: the round wrote nothing: the head: the rate limit is spent; the next round tries again",
  ].join("\n");

  console.log("log lines");
  const events = parseProbeLog(SYNC_LOG);
  check("every sample line is read", events.map((e) => e.kind), ["start", "mode", "snapshot", "metadata", "metadata", "snapshot", "metadata", "error"]);
  check("the start line's intervals", [events[0].mode, events[0].codeInterval, events[0].metaInterval, events[0].snapshotMaxMb], ["sync", 60, 300, 512]);
  check("nanosecond timestamps", events[0].at.toISOString(), "2026-10-06T08:00:00.123Z");
  check("the snapshot line", [events[2].head, events[2].files, events[2].mb, events[2].commits], ["1a2b3c4", 412, 3.4, 57]);
  check("the error line", [events[7].rateLimited, events[7].message.startsWith("the head:")], [true, true]);
  const prefixed = parseProbeLine("probe-delta-sync-1  | mercury-probe: history at 5d6e7f8: 60 commits in 90 days");
  check("a line with the service prefix and no timestamp", [prefixed.kind, prefixed.at, prefixed.commits], ["history", null, 60]);
  const limited = parseProbeLine(
    "mercury-probe: access metadata (limited by the token), the manifest's copy in projects.yaml (the repository's manifest isn't used: the token can't read it)",
  );
  check("a mode line limited by the token", [limited.kind, limited.access, limited.limitedBy, limited.head], ["mode", "metadata", "the token", null]);
  check(
    "the capped and cleared lines",
    [
      parseProbeLine("mercury-probe: snapshot 1a2b3c4 is larger than PROBE_SNAPSHOT_MAX_MB (512 MB): metadata mode until the head moves").maxMb,
      parseProbeLine("mercury-probe: the mode is below code: the snapshots and the history are deleted").kind,
    ],
    [512, "cleared"],
  );
  check(
    "the dev and serve start lines",
    [
      parseProbeLine("2026-10-06T20:46:51.417005854Z mercury-probe 0.0.0 (dev): MCP at /mcp and the health check, on http://0.0.0.0:7000").mode,
      parseProbeLine("mercury-probe 0.0.0 (serve): MCP at /mcp and the health check, on http://0.0.0.0:7000; the tools answer once the first index is built").mode,
    ],
    ["dev", "serve"],
  );
  check("a line it doesn't read", parseProbeLine("mercury-probe: index built, 19 facts in 4 modules"), null);

  console.log("requests per round");
  // pulls 2 pages, issues + pulls 43 → 2, runs 141 → 2, releases 1, 2 fetched
  check("a metadata round", roundRequests(events[4]), 9);
  check("a metadata round at the start", roundRequests(events[3]), 38);
  check("a big metadata round", roundRequests({ kind: "metadata", pulls: 250, fetched: 0, issues: 120, runs: 2500, releases: 10 }), 4 + 5 + 10 + 1);
  check("a new head after another", roundRequests(events[5], 57), 2 + 1 + 2);
  check("a new head at the start (the whole history)", roundRequests(events[2], null), 2 + 1 + 57);
  check("a new head with fewer commits in the window", roundRequests(events[5], 70), 2 + 1 + 1);

  console.log("the hourly estimate");
  const from = new Date("2026-10-06T08:02:00Z");
  const to = new Date("2026-10-06T08:12:00Z");
  const e = estimateGithubRequests(events, { from, to });
  // 60 ticks × 2 + 12 rounds × mean(9, 8) + one head move of 5 requests in 10 minutes
  near("ticks and the head", e.ticksPerHour + e.headPerHour, 120);
  near("metadata rounds", e.metaPerHour, 12 * 8.5);
  near("head moves", e.movesPerHour, 30);
  near("the total", e.perHour, 252);
  check("the start's rounds are outside this run", e.startup, 0);
  check("the run's failed rounds", [e.errors.length, e.rateLimited], [1, 1]);
  const fromStart = estimateGithubRequests(events, { from: new Date("2026-10-06T07:55:00Z"), to });
  // the probe started during the run: its first rounds are left out, the run counts from the start line
  check("the first rounds after a start", fromStart.startup, 60 + 38);
  near("the run counts from the start line", fromStart.hours, 0.2);
  near("the total with a start in the run", fromStart.perHour, 120 + 102 + 25);
  const tokenLimited = estimateGithubRequests(
    parseProbeLog(`${SYNC_LOG.split("\n")[0]}\nmercury-probe: access metadata (limited by the token), the manifest's copy in projects.yaml`),
    { from, to },
  );
  near("a token that can't read the code: a head poll every 10 minutes", tokenLimited.ticksPerHour + tokenLimited.headPerHour, 66);
  check("no metadata round logged: six lists assumed", tokenLimited.metaRequests, 6);
  const noStart = estimateGithubRequests(parseProbeLog(SYNC_LOG.split("\n").slice(4).join("\n")), { from, to });
  check(
    "no start line: the probe's default intervals, and no round taken for a start's",
    [noStart.intervalsAssumed, noStart.codeInterval, noStart.metaInterval, noStart.startup, noStart.metaRequests],
    [true, 60, 300, 0, 8.5],
  );

  console.log("mounts, df, stats, ps");
  // lines as a probe container shows them: a tmpfs at /tmp, the fixture on a bind mount, none at /repo
  const MOUNTINFO = [
    "1167 763 0:127 / / ro,relatime - overlay overlay rw,lowerdir=/var/lib/docker/overlay2/l/A:/var/lib/docker/overlay2/l/B",
    "1177 1167 0:153 / /tmp rw,nosuid,nodev,noexec,relatime - tmpfs tmpfs rw",
    "1178 1167 0:70 /src/tests/fixtures/repo-delta /fixture ro,noatime - 9p drvfs rw,aname=drvfs",
    "1190 1167 0:161 / /repo ro,relatime - tmpfs tmpfs rw,size=1048576k,uid=10001,gid=10001,mode=700",
    "1191 1167 0:162 / /with\\040space rw - tmpfs tmpfs rw",
  ].join("\n");
  check("a tmpfs mount", parseMountinfo(MOUNTINFO, "/repo"), { device: "0:161", fstype: "tmpfs", source: "tmpfs" });
  check("a bind mount", parseMountinfo(MOUNTINFO, "/fixture")?.fstype, "9p");
  check("an escaped mount point", parseMountinfo(MOUNTINFO, "/with space")?.device, "0:162");
  check("no mount", parseMountinfo(MOUNTINFO, "/meta"), null);
  const DF = "Filesystem      1B-blocks     Used  Available Use% Mounted on\ntmpfs          1073741824 52428800 1021313024   5% /repo\n";
  check("df -B1", parseDf(DF), { size: 1073741824, used: 52428800, available: 1021313024, mountedOn: "/repo" });
  check("df -k", parseDf("Filesystem 1K-blocks Used Available Use% Mounted on\ntmpfs 1048576 51200 997376 5% /repo", 1024).used, 52428800);
  check("df with nothing", parseDf("df: /repo: No such file or directory"), null);
  const STATS =
    '{"BlockIO":"0B / 0B","CPUPerc":"0.01%","Container":"0bcb81fea74d","ID":"0bcb81fea74d","MemPerc":"0.08%","MemUsage":"12.5MiB / 15.51GiB","Name":"mercury-bench-probes-probe-delta-1","NetIO":"5.73kB / 23.2kB","PIDs":"11"}';
  check("docker stats", parseStats(STATS)[0].memBytes, Math.round(12.5 * 1024 * 1024));
  check("sizes", [toBytes("0B"), toBytes("5.73kB"), toBytes("1.5GiB"), toBytes("2MB"), toBytes("lots")], [0, 5730, 1610612736, 2000000, null]);
  const PS_LINE = (service, command, state = "running") =>
    JSON.stringify({ Command: `"${command}"`, Name: `bench-${service}-1`, Service: service, State: state });
  const ps = [PS_LINE("probe-delta", "mercury-probe dev --repo /fixture --meta /fixture/meta --listen 0.0.0.0:7000"), PS_LINE("core", "mercury-core")].join("\n");
  check("ps, one object a line", parsePs(ps).map((c) => [c.service, probeMode(c.command)]), [ ["probe-delta", "dev"], ["core", null] ]);
  check("ps, one array", parsePs(`[${PS_LINE("probe-x-sync", "mercury-probe sync")}]`)[0].command, "mercury-probe sync");
  check(
    "the egress proxy's CONNECT lines",
    countProxyTunnels(
      [
        "2026-10-06T08:03:00.000000000Z mercury-probe: CONNECT github.com:443",
        "2026-10-06T08:03:00.000000000Z mercury-probe: CONNECT api.github.com:443",
        "2026-10-06T08:07:01.000000000Z mercury-probe: CONNECT codeload.github.com:443",
        "2026-10-06T07:00:00.000000000Z mercury-probe: CONNECT api.github.com:443",
      ].join("\n"),
      from,
      to,
    ),
    { "api.github.com": 1, "codeload.github.com": 1 },
  );

  console.log("secrets");
  // made up at run time, so no scanner takes the samples for real tokens
  const madeUp = (prefix, length) => prefix + "Ab3".repeat(length).slice(0, length);
  check(
    "masked",
    [
      maskSecrets("https://user:hunter2hunter2@example.com:8888"),
      maskSecrets(`token ${madeUp("ghp_", 36)} here`),
      maskSecrets(madeUp("github_pat_", 82)),
      maskSecrets(`Authorization: Bearer ${madeUp("mcy_", 43)}`),
      maskSecrets(`key ${madeUp("sk-or-v1-", 64)}`),
      maskSecrets(`x ${madeUp("", 44)} y`),
    ],
    [
      "https://[secret]@example.com:8888",
      "token [secret] here",
      "[secret]",
      "Authorization: Bearer [secret]",
      "key [secret]",
      "x [secret] y",
    ],
  );
  check(
    "left alone",
    maskSecrets("mercury-bench-probes-probe-delta-sync-1 at 1a2b3c4d5e6f7a8b9c0d1a2b3c4d5e6f7a8b9c0d: the token isn't allowed to read it"),
    "mercury-bench-probes-probe-delta-sync-1 at 1a2b3c4d5e6f7a8b9c0d1a2b3c4d5e6f7a8b9c0d: the token isn't allowed to read it",
  );

  console.log("the whole measurement, on canned outputs");
  const canned = {
    "ps -a --format json --no-trunc": [
      PS_LINE("probe-delta-sync", "mercury-probe sync"),
      PS_LINE("probe-delta", "mercury-probe serve"),
      PS_LINE("probe-fixture", "mercury-probe dev --repo /fixture --meta /fixture/meta"),
      PS_LINE("probe-gone", "mercury-probe serve", "exited"),
      PS_LINE("egress-proxy", "mercury-probe egress --listen 0.0.0.0:8888 --allow /etc/egress/allowlist"),
    ].join("\n"),
    "logs --no-color --no-log-prefix --timestamps probe-delta-sync": SYNC_LOG,
    "logs --no-color --no-log-prefix --timestamps egress-proxy":
      "2026-10-06T08:03:00.000000000Z mercury-probe: CONNECT api.github.com:443\n",
    "stats --no-stream --format json probe-delta-sync": STATS.replace("12.5MiB", "60MiB"),
    "stats --no-stream --format json probe-delta": STATS.replace("12.5MiB", "20MiB"),
    "stats --no-stream --format json probe-fixture": STATS,
    "exec -T probe-delta-sync cat /proc/self/mountinfo": MOUNTINFO,
    "exec -T probe-delta cat /proc/self/mountinfo": MOUNTINFO,
    "exec -T probe-fixture cat /proc/self/mountinfo": MOUNTINFO.replace(/^1190 .*$/m, ""),
    "exec -T probe-delta-sync df -B1 /repo": DF,
    "exec -T probe-delta df -B1 /repo": DF,
  };
  const fake = (args) => {
    const key = args.join(" ");
    return key in canned ? { code: 0, stdout: canned[key], stderr: "" } : { code: 1, stdout: "", stderr: `no such command: ${key}` };
  };
  const result = await probeMetrics({ compose: fake, since: from.toISOString(), now: to });
  check("the GitHub figure", result.githubRequestsPerHour, 252);
  check("the note says it is an estimate", /^Estimated from the sync probes' round lines, not counted/.test(result.githubNote), true);
  check(
    "snapshot memory",
    result.snapshotRam.map((r) => [r.service, r.bytes]),
    [
      ["probe-delta", 52428800],
      ["probe-delta-sync", 52428800],
      ["probe-fixture", Math.round(12.5 * 1024 * 1024)],
      ["probe-gone", null],
    ],
  );
  check("how the dev probe was read", /no mount here \(a dev probe .*docker compose stats/.test(result.snapshotRam[2].how), true);
  check("the shared tmpfs", result.notes.some((n) => n.startsWith("probe-delta and probe-delta-sync share one tmpfs")), true);
  check("the egress proxy's tunnels", result.notes.some((n) => n.startsWith("egress-proxy: 1 tunnels to api.github.com")), true);
  const devOnly = await probeMetrics({ compose: fake, since: from.toISOString(), now: to, services: ["probe-fixture"] });
  check("no sync probe: no figure", [devOnly.githubRequestsPerHour, /^No sync probe on this stack/.test(devOnly.githubNote)], [null, true]);

  console.log(failures.length ? `\n${failures.length} failed` : "\nall passed");
  return failures.length === 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.includes("--self-test")) {
    let passed = false;
    try {
      passed = await selfTest();
    } catch (err) {
      // a parser that reads nothing leaves the later checks without their input
      console.log(`  FAIL  the self-test stopped: ${err.message}`);
    }
    process.exitCode = passed ? 0 : 1;
  } else {
    console.log("node tests/bench/probes.mjs --self-test   checks the parsers; the runner imports probeMetrics");
    process.exitCode = 2;
  }
}

// The stack the runs on Compose share (`just it`, `just golden`, `just redteam`): the laptop files
// with the fixture and the it overlay, under the project name `mercury-it`; the configuration and
// the secrets in target/it; one person per role, granted delta. Questions are asked with
// `mercury ask --json`, as a script would ask them, and what a question left in the audit is read
// back through its event stream, which the core serves from the audit rows.
//
// A second run beside it gets a stack of its own from MERCURY_IT_PROJECT, MERCURY_IT_PORT and
// MERCURY_IT_PROBES_SUBNET; MERCURY_CLI names a CLI built elsewhere (CI exports one from the images).

import { spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { request } from "node:http";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

export const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const composeDir = join(root, "deploy/compose");
const FILES = [
  "docker-compose.yml",
  "docker-compose.fixture.yml",
  "docker-compose.local.yml",
  "docker-compose.it.yml",
];
const PROJECT = process.env.MERCURY_IT_PROJECT ?? "mercury-it";
const work = join(root, "target", PROJECT === "mercury-it" ? "it" : PROJECT);
/**
 * `--live`: the models of config-examples/models.yaml (Jev and the chat models on OpenRouter) and the
 * key in deploy/compose/secrets/openrouter_api_key, in place of the replay server. It costs a little
 * money and sends the labelled questions, which are synthetic, to OpenRouter. The key is a secret
 * of the run like a token: no output may hold it, and its copy in target/ goes with the stack.
 */
export const LIVE = process.argv.includes("--live");
const BASE_SERVICES = LIVE ? ["core", "probe-delta", "caddy"] : ["core", "probe-delta", "replay", "caddy"];
/**
 * The red team's repository: a probe on it, the proxy the core calls it through, and its code reader
 * with the reader's egress proxy, for `just redteam`.
 */
const POISON_SERVICES = ["probe-poison", "rogue", "code-reader", "reader-egress"];
/**
 * The code reader's upstream, model and allowlist: the replay server's `replay/reader`, reached directly;
 * with `--live`, the demo preset's model on OpenRouter through reader-egress and its own allowlist.
 */
const READER = LIVE
  ? {
      MERCURY_IT_READER_URL: "https://openrouter.ai/api/v1/chat/completions",
      MERCURY_IT_READER_MODEL: "anthropic/claude-4.6-sonnet-20260217",
      MERCURY_IT_READER_ALLOWLIST: "./reader-egress/allowlist",
    }
  : {
      MERCURY_IT_READER_URL: "http://replay:9000/v1/chat/completions",
      MERCURY_IT_READER_MODEL: "replay/reader",
      MERCURY_IT_READER_ALLOWLIST: "../../tests/it/reader-egress/allowlist",
    };
let poison = false;
/** The services of this run. */
const services = () => (poison ? [...BASE_SERVICES, ...POISON_SERVICES] : BASE_SERVICES);
const PORT = Number(process.env.MERCURY_IT_PORT ?? 18080);
export const URL = `http://127.0.0.1:${PORT}`;
/**
 * The page a browser opens: the same Caddy at `localhost`, the origin the core accepts a session
 * from. Every browser takes the `Secure` session cookie over plain http on `localhost`; WebKit refuses
 * it on `127.0.0.1`.
 */
export const PAGE_URL = `http://localhost:${PORT}`;
const CLI = process.env.MERCURY_CLI
  ? resolve(process.env.MERCURY_CLI)
  : join(root, "target/debug", process.platform === "win32" ? "mercury.exe" : "mercury");
/** How long each kind of `docker compose` call may take, in seconds: `up` pulls images, `build` the proxy's base. */
const LIMITS = { up: 300, build: 300, down: 120, logs: 60, exec: 60 };

/** The people the runs ask as: one per role, each granted delta and nothing else, a second
 * project manager whose day the budget scenario spends, and an admin who reads the audit. */
export const PEOPLE = {
  pm: { email: "pm@example.com", name: "Project Manager" },
  // the red team's attacker: a project manager with access to the poisoned project only
  red: { email: "red@example.com", name: "Red Teamer", role: "pm", projects: ["poison"] },
  tech_lead: { email: "lead@example.com", name: "Tech Lead" },
  board: { email: "board@example.com", name: "Board Member" },
  spender: { email: "spender@example.com", name: "Second Manager", role: "pm" },
  admin: { email: "admin@example.com", name: "Administrator" },
};

/** Every secret of a run, so no output may hold one. */
export const secrets = [];

/** The exact command, for a person to inspect a stack left up with `--keep`. */
export const composeCommand = `docker compose -p ${PROJECT} ${FILES.map((f) => `-f ${f}`).join(" ")}`;

function pause(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/**
 * A `docker compose` call on this run's stack as plain data: the command, its arguments, the folder it runs in and
 * the environment it needs. For a test that has to act on the stack from another process (a browser test that
 * revokes the token its page signed in with); it holds no secret.
 */
export function composeCall(args) {
  return {
    command: "docker",
    args: ["compose", "-p", PROJECT, ...FILES.flatMap((f) => ["-f", f]), ...args],
    cwd: composeDir,
    env: {
      MERCURY_IT_PORT: String(PORT),
      // the overlay's files of this run, relative to deploy/compose
      MERCURY_IT_WORK: relative(composeDir, work).split(sep).join("/"),
      ...READER,
    },
  };
}

function compose(args) {
  const limit = LIMITS[args[0]] ?? 120;
  const call = composeCall(args);
  for (let attempt = 1; ; attempt += 1) {
    const run = spawnSync(call.command, call.args, {
      cwd: call.cwd,
      encoding: "utf8",
      env: { ...process.env, ...call.env },
      timeout: limit * 1000,
      maxBuffer: 64 * 1024 * 1024,
    });
    if (run.error?.code === "ETIMEDOUT") throw new Error(`docker compose ${args[0]} didn't finish within ${limit} s`);
    if (run.error) throw new Error(`docker compose can't be run: ${run.error.message}`);
    // Docker Desktop on Windows sometimes holds its context file for a moment
    if (run.status !== 0 && /being used by another process/.test(run.stderr) && attempt < 5) {
      pause(1000);
      continue;
    }
    return run;
  }
}

/** The configuration examples with the runs' models and higher limits, and fresh secrets. */
export function prepare() {
  rmSync(work, { recursive: true, force: true });
  mkdirSync(join(work, "secrets/probes"), { recursive: true });
  cpSync(join(root, "config-examples"), join(work, "config"), { recursive: true });
  if (!LIVE) cpSync(join(root, "tests/it/config/models.yaml"), join(work, "config/models.yaml"));
  // the golden set asks 200 questions, most of them as one person, within minutes
  const policies = join(work, "config/policies.yaml");
  const raised = readFileSync(policies, "utf8")
    .replace(/questions_per_hour: \d+/, "questions_per_hour: 3000")
    .replace(/burst: \d+/, "burst: 300");
  if (!raised.includes("questions_per_hour: 3000") || !raised.includes("burst: 300")) {
    throw new Error("config-examples/policies.yaml has no limits to raise");
  }
  writeFileSync(policies, raised);
  // the second project, the red team's: its probe is reached through the proxy (tests/redteam/rogue-probe)
  const projects = join(work, "config/projects.yaml");
  const examples = readFileSync(projects, "utf8").replace(/\r\n/g, "\n");
  if (!/^projects:\n/m.test(examples)) throw new Error("config-examples/projects.yaml has no projects to add one to");
  writeFileSync(
    projects,
    `${examples.replace(/\n*$/, "\n")}  poison:                              # tests/redteam/repo-poisoned, for \`just redteam\`
    display_name: "Ember — reports"
    aliases: [poison, ember]
    access_max: code_model                # its manifest asks for the code reader
    probe:
      url: http://rogue:7000/mcp            # the proxy that can break the contract, in front of the real probe
      token_file: /run/mercury/probe-tokens/poison.token
`,
  );
  // the proxy's mode, the way the runner tells it to misbehave
  mkdirSync(join(work, "rogue"), { recursive: true });
  writeFileSync(join(work, "rogue/mode"), "off\n");
  const replayKey = randomBytes(24).toString("base64url");
  const probeToken = randomBytes(32).toString("base64url");
  const poisonToken = randomBytes(32).toString("base64url");
  secrets.push(replayKey, probeToken, poisonToken);
  let openrouterKey = "";
  if (LIVE) {
    openrouterKey = readFileSync(join(root, "deploy/compose/secrets/openrouter_api_key"), "utf8").trim();
    if (!openrouterKey) throw new Error("--live needs the OpenRouter key in deploy/compose/secrets/openrouter_api_key");
    secrets.push(openrouterKey);
  }
  writeFileSync(join(work, "secrets/openrouter_api_key"), openrouterKey);
  writeFileSync(join(work, "secrets/replay_key"), replayKey);
  // the code reader's key: the replay server asks for one; with --live, the same OpenRouter key
  const readerKey = LIVE ? openrouterKey : randomBytes(24).toString("base64url");
  if (!LIVE) secrets.push(readerKey);
  writeFileSync(join(work, "secrets/reader_api_key"), readerKey);
  writeFileSync(join(work, "secrets/probes/delta.token"), probeToken);
  writeFileSync(join(work, "secrets/probes/poison.token"), poisonToken);
}

/** Tells the proxy in front of the poisoned project's probe how to behave: `off`, `extra_field`, `extra_tool` or `huge`. */
export function setRogue(mode) {
  if (!["off", "extra_field", "extra_tool", "huge"].includes(mode)) throw new Error(`no such mode: ${mode}`);
  writeFileSync(join(work, "rogue/mode"), `${mode}\n`);
}

/** Restarts the poisoned repository's probe, which registers with the core again as it starts. */
export function restartPoisonProbe() {
  const restarted = compose(["restart", "probe-poison"]);
  if (restarted.status !== 0) throw new Error(`docker compose restart failed: ${mask(restarted.stderr)}`);
}

/**
 * After a probe broke the contract: the proxy honest again, the probe released and registered anew,
 * and healthy.
 */
export async function recoverPoisonProbe() {
  setRogue("off");
  admin(["probe", "release", "poison"]);
  restartPoisonProbe();
  if (!(await waitForProbe("poison", "healthy", 60))) {
    throw new Error("the poisoned repository's probe didn't become healthy again");
  }
}

/** Waits until `probe list` shows `project` with `status`, `seconds` at most; false when it never did. */
export async function waitForProbe(project, status, seconds) {
  const end = Date.now() + seconds * 1000;
  const shown = new RegExp(`^${project}\\s+${status}\\b`, "m");
  while (Date.now() < end) {
    if (shown.test(admin(["probe", "list"]))) return true;
    await new Promise((done) => setTimeout(done, 1000));
  }
  return false;
}

function get(path) {
  return new Promise((done) => {
    const req = request(`${URL}${path}`, { method: "GET", agent: false }, (res) => {
      res.resume();
      res.on("end", () => done(res.statusCode));
    });
    req.on("error", () => done(0));
    req.setTimeout(5000, () => req.destroy());
    req.end();
  });
}

/** Waits for `test` to hold; an attempt that throws (a restarting container, say) is tried again. */
async function until(what, test, seconds) {
  const end = Date.now() + seconds * 1000;
  let last = null;
  while (Date.now() < end) {
    try {
      if (await test()) return;
      last = null;
    } catch (err) {
      last = err;
    }
    await new Promise((done) => setTimeout(done, 500));
  }
  throw new Error(`${what} didn't happen within ${seconds} s${last ? `; the last attempt: ${last.message}` : ""}`);
}

/** The run's configuration folder, which the core reads at start (`prepare` writes it). */
export const configDir = join(work, "config");

/**
 * Restarts the core, which reads its configuration again, and waits until it answers through Caddy
 * and the fixture's probe is registered with it.
 */
export async function restartCore() {
  const restarted = compose(["restart", "core"]);
  if (restarted.status !== 0) throw new Error(`docker compose restart core failed:\n${mask(restarted.stderr)}`);
  await until("/v1/health through Caddy after the restart", async () => (await get("/v1/health")) === 200, 120);
  await until("the fixture's probe registered after the restart", () => /^delta\s+healthy\b/m.test(admin(["probe", "list"])), 120);
}

/** The stack started, the core answering through Caddy and the fixture's probe registered. */
export async function up() {
  // nothing to build: Mercury's services run the images `just _images` made (the reader's egress proxy
  // too, the probe image's `egress`), and the others are pulled
  const started = compose(["up", "-d", "--no-build", "--quiet-pull", ...services()]);
  if (started.status !== 0) throw new Error(`docker compose up failed:\n${started.stderr}`);
  await until("/v1/health through Caddy", async () => (await get("/v1/health")) === 200, 120);
  await until("the fixture's probe registering", () => /^delta\s+healthy\b/m.test(admin(["probe", "list"])), 120);
  if (poison) {
    // with its code reader: a probe that came up before the reader answered still says code_model, since
    // the mode comes from the configuration, not from a call
    await until(
      "the poisoned repository's probe registering with code_model",
      () => /^poison\s+healthy\s+code_model\b/m.test(admin(["probe", "list"])),
      120,
    );
  }
}

export function down() {
  const stopped = compose(["down", "-v", "--remove-orphans"]);
  if (stopped.status !== 0) console.error(`docker compose down failed:\n${stopped.stderr}`);
}

/** `text` with every secret of the run replaced, for anything a run prints. */
export function mask(text) {
  return secrets.reduce((out, secret) => out.split(secret).join("[secret]"), text);
}

/** The last lines each service logged, for a run that failed, with the run's secrets masked. */
export function logs() {
  return mask(compose(["logs", "--no-color", "--tail", "40", ...services()]).stdout);
}

/**
 * Everything the services logged, for the checks on what a log may hold. A failed or empty read
 * is an error: the checks would pass on nothing.
 */
export function allLogs() {
  const run = compose(["logs", "--no-color", ...services()]);
  if (run.status !== 0) throw new Error(`docker compose logs failed: ${mask(run.stderr.trim())}`);
  if (!run.stdout.trim()) throw new Error("docker compose logs returned nothing, so no check on the logs can run");
  return run.stdout;
}

/** An admin command on the core's database, inside the core's container. */
export function admin(args) {
  const run = compose(["exec", "-T", "core", "mercury-core", "admin", ...args]);
  if (run.status !== 0) throw new Error(`admin ${args.slice(0, 2).join(" ")} failed: ${mask(run.stderr.trim())}`);
  return run.stdout;
}

/**
 * The channel each question was asked on (`cli`, `web` or `mcp`), by id; `null` for an id the
 * database doesn't hold. No route shows it, so it is read from a copy of the core's database
 * (`docker compose cp`, then Node's own SQLite, Node.js 22.13 or later). The copy holds hashes and
 * redacted texts, no secret, and goes as soon as it is read; a copy taken while the core wrote is
 * taken again once.
 */
export async function channelsOf(ids) {
  let sqlite;
  try {
    sqlite = await import("node:sqlite");
  } catch {
    throw new Error("reading a question's channel needs node:sqlite (Node.js 22.13 or later)");
  }
  const copy = join(work, "core-db");
  for (let attempt = 1; ; attempt += 1) {
    rmSync(copy, { recursive: true, force: true });
    mkdirSync(copy, { recursive: true });
    try {
      const copied = compose(["cp", "core:/data/.", relative(composeDir, copy).split(sep).join("/")]);
      if (copied.status !== 0) throw new Error(`docker compose cp failed: ${mask(copied.stderr.trim())}`);
      // the index of the write-ahead log is rebuilt from the log itself
      rmSync(join(copy, "mercury.db-shm"), { force: true });
      const db = new sqlite.DatabaseSync(join(copy, "mercury.db"));
      try {
        const read = db.prepare("SELECT channel FROM questions WHERE id = ?");
        return Object.fromEntries(ids.map((id) => [id, read.get(id)?.channel ?? null]));
      } finally {
        db.close();
      }
    } catch (err) {
      if (attempt >= 2) throw err;
    } finally {
      rmSync(copy, { recursive: true, force: true });
    }
  }
}

/** Each of [`PEOPLE`], granted delta, with a token each: name → token. */
export function people() {
  const tokens = {};
  for (const [key, { email, name, role = key, projects = ["delta"] }] of Object.entries(PEOPLE)) {
    admin(["user", "add", "--email", email, "--name", name, "--role", role]);
    admin(["user", "grant", "--email", email, "--projects", projects.join(",")]);
    const token = admin(["token", "create", "--email", email, "--label", "it"]).match(/mcy_[A-Za-z0-9_-]+/)?.[0];
    if (!token) throw new Error(`token create printed no token for ${email}`);
    secrets.push(token);
    tokens[key] = token;
  }
  return tokens;
}

/** `mercury <args>` with this token: the exit code and the outputs. */
export function mercury(token, args) {
  const run = spawnSync(CLI, args, {
    env: { ...process.env, MERCURY_URL: URL, MERCURY_TOKEN: token },
    encoding: "utf8",
    timeout: 120_000,
  });
  if (run.error) throw new Error(`the mercury CLI can't be run: ${run.error.message}`);
  return { code: run.status, stdout: run.stdout, stderr: run.stderr };
}

/** `mercury ask <args> --json` with this token: the exit code, the outputs and the parsed object. */
export function ask(token, args) {
  const run = spawnSync(CLI, ["ask", ...args, "--json"], {
    env: { ...process.env, MERCURY_URL: URL, MERCURY_TOKEN: token },
    encoding: "utf8",
    timeout: 120_000,
  });
  if (run.error) throw new Error(`the mercury CLI can't be run: ${run.error.message}`);
  let summary = null;
  try {
    summary = JSON.parse(run.stdout);
  } catch {
    // an error before the question started may leave stdout empty
  }
  return { code: run.status, stdout: run.stdout, stderr: run.stderr, summary };
}

/** The question's audit rows, in order, as its event stream replays them: `{ id, event, data }`. */
export function rows(token, question) {
  return new Promise((done, fail) => {
    const req = request(
      `${URL}/v1/questions/${question}/events`,
      { method: "GET", agent: false, headers: { authorization: `Bearer ${token}`, accept: "text/event-stream" } },
      (res) => {
        let body = "";
        res.setEncoding("utf8");
        res.on("data", (chunk) => (body += chunk));
        res.on("end", () => {
          if (res.statusCode !== 200) return fail(new Error(`the event stream answered ${res.statusCode}`));
          const found = [];
          for (const block of body.replace(/\r\n/g, "\n").split("\n\n")) {
            const field = (name) => block.match(new RegExp(`^${name}: ?(.*)$`, "m"))?.[1];
            if (field("event")) {
              found.push({ id: Number(field("id")), event: field("event"), data: JSON.parse(field("data") ?? "null") });
            }
          }
          done(found);
        });
      },
    );
    req.on("error", fail);
    req.setTimeout(30_000, () => req.destroy(new Error("the event stream didn't end")));
    req.end();
  });
}

/** What the web client's page sends with everything it asks: its own origin and its header. */
export const PAGE = { origin: PAGE_URL, "x-mercury-client": "web" };

/**
 * One HTTP request as a browser or curl would send it, with the headers given and nothing else:
 * `{ status, headers, text, json }`. A body that isn't a string is sent as JSON.
 */
export function http(method, path, { headers = {}, body } = {}) {
  return new Promise((done, fail) => {
    const payload = body === undefined ? undefined : typeof body === "string" ? body : JSON.stringify(body);
    const sent = { ...headers };
    if (payload !== undefined) {
      sent["content-length"] = Buffer.byteLength(payload);
      sent["content-type"] ??= typeof body === "string" ? "text/plain" : "application/json";
    }
    // a connection of its own each time: a pooled one may have been closed by Caddy while the run
    // waited minutes on something else (the browsers), and reusing it ends in "socket hang up"
    const req = request(`${URL}${path}`, { method, headers: sent, agent: false }, (res) => {
      let text = "";
      res.setEncoding("utf8");
      res.on("data", (chunk) => (text += chunk));
      res.on("end", () => {
        let json = null;
        try {
          json = JSON.parse(text);
        } catch {
          // an event stream, or no body at all
        }
        done({ status: res.statusCode, headers: res.headers, text, json });
      });
    });
    req.on("error", fail);
    req.setTimeout(30_000, () => req.destroy(new Error(`${method} ${path} didn't answer`)));
    if (payload !== undefined) req.write(payload);
    req.end();
  });
}

/**
 * Signs `token` in as the web client's page does. The session id is a secret of the run, so no log
 * or output may hold it. Gives the response and the cookie as a request sends it back, if one was set.
 */
export async function signIn(token) {
  const response = await http("POST", "/v1/session", { headers: PAGE, body: { token } });
  const cookie = response.headers["set-cookie"]?.[0]?.split(";")[0] ?? null;
  if (cookie) secrets.push(cookie.slice(cookie.indexOf("=") + 1));
  return { response, cookie };
}

/** The secrets an output holds; none, or the run fails. */
export function leaked(text) {
  return secrets.filter((secret) => text.includes(secret)).length;
}

/**
 * Runs `body` on a fresh stack and takes the stack down after it, unless `--keep`. `{ poison: true }`
 * adds the red team's repository: its probe, the proxy in front of it and the project `poison`.
 */
export async function withStack(body, options = {}) {
  const keep = process.argv.includes("--keep");
  poison = options.poison === true;
  let failed = true;
  try {
    // a stack an earlier --keep left up holds target/it open
    down();
    prepare();
    await up();
    failed = !(await body(people()));
  } catch (err) {
    console.error(`\n${mask(err.message)}`);
  } finally {
    if (failed) {
      try {
        console.error(`\nthe last lines the services logged:\n${logs()}`);
      } catch (err) {
        console.error(`\nthe services' logs can't be read: ${mask(err.message)}`);
      }
    }
    if (keep) {
      console.log(`\nthe stack is up on ${URL}; ${composeCommand} exec core mercury-core admin token create --email pm@example.com --label me gives a token`);
    } else {
      down();
      // the key was copied beside the run's other secrets; it doesn't outlive the stack
      if (LIVE) {
        rmSync(join(work, "secrets/openrouter_api_key"), { force: true });
        rmSync(join(work, "secrets/reader_api_key"), { force: true });
      }
    }
  }
  process.exitCode = failed ? 1 : 0;
}

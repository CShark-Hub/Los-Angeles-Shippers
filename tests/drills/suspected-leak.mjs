// The drill "a leak is suspected": the steps an operator takes, walked on a fresh stack. A question
// was answered; then the token that asked it is revoked, the project's probe quarantined, the
// question's rows and the token's rows exported, the export verified in another folder without the
// database, and the probe released. Each step is checked for what it must do: the token refused at
// once, no question to the quarantined project reaching its probe, an export whose rows are the
// question's and the token's and whose hashes recompute, and the project answering again after the
// release.
//
// The models are the replay server's recordings, so the drill costs nothing. It runs under its own
// Compose project, port and subnets, so it never touches the local stack; the stack goes down with its
// volumes when the drill ends, unless `--keep`.
//
//   node tests/drills/suspected-leak.mjs [--keep]

import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// stack.mjs reads these when it is imported, so they are set first
const DEFAULTS = {
  MERCURY_IT_PROJECT: "mercury-drill-leak",
  MERCURY_IT_PORT: "18105",
  MERCURY_IT_PROBES_SUBNET: "172.30.59.0/24",
  MERCURY_IT_EDGE_SUBNET: "172.30.58.0/24",
};
for (const [name, value] of Object.entries(DEFAULTS)) process.env[name] ??= value;

const { admin, ask, composeCommand, http, leaked, mask, rows, waitForProbe, withStack } = await import("../it/stack.mjs");

/** The question whose answer is suspected: a recording answers it from the fixture. */
const QUESTION = "what was merged this week?";
/** How long a released probe may take to register and be checked again. */
const RELEASED_WITHIN = 120;

const failures = [];
const outputs = [];

function check(name, holds, seen) {
  const line = holds ? `  ok    ${name}` : `  FAIL  ${name}${seen === undefined ? "" : `: saw ${seen}`}`;
  console.log(mask(line));
  if (!holds) failures.push(name);
  return holds;
}

const say = (text) => console.log(mask(text));
const phase = (title) => say(`\n== ${title}`);
const seconds = (ms) => `${(ms / 1000).toFixed(1)} s`;
const sha256 = (bytes) => createHash("sha256").update(bytes).digest();

async function asked(token, label, project = "delta") {
  const run = ask(token, ["--project", project, QUESTION]);
  outputs.push(run.stdout, run.stderr);
  const found = run.summary?.id ? await rows(token, run.summary.id) : [];
  outputs.push(JSON.stringify(found));
  say(`      ${label}: mercury ask --project ${project} "${QUESTION}" → exit ${run.code}, ${run.summary?.status}${run.summary?.error ? ` (${run.summary.error.code})` : ""}`);
  return { ...run, events: found.map((r) => r.event) };
}

function auditHolds(after) {
  const verified = admin(["audit", "verify"]).trim();
  say(`      admin audit verify → ${verified}`);
  check(`audit verify after ${after}`, /^ok: \d+ row\(s\), the chain holds$/.test(verified), verified);
}

/**
 * An export, checked in its own folder with nothing but the file: every row's hash recomputed from
 * its fields (`prev_hash`, `seq`, `type`, `payload_sha256`, `at`), consecutive rows linked, and each
 * payload's digest from its bytes in the file. Gives what it found.
 */
function verifyAlone(file) {
  const lines = readFileSync(file, "utf8").trim().split("\n").filter(Boolean);
  const found = { rows: lines.length, hashes: 0, links: 0, gaps: 0, payloads: 0 };
  let previous = null;
  for (const line of lines) {
    const row = JSON.parse(line);
    const seq = Buffer.alloc(8);
    seq.writeBigUInt64BE(BigInt(row.seq));
    // `at` as the file writes it: the stored time, milliseconds included
    const hash = sha256(
      Buffer.concat([
        Buffer.from(row.prev_hash, "hex"),
        seq,
        Buffer.from([Buffer.byteLength(row.type)]),
        Buffer.from(row.type),
        Buffer.from(row.payload_sha256, "hex"),
        Buffer.from(row.at),
      ]),
    ).toString("hex");
    if (hash === row.hash) found.hashes += 1;
    if (previous && previous.seq + 1 === row.seq) {
      if (previous.hash === row.prev_hash) found.links += 1;
    } else if (previous) found.gaps += 1;
    // the payload's bytes as the file holds them; one retention deleted is null, its digest kept
    const raw = line.match(/"payload":(.*),"payload_sha256":"[0-9a-f]{64}"/)?.[1];
    if (raw === "null" || (raw && sha256(raw).toString("hex") === row.payload_sha256)) found.payloads += 1;
    previous = row;
  }
  return found;
}

await withStack(async (tokens) => {
  say(`the drill's stack: ${composeCommand}`);

  phase("0. the suspected answer");
  const suspect = await asked(tokens.pm, "the manager's question");
  check("it was answered", suspect.code === 0 && suspect.summary?.status === "delivered", `exit ${suspect.code}`);
  const tokenId = tokens.pm.slice(4, 16);
  const questionId = suspect.summary?.id;
  say(`      the question ${questionId}, asked with the token ${tokenId}`);

  phase("1. revoke the token involved");
  admin(["token", "revoke", tokenId]);
  const me = await http("GET", "/v1/me", { headers: { authorization: `Bearer ${tokens.pm}` } });
  check("the revoked token: 401 at once", me.status === 401, me.status);
  const refused = await asked(tokens.pm, "the revoked token asking again");
  check("the revoked token asks nothing", refused.code === 1 && !refused.summary?.id, `exit ${refused.code}, ${refused.summary?.id}`);
  auditHolds("the revocation");

  phase("2. quarantine the project's probe");
  say('      admin probe quarantine delta --reason "suspected leak"');
  admin(["probe", "quarantine", "delta", "--reason", "suspected leak"]);
  const listed = admin(["probe", "list"]);
  for (const line of listed.trimEnd().split("\n")) say(`        ${line}`);
  check("probe list shows delta quarantined", /^delta\s+quarantined\b/m.test(listed), listed);
  const during = await asked(tokens.tech_lead, "another person asking about delta");
  check("nothing is delivered from the quarantined project", during.summary?.status !== "delivered" && (during.summary?.answer ?? null) === null, during.summary?.status);
  check("its probe was never called", !during.events.includes("tool_result"), during.events.join(","));
  auditHolds("the quarantine");

  phase("3. export by question and by token, and verify in another folder");
  const folder = mkdtempSync(join(tmpdir(), "mercury-leak-"));
  try {
    const byQuestion = join(folder, "question.jsonl");
    const byToken = join(folder, "token.jsonl");
    writeFileSync(byQuestion, admin(["audit", "export", "--question", questionId]));
    writeFileSync(byToken, admin(["audit", "export", "--token", tokenId]));
    const questionRows = readFileSync(byQuestion, "utf8").trim().split("\n").map((l) => JSON.parse(l));
    const tokenRows = readFileSync(byToken, "utf8").trim().split("\n").map((l) => JSON.parse(l));
    say(`      admin audit export --question ${questionId} > question.jsonl → ${questionRows.length} rows: ${questionRows.map((r) => r.type).join(", ")}`);
    say(`      admin audit export --token ${tokenId} > token.jsonl → ${tokenRows.length} rows`);
    check("the question's export: its rows, in order, from accepted to done", questionRows[0]?.type === "accepted" && questionRows.at(-1)?.type === "done" && questionRows.every((r) => r.question_id === questionId), questionRows.map((r) => r.type).join(","));
    check("each stage's checks with their verdicts", (questionRows.find((r) => r.type === "classified")?.payload?.checks ?? []).every((c) => c.verdict && c.source), JSON.stringify(questionRows.find((r) => r.type === "classified")?.payload ?? null));
    check("the token's export: its question and its revocation", tokenRows.some((r) => r.question_id === questionId) && tokenRows.some((r) => r.type === "token_revoked"), tokenRows.map((r) => r.type).join(","));
    for (const [name, file] of [["question.jsonl", byQuestion], ["token.jsonl", byToken]]) {
      const found = verifyAlone(file);
      say(`      ${name}, alone: ${JSON.stringify(found)}`);
      check(`${name}: every row's hash recomputes from its fields`, found.hashes === found.rows, JSON.stringify(found));
      check(`${name}: every payload re-digests from its bytes in the file`, found.payloads === found.rows, JSON.stringify(found));
      check(`${name}: consecutive rows link`, found.gaps === 0 || found.links > 0 || found.rows < 2, JSON.stringify(found));
    }
  } finally {
    rmSync(folder, { recursive: true, force: true });
  }
  auditHolds("the exports");

  phase("4. release the probe");
  say("      admin probe release delta");
  admin(["probe", "release", "delta"]);
  const releasedAt = Date.now();
  const healthy = await waitForProbe("delta", "healthy", RELEASED_WITHIN);
  check(`delta healthy within ${RELEASED_WITHIN} s of the release`, healthy, admin(["probe", "list"]));
  say(`      healthy ${seconds(Date.now() - releasedAt)} after the release`);
  const after = await asked(tokens.tech_lead, "after the release");
  check("delta answers again", after.code === 0 && after.summary?.answer?.text === suspect.summary?.answer?.text, after.summary?.answer?.text);
  auditHolds("the release");

  check("no secret in the CLI's outputs or the streams", leaked(outputs.join("\n")) === 0, "a secret");
  say(failures.length === 0 ? "\nthe drill passed" : `\nthe drill failed: ${failures.length} check(s)\n  ${failures.join("\n  ")}`);
  return failures.length === 0;
});

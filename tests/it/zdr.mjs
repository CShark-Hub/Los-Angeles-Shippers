// The zero-retention check: with `zdr: true` in the request's provider settings,
// OpenRouter sends a request only to endpoints that keep no data, so
//   1. a model with no zero-retention endpoint is refused (a model chosen from OpenRouter's own lists:
//      none of its endpoints is in /endpoints/zdr), and, when the model is a free one, the same request
//      without the setting is answered, so the refusal is the policy's and not a malformed request;
//   2. every model the shipped configuration names (config-examples/models.yaml and the presets) is
//      answered under the same setting: the chat models through /chat/completions, Jev through
//      /decisions.
//
//   node tests/it/zdr.mjs          the live run: about a tenth of a cent, the key from
//                                  deploy/compose/secrets/openrouter_api_key (OPENROUTER_API_KEY_FILE)
//   node tests/it/zdr.mjs --dry    what it would ask, without asking
//   node tests/it/zdr.mjs --refusal-only   only the refusal of a model without a zero-retention endpoint, which costs nothing
//
// The key goes into the Authorization header and nowhere else: no output holds it, a request, a header
// or a reply body. Exit 0 when every check held, 1 when one did not, 2 on a command line it doesn't know.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "../..");
const API = "https://openrouter.ai/api";
const PROVIDER = { data_collection: "deny", zdr: true };
const MODEL_FILES = ["config-examples/models.yaml", ...["demo", "private", "strict"].map((p) => `config-examples/presets/${p}/models.yaml`)];

const dry = process.argv.includes("--dry");
const refusalOnly = process.argv.includes("--refusal-only");
if (process.argv.slice(2).some((arg) => arg !== "--dry" && arg !== "--refusal-only")) {
  console.error("usage: node tests/it/zdr.mjs [--dry | --refusal-only]");
  process.exit(2);
}

/** The OpenRouter models the configuration names: slug → { kind: "jev" | "chat", roles }. */
function configuredModels() {
  const found = new Map();
  for (const file of MODEL_FILES) {
    const text = readFileSync(join(root, file), "utf8");
    for (const line of text.split(/\r?\n/)) {
      const role = /^\s{2}(\w+):\s*\{(.*)\}/.exec(line);
      if (!role || !/backend:\s*openrouter\b/.test(role[2])) continue;
      const slug = /\bmodel:\s*([^,\s}]+)/.exec(role[2])?.[1];
      if (!slug) continue;
      const kind = /\bkind:\s*jev\b/.test(role[2]) ? "jev" : "chat";
      const entry = found.get(slug) ?? { kind, roles: new Set() };
      entry.roles.add(role[1]);
      found.set(slug, entry);
    }
  }
  return found;
}

const failures = [];
const report = (held, line) => {
  console.log(`${held ? "ok  " : "FAIL"} ${line}`);
  if (!held) failures.push(line);
};

let key = "";
function readKey() {
  const file = process.env.OPENROUTER_API_KEY_FILE ?? join(root, "deploy/compose/secrets/openrouter_api_key");
  key = readFileSync(file, "utf8").trim();
  if (!key) throw new Error("the key file is empty");
}

/** One request; the status and the parsed body, `null` when the body isn't JSON. */
async function send(method, path, body, withKey = true) {
  const response = await fetch(`${API}${path}`, {
    method,
    headers: { ...(withKey ? { Authorization: `Bearer ${key}` } : {}), "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(60_000),
  });
  const text = await response.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    // not JSON: the status alone speaks
  }
  return { status: response.status, json };
}

/** The message of an error reply, which names the policy and never a key; cut to one line. */
const why = (reply) => String(reply.json?.error?.message ?? "no message").split("\n")[0].slice(0, 140);

const chatRequest = (model, provider) => ({
  model,
  messages: [{ role: "user", content: "Reply with the word OK." }],
  max_tokens: 5,
  temperature: 0,
  ...(provider ? { provider } : {}),
});

const models = configuredModels();
if (models.size === 0) throw new Error("no OpenRouter model found in the configuration");

// the model without a zero-retention endpoint: from OpenRouter's own lists, a text model none of whose
// endpoints is in the zero-retention list; a free one when there is one, so the control costs nothing
async function modelWithoutZdr() {
  const zdr = await send("GET", "/v1/endpoints/zdr", undefined, false);
  const listed = await send("GET", "/v1/models", undefined, false);
  const kept = new Set((zdr.json?.data ?? []).map((endpoint) => endpoint.model_id));
  const all = (listed.json?.data ?? []).filter((m) => (m.architecture?.output_modalities ?? []).includes("text") && (m.architecture?.input_modalities ?? []).includes("text"));
  if (kept.size === 0 || all.length === 0) throw new Error("OpenRouter's model lists couldn't be read");
  const without = all.filter((m) => !kept.has(m.id) && !models.has(m.id));
  const free = without.filter((m) => m.id.endsWith(":free") || Number(m.pricing?.prompt) === 0);
  const price = (m) => Number(m.pricing?.prompt ?? 0) + Number(m.pricing?.completion ?? 0);
  const pick = (free.length ? free : without.sort((a, b) => price(a) - price(b)))[0];
  if (!pick) throw new Error("every text model of OpenRouter has a zero-retention endpoint, so there is none to refuse");
  return { id: pick.id, free: free.length > 0 };
}

if (dry) {
  console.log("would ask OpenRouter, with the provider settings", JSON.stringify(PROVIDER), "and a prompt of a few tokens:");
  for (const [slug, { kind, roles }] of models) console.log(`  ${kind === "jev" ? "/decisions" : "/chat/completions"}  ${slug}  (${[...roles].join(", ")})`);
  console.log("  and one text model that has no zero-retention endpoint, chosen from /models and /endpoints/zdr, expecting it refused");
  process.exit(0);
}

readKey();

// 1. the refusal
const bare = await modelWithoutZdr();
const refused = await send("POST", "/v1/chat/completions", chatRequest(bare.id, PROVIDER));
// a 4xx whose message names the policy: a refusal for another reason (a bad request, a rate limit) would pass for nothing
const byPolicy = refused.status >= 400 && refused.status < 500 && /zero data retention|data policy/i.test(why(refused));
report(byPolicy, `${bare.id}, which has no zero-retention endpoint, is refused under zdr:true by the data policy (HTTP ${refused.status}: ${why(refused)})`);
if (bare.free) {
  const open = await send("POST", "/v1/chat/completions", chatRequest(bare.id, undefined));
  if (open.status === 200) {
    report(true, `the same request without the setting is answered (HTTP 200), so the refusal is the policy's`);
  } else {
    console.log(`note: the control without the setting got HTTP ${open.status} (${why(open)}): the refusal above is not shown to be the policy's`);
  }
} else {
  console.log("note: no free model without a zero-retention endpoint, so there is no unrestricted control; the refusal stands on its message");
}

if (refusalOnly) {
  console.log(failures.length === 0 ? "the refusal held" : "the refusal did not hold");
  process.exit(failures.length === 0 ? 0 : 1);
}

// 2. every configured model passes
for (const [slug, { kind, roles }] of models) {
  const label = `${slug} (${[...roles].join(", ")})`;
  if (kind === "jev") {
    const state = { question: { text: "Is the export tested?", role: "pm", project: "delta" } };
    const questions = { asks_for_code: { type: "noul", instructions: "`question.text` asks to see source code or file paths." } };
    const reply = await send("POST", "/alpha/decisions", { model: slug, provider: PROVIDER, state, questions });
    report(reply.status === 200 && reply.json?.answers && typeof reply.json?.usage === "object", `${label} answers under zdr:true (HTTP ${reply.status}${reply.status === 200 ? `, snapshot ${reply.json?.model}` : `: ${why(reply)}`})`);
  } else {
    const reply = await send("POST", "/v1/chat/completions", chatRequest(slug, PROVIDER));
    const answered = reply.status === 200 && Array.isArray(reply.json?.choices) && reply.json.choices.length > 0;
    report(answered, `${label} answers under zdr:true (HTTP ${reply.status}${answered ? `, by ${reply.json?.provider ?? "an endpoint that keeps no data"}` : `: ${why(reply)}`})`);
  }
}

console.log(failures.length === 0 ? "the zero-retention check held" : `the zero-retention check did not hold: ${failures.length} failed`);
process.exit(failures.length === 0 ? 0 : 1);

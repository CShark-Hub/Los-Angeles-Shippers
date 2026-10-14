// Records the replies the judge tests read: the same two requests the tests build (tests/jev.rs and
// tests/chat.rs, `batch()`), sent to the real endpoints, their replies saved as they came in
// `recorded/`. Run it from the root of the repository when a reply no longer fits its test, or
// when the pinned model moves:
//
//   node crates/mercury-judge/tests/fixtures/record.mjs
//
// The key is read from deploy/compose/secrets/openrouter_api_key (OPENROUTER_API_KEY_FILE) inside this
// process and goes only into the Authorization header. What is saved is the reply, nothing of the
// request: no key, no header. The questions are synthetic.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../../../..");
const keyFile = process.env.OPENROUTER_API_KEY_FILE ?? join(root, "deploy/compose/secrets/openrouter_api_key");
const key = readFileSync(keyFile, "utf8").trim();
if (!key) throw new Error("the key file is empty");

const DECISIONS = "https://openrouter.ai/api/alpha/decisions";
const CHAT = "https://openrouter.ai/api/v1/chat/completions";
const JEV = "typesafe/jev-1.13";
const CHAT_MODEL = "openai/gpt-4.1-mini";
const PROVIDER = { data_collection: "deny", zdr: true };

// the batch of both tests: one question of each type, about one state
const state = { question: { text: "Is the export tested?", role: "pm", project: "delta" } };
const questions = {
  asks_for_code: { type: "noul", instructions: "`question.text` asks to see source code or file paths." },
  class: {
    type: "choice",
    instructions: "The kind of question `question.text` is.",
    criteria: { progress: null, tested: null, forbidden: "asks for something Mercury never shows" },
  },
  health: {
    type: "score",
    instructions: "How healthy the build of `question.project` is.",
    criteria: ["failing", "flaky", "green"],
  },
};

async function post(url, body) {
  const res = await fetch(url, {
    method: "POST",
    headers: { Authorization: `Bearer ${key}`, "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(30_000),
  });
  const text = await res.text();
  if (res.status !== 200) throw new Error(`${url}: HTTP ${res.status}`);
  return JSON.parse(text);
}

mkdirSync(join(here, "recorded"), { recursive: true });
const save = (name, value) => writeFileSync(join(here, "recorded", name), JSON.stringify(value, null, 2) + "\n", "utf8");

// 1. the Decisions endpoint, as JevJudge sends it
const decisions = await post(DECISIONS, { model: JEV, provider: PROVIDER, state, questions });
save("decisions.json", decisions);
console.log(`decisions.json: ${decisions.model}, ${Object.keys(decisions.answers).join(", ")}`);

// 2. the chat endpoint, as ChatJudge sends it: the prompt as the system message, the batch as the user's
const prompt = readFileSync(join(root, "config-examples/prompts/judge_fallback.v1.md"), "utf8").replace(/\r\n/g, "\n");
const chat = await post(CHAT, {
  model: CHAT_MODEL,
  messages: [
    { role: "system", content: prompt },
    { role: "user", content: JSON.stringify({ state, questions }) },
  ],
  temperature: 0,
  max_tokens: 300,
  response_format: { type: "json_object" },
  provider: PROVIDER,
});
save("chat.json", chat);
console.log(`chat.json: ${chat.model}, finish ${chat.choices?.[0]?.finish_reason}`);

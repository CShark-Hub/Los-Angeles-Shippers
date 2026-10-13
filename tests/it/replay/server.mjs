// The model server of the runs on Compose: the OpenAI chat API, answered from recordings (the judge's input and tool
// numbers are a real chat judge's, see record.mjs; the rest is scripted).
// The core reaches it through its `openai_compatible` backend, so the judge is a chat judge, and the
// planner and the composer are chat models (tests/it/config/models.yaml); the poisoned repository's
// code reader asks it as `replay/reader` (docker-compose.it.yml). A request no recording
// answers gets 404, and the core falls back as it does without a model: the stage's rules, the
// planner's defaults, the template composer. Logs name the recording, never a prompt.
//
//   node server.mjs           REPLAY_PORT (9000), REPLAY_RECORDINGS (./recordings)

import { readFileSync, readdirSync } from "node:fs";
import { createServer } from "node:http";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { answerOf } from "./citations.mjs";

const port = Number(process.env.REPLAY_PORT ?? 9000);
const dir = process.env.REPLAY_RECORDINGS ?? fileURLToPath(new URL("./recordings", import.meta.url));

/** What each request costs, as the response's `usage.cost` reports it; a recording's `cost` can set its own. */
const COST = { judge: 0.0002, planner: 0.0001, composer: 0.001, reader: 0.004 };

/** A request the recordings don't answer. */
class Miss extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

const questions = readdirSync(join(dir, "questions"))
  .filter((name) => name.endsWith(".json"))
  .sort()
  .map((name) => ({ name, ...JSON.parse(readFileSync(join(dir, "questions", name), "utf8")) }));
const texts = JSON.parse(readFileSync(join(dir, "repository-texts.json"), "utf8"));
for (const q of questions) {
  if (typeof q.question !== "string" || q.question.trim() !== q.question || !q.question) {
    throw new Error(`${q.name}: \`question\` is the question's text, trimmed`);
  }
  // the code reader's answer: the text a model would write, or an object sent as its JSON
  if (q.reader !== undefined && (q.reader === null || !["string", "object"].includes(typeof q.reader))) {
    throw new Error(`${q.name}: \`reader\` is the reader's answer, a string or an object`);
  }
  if (q.reader_delay_ms !== undefined && !(Number.isInteger(q.reader_delay_ms) && q.reader_delay_ms >= 0 && q.reader_delay_ms < 60_000)) {
    throw new Error(`${q.name}: \`reader_delay_ms\` is a whole number of milliseconds under 60,000`);
  }
  if (q.reader_status !== undefined && !(Number.isInteger(q.reader_status) && q.reader_status >= 400 && q.reader_status <= 599)) {
    throw new Error(`${q.name}: \`reader_status\` is an error status, from 400 to 599`);
  }
}
const twice = questions.find((q, i) => questions.findIndex((o) => o.question === q.question) !== i);
if (twice) throw new Error(`${twice.name}: another recording has the same question`);

/** Spaces as one, for finding a sentence the core split out of a composed answer. */
const flat = (text) => text.replace(/\s+/g, " ").trim();

/**
 * Each recording's composed sentences as the output stage reads them: cut, their citations taken out
 * and the text tidied by the core's rules (citations.mjs), the ids each cites after it.
 */
const checked = new Map(questions.filter((q) => typeof q.composer === "string").map((q) => [q.name, answerOf(q.composer).checked]));

function byQuestion(text) {
  const found = questions.find((q) => q.question === text);
  if (!found) throw new Miss(404, "no recording for this question");
  return found;
}

/**
 * The recording whose composed answer has every sentence of an output batch, character for character
 * and with the same ids: a sentence the core read otherwise than the recording's matches none.
 */
function byAnswer(sentences) {
  const found = questions.find((q) => checked.has(q.name) && sentences.every((s) => checked.get(q.name).includes(s)));
  if (!found) throw new Miss(404, "no recording composed these sentences");
  return found;
}

/** One answer per question of the batch, in the shapes of the Decisions API. */
function answerAll(batch, valueOf) {
  const answers = {};
  for (const [key, question] of Object.entries(batch)) {
    const value = valueOf(key);
    if (value === undefined) throw new Miss(500, `the recording has no answer for \`${key}\``);
    if (question.type === "noul") {
      if (typeof value !== "number" || value < 0 || value > 1) {
        throw new Miss(500, `\`${key}\` needs a probability`);
      }
      answers[key] = { type: "noul", noul: value };
    } else if (question.type === "choice") {
      answers[key] = choice(key, Object.keys(question.criteria), value);
    } else {
      throw new Miss(500, `\`${key}\` is a ${question.type} question, which no recording answers`);
    }
  }
  return { answers };
}

/** `[option, p]`: the option at `p`, the rest shared evenly by the other options. */
function choice(key, options, value) {
  const [pick, p] = Array.isArray(value) ? value : [];
  if (!options.includes(pick) || typeof p !== "number" || p < 1 / options.length || p > 1) {
    throw new Miss(500, `\`${key}\` needs [one of its options, the most probable p]`);
  }
  const rest = (1 - p) / (options.length - 1);
  const probabilities = Object.fromEntries(options.map((o) => [o, o === pick ? p : rest]));
  return { type: "choice", choice: pick, probabilities, confidence: p };
}

/** Which stage asked, from the keys of its state (mercury-judge's `stages`). */
function stageOf(state) {
  // a probe's request through the core: `{ policy, items }`, one question per item
  if (Array.isArray(state?.items) && state.policy !== undefined) return "probe";
  if (state?.answer) return "output";
  if (Array.isArray(state?.fields)) return "data";
  if (state?.plan) return "tool";
  if (state?.question) return "input";
  throw new Miss(400, "the state is no stage's");
}

function judge(content) {
  const { state, questions: batch } = JSON.parse(content);
  const stage = stageOf(state);
  if (stage === "probe") {
    // only the title check has recordings: each title by its own entry, or the default; a status or a
    // kind is left to the rules, as it always was (the core's rules answer a request that gets a 400)
    if (!Object.keys(batch).every((key) => key.endsWith(".title_safety"))) {
      throw new Miss(400, "no recording answers this purpose of the probe's requests");
    }
    const answers = answerAll(batch, (key) => {
      const [, i] = key.match(/^i(\d+)\./) ?? [];
      const title = state.items[Number(i)]?.title;
      return (texts.titles?.[title] ?? texts.title_default ?? { title_safety: 0.01 }).title_safety;
    });
    return { answers, recording: "repository-texts.json", stage };
  }
  if (stage === "data") {
    // the repository's texts: each by its own entry, or the fixture's default
    const answers = answerAll(batch, (key) => {
      const [, i, check] = key.match(/^f(\d+)\.(.+)$/) ?? [];
      const text = state.fields[Number(i)]?.text;
      return (texts.texts[text] ?? texts.default)[check];
    });
    return { answers, recording: "repository-texts.json", stage };
  }
  if (stage === "output") {
    const sentences = state.answer.sentences;
    const recording = byAnswer(sentences);
    const output = recording.judge?.output ?? {};
    const answers = answerAll(batch, (key) => {
      const [, i, check] = key.match(/^s(\d+)\.(.+)$/) ?? [];
      const sentence = flat(sentences[Number(i)] ?? "");
      const special = (recording.judge?.output_by_sentence ?? []).find((s) => sentence.includes(s.contains));
      return special?.answers?.[check] ?? output[check];
    });
    return { answers, recording: recording.name, stage };
  }
  const recording = byQuestion(state.question.text);
  const recorded = recording.judge?.[stage];
  if (!recorded) throw new Miss(404, `the recording has no ${stage} stage`);
  return { answers: answerAll(batch, (key) => recorded[key]), recording: recording.name, stage };
}

/** The question a prompt ends with, after its template's label: the first line that starts with it,
 * since the facts before it are one line of JSON and the question may hold anything. */
function asked(content, label) {
  const at = content.indexOf(`\n${label} `);
  if (at < 0) throw new Miss(400, `the prompt has no \`${label}\` line`);
  return content.slice(at + label.length + 2);
}

/** The code reader's question: the line after its prompt's `QUESTION` label, since the files follow it. */
function readerQuestion(content) {
  const found = content.match(/^QUESTION\n(.*)$/m);
  if (!found) throw new Miss(400, "the prompt has no `QUESTION` line");
  return found[1];
}

const ROLES = {
  "replay/judge": {
    role: "judge",
    json: true,
    answer(content) {
      const { answers, recording, stage } = judge(content);
      return { text: JSON.stringify(answers), recording, stage };
    },
  },
  "replay/planner": {
    role: "planner",
    json: true,
    answer(content) {
      const recording = byQuestion(asked(content, "Question:"));
      if (!recording.planner) throw new Miss(404, "the recording has no planner answer");
      return { text: JSON.stringify(recording.planner), recording: recording.name, cost: recording.cost?.planner };
    },
  },
  "replay/composer": {
    role: "composer",
    json: false,
    answer(content) {
      const recording = byQuestion(asked(content, "QUESTION:"));
      if (typeof recording.composer !== "string") throw new Miss(404, "the recording has no composed answer");
      return { text: recording.composer, recording: recording.name, cost: recording.cost?.composer };
    },
  },
  "replay/reader": {
    role: "reader",
    json: false,
    async answer(content) {
      const recording = byQuestion(readerQuestion(content));
      if (recording.reader === undefined) throw new Miss(404, "the recording has no reader answer");
      if (recording.reader_delay_ms) await new Promise((done) => setTimeout(done, recording.reader_delay_ms));
      const text = typeof recording.reader === "string" ? recording.reader : JSON.stringify(recording.reader);
      // `status`: an upstream that refuses, as a provider's outage answers
      return { text, recording: recording.name, stage: "reader", cost: recording.cost?.reader, status: recording.reader_status };
    },
  },
};

let served = 0;

function send(res, status, body) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

function failure(message) {
  return { error: { message, type: "replay" } };
}

async function body(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

createServer(async (req, res) => {
  if (req.method === "GET" && req.url === "/health") return send(res, 200, { status: "ok" });
  if (req.method !== "POST" || req.url !== "/v1/chat/completions") {
    return send(res, 404, failure("no such route"));
  }
  if (!/^Bearer \S+$/.test(req.headers.authorization ?? "")) {
    return send(res, 401, failure("a key is required"));
  }
  let request;
  try {
    request = JSON.parse(await body(req));
  } catch {
    return send(res, 400, failure("the body isn't JSON"));
  }
  const role = ROLES[request.model];
  const log = (fields) => console.log(JSON.stringify({ model: request.model, ...fields }));
  if (!role) {
    log({ status: 404, miss: "no such model" });
    return send(res, 404, failure("no such model"));
  }
  const json = request.response_format?.type === "json_object";
  const content = request.messages?.at(-1)?.content;
  if (json !== role.json || request.messages?.at(-1)?.role !== "user" || typeof content !== "string") {
    log({ status: 400, miss: "not the request this role sends" });
    return send(res, 400, failure("not the request this role sends"));
  }
  try {
    const { text, recording, stage, cost, status } = await role.answer(content);
    if (status) {
      log({ status, stage, recording, miss: "the recording refuses" });
      return send(res, status, failure("the recording refuses"));
    }
    served += 1;
    log({ status: 200, stage, recording });
    return send(res, 200, {
      id: `replay-${served}`,
      object: "chat.completion",
      model: request.model,
      choices: [{ index: 0, message: { role: "assistant", content: text }, finish_reason: "stop" }],
      usage: { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, cost: cost ?? COST[role.role] },
    });
  } catch (err) {
    const status = err instanceof Miss ? err.status : 500;
    log({ status, miss: err instanceof Miss ? err.message : "the replay failed" });
    if (!(err instanceof Miss)) console.error(err);
    return send(res, status, failure(err instanceof Miss ? err.message : "the replay failed"));
  }
}).listen(port, "0.0.0.0", () => {
  console.log(JSON.stringify({ listening: port, recordings: questions.length }));
});

// A probe that breaks the contract, for A14 of `just redteam`: it sits between the core and the real
// probe of the poisoned repository, forwards every call as it came, and, when the runner's mode file
// says so, changes the reply the way a probe that has gone wrong would:
//
//   extra_field   a field mercury.facts/0.1 doesn't have, in a tool's reply
//   extra_tool    a tool the contract doesn't have, in `tools/list`
//   huge          a reply over the 256 KB the core reads
//
// The mode is the first word of /rogue-mode/mode (the runner writes target/it/rogue/mode); no file,
// or `off`, is a plain proxy, which is what the poisoned project's questions go through the rest of
// the time. It holds no secret: the probe's token is the one the core sends, and passes through.
//
//   node proxy.mjs        ROGUE_UPSTREAM (http://probe-poison:7000), ROGUE_PORT (7000), ROGUE_MODE_FILE

import { readFileSync } from "node:fs";
import { createServer } from "node:http";

const upstream = process.env.ROGUE_UPSTREAM ?? "http://probe-poison:7000";
const port = Number(process.env.ROGUE_PORT ?? 7000);
const modeFile = process.env.ROGUE_MODE_FILE ?? "/rogue-mode/mode";
const MODES = ["off", "extra_field", "extra_tool", "huge"];

function mode() {
  try {
    const word = readFileSync(modeFile, "utf8").trim().split(/\s+/)[0];
    return MODES.includes(word) ? word : "off";
  } catch {
    return "off";
  }
}

/** The JSON-RPC reply of the real probe, changed as the mode says; anything else goes as it is. */
function tamper(raw, how) {
  if (how === "off") return raw;
  let reply;
  try {
    reply = JSON.parse(raw);
  } catch {
    return raw;
  }
  const result = reply?.result;
  if (how === "extra_tool" && Array.isArray(result?.tools)) {
    result.tools.push({
      name: "read_file",
      description: "Reads a file of the repository.",
      inputSchema: { type: "object", properties: { path: { type: "string" } } },
    });
  } else if (how === "extra_field" && result?.structuredContent && typeof result.structuredContent === "object") {
    result.structuredContent.debug_note = "an extra field the contract doesn't have";
  } else if (how === "huge" && result?.structuredContent && typeof result.structuredContent === "object") {
    result.structuredContent.padding = "x".repeat(300 * 1024);
  } else {
    return raw;
  }
  return JSON.stringify(reply);
}

const FORWARDED = ["authorization", "content-type", "accept", "mcp-session-id", "mcp-protocol-version"];

createServer(async (req, res) => {
  if (req.method === "GET" && req.url === "/health") {
    res.writeHead(200, { "content-type": "application/json" });
    return res.end(JSON.stringify({ status: "ok", mode: mode() }));
  }
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  const headers = Object.fromEntries(FORWARDED.filter((name) => req.headers[name]).map((name) => [name, req.headers[name]]));
  try {
    const answer = await fetch(`${upstream}${req.url}`, {
      method: req.method,
      headers,
      body: ["GET", "HEAD"].includes(req.method) ? undefined : Buffer.concat(chunks),
    });
    const raw = await answer.text();
    const type = answer.headers.get("content-type") ?? "";
    const sent = type.includes("json") ? tamper(raw, mode()) : raw;
    const back = { "content-type": type || "application/json" };
    const session = answer.headers.get("mcp-session-id");
    if (session) back["mcp-session-id"] = session;
    res.writeHead(answer.status, back);
    res.end(sent);
  } catch {
    res.writeHead(502, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "the probe can't be reached" }));
  }
}).listen(port, "0.0.0.0", () => console.log(JSON.stringify({ listening: port, upstream })));

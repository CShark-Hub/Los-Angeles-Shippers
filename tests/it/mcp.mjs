// A small MCP client for the runs on Compose (`just it`, `just redteam`): JSON-RPC over Streamable
// HTTP with plain fetch, the way an agent such as Claude Code talks to the core's `/mcp` through
// Caddy. The core is stateless: every POST carries the Bearer token and is checked on its own, and
// it answers with one JSON body or with an SSE stream whose `data:` lines are JSON-RPC messages
// (progress notifications first, then the response with the request's id). The client sends
// `initialize` and `notifications/initialized` first, as an agent does, and keeps every byte it
// read, so a run can check that no secret or planted text came out.

import { URL } from "./stack.mjs";

/** The protocol version the client asks for: the one that has a tool result's `structuredContent`. */
export const PROTOCOL_VERSION = "2025-06-18";
/** What an MCP client accepts from a POST: one JSON body, or an SSE stream. */
export const ACCEPT = "application/json, text/event-stream";
/** The names a progress notification may carry: the events before the end, never any text. */
export const PROGRESS_NAMES = ["accepted", "project", "classified", "plan", "tool_checked", "tool_result", "composing", "judged"];

/**
 * The JSON-RPC messages of an SSE text, in order: each event's `data:` lines joined, parsed as JSON.
 * An event without data (a comment, a priming event) or whose data isn't JSON is skipped; a batch
 * (an array) gives each of its messages.
 */
export function sseMessages(text) {
  const messages = [];
  for (const block of text.replace(/\r\n?/g, "\n").split("\n\n")) {
    const data = block
      .split("\n")
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice(5).replace(/^ /, ""));
    if (data.length === 0) continue;
    try {
      messages.push(...[JSON.parse(data.join("\n"))].flat());
    } catch {
      // not a JSON-RPC message
    }
  }
  return messages;
}

/** The JSON-RPC messages of a response body, by its content type: an SSE stream or one JSON body. */
export function messagesOf(contentType, text) {
  if (/^text\/event-stream\b/i.test(contentType ?? "")) return sseMessages(text);
  try {
    return [JSON.parse(text)].flat();
  } catch {
    return [];
  }
}

/**
 * The response to the request `id` among `messages` (its `result` or its `error`) and the params of
 * the `notifications/progress` that came before it.
 */
export function responseTo(messages, id) {
  const progress = [];
  for (const message of messages) {
    if (message?.method === "notifications/progress") progress.push(message.params ?? {});
    if (message?.id === id && ("result" in message || "error" in message)) {
      return { response: message, progress };
    }
  }
  return { response: null, progress };
}

/**
 * One POST to `/mcp`. Reads an SSE stream until the response with `id` arrives (then stops reading,
 * whether or not the server ends the stream), any other body to its end. Gives the status, the
 * headers (lower-case names), the text read and the messages in it.
 */
async function post(body, { headers, id, seconds }) {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), seconds * 1000);
  try {
    const res = await fetch(`${URL}/mcp`, {
      method: "POST",
      headers: { "content-type": "application/json", accept: ACCEPT, ...headers },
      body: JSON.stringify(body),
      signal: abort.signal,
    });
    const contentType = res.headers.get("content-type") ?? "";
    let text = "";
    if (/^text\/event-stream\b/i.test(contentType) && res.body && id !== undefined) {
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        text += decoder.decode(value, { stream: true });
        if (responseTo(sseMessages(text), id).response) {
          await reader.cancel().catch(() => {});
          break;
        }
      }
    } else {
      text = await res.text();
    }
    return {
      status: res.status,
      headers: Object.fromEntries([...res.headers].map(([name, value]) => [name.toLowerCase(), value])),
      contentType,
      text,
      messages: messagesOf(contentType, text),
    };
  } catch (err) {
    if (abort.signal.aborted) throw new Error(`POST /mcp didn't answer within ${seconds} s`);
    throw err;
  } finally {
    clearTimeout(timer);
  }
}

/**
 * A client acting with `token`. Each call gives `{ status, headers, result, error, progress, raw }`:
 * the HTTP status, the response's headers, the JSON-RPC result or error, the progress
 * notifications' params, and the text read (`raw`), for the checks on what came out.
 */
export function mcpClient(token, { name = "mercury-it" } = {}) {
  let next = 0;
  let version = null;
  let session = null;
  const headers = () => ({
    ...(token ? { authorization: `Bearer ${token}` } : {}),
    ...(version ? { "mcp-protocol-version": version } : {}),
    // the core issues none; a server that did would expect it back
    ...(session ? { "mcp-session-id": session } : {}),
  });

  async function request(method, params, seconds = 30) {
    next += 1;
    const id = next;
    const sent = await post({ jsonrpc: "2.0", id, method, params }, { headers: headers(), id, seconds });
    const { response, progress } = responseTo(sent.messages, id);
    return {
      status: sent.status,
      headers: sent.headers,
      contentType: sent.contentType,
      result: response?.result ?? null,
      error: response?.error ?? null,
      progress,
      raw: sent.text,
    };
  }

  return {
    /** `initialize`, then `notifications/initialized`; `initialized` is the second POST's status. */
    async initialize() {
      const init = await request("initialize", {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name, version: "0.0.0" },
      });
      session = init.headers["mcp-session-id"] ?? null;
      if (init.result?.protocolVersion) version = init.result.protocolVersion;
      let initialized = null;
      if (init.result) {
        const sent = await post({ jsonrpc: "2.0", method: "notifications/initialized" }, { headers: headers(), seconds: 30 });
        initialized = sent.status;
        init.raw += `\n${sent.text}`;
      }
      return { ...init, initialized };
    },
    listTools() {
      return request("tools/list", {});
    },
    /** `tools/call`; with `progressToken` the call asks for progress notifications. */
    callTool(tool, args, { progressToken, seconds = 120 } = {}) {
      const params = { name: tool, arguments: args };
      if (progressToken !== undefined) params._meta = { progressToken };
      return request("tools/call", params, seconds);
    },
  };
}

/** The text items of a tool result, joined: what an agent reads besides `structuredContent`. */
export function textOf(result) {
  return (result?.content ?? [])
    .filter((item) => item?.type === "text")
    .map((item) => item.text)
    .join("\n");
}

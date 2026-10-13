// The web client's side of `just it`: what a browser does with the core through Caddy, written as
// plain HTTP the way curl would send it: sign in with the token, use the cookie, a write from another
// page, sign out, a revoked token's cookie and the sign-in limit. The Rust tests cover the rules one
// by one; this is the same rules on the real stack, behind Caddy, which sets the forwarded address
// the limit counts.

import { PEOPLE, PAGE, admin, http, signIn } from "./stack.mjs";

/** The public id of a token: the 12 characters after `mcy_`. */
const publicId = (token) => token.slice(4, 16);

/**
 * Runs the checks with `t` (`ok` and `eq`), as `people.pm` and `people.board`; each check is
 * independent of the order the others ran in, except that the limit goes last.
 * `question` is a question the replay server has a recording for.
 */
export async function webChecks(people, t, question) {
  const asPage = (cookie, extra = {}) => ({ headers: { ...PAGE, cookie, ...extra } });

  // sign in: 204 and a cookie a page can't read, tied to nothing but its own id
  const { response: signedIn, cookie } = await signIn(people.pm);
  t.eq(signedIn.status, 204, "sign-in status");
  const cookies = signedIn.headers["set-cookie"] ?? [];
  t.eq(cookies.length, 1, "one cookie is set");
  const attributes = (cookies[0] ?? "").split("; ");
  t.ok(attributes[0]?.startsWith("__Host-mercury_session="), "the __Host- prefix");
  t.eq(attributes.slice(1, 5), ["Path=/", "Secure", "HttpOnly", "SameSite=Strict"], "the cookie's attributes");
  t.ok(/^Max-Age=\d+$/.test(attributes[5] ?? "") && !attributes.some((a) => /^Domain=/i.test(a)), "a lifetime and no domain");
  t.ok(!signedIn.text.includes(people.pm), "the token isn't in the answer");
  if (!cookie) return;

  // the cookie stands for the token: who am I, then a question asked with it and followed to its end
  const me = await http("GET", "/v1/me", { headers: { cookie } });
  t.eq([me.status, me.json?.user?.email], [200, PEOPLE.pm.email], "who am I, by cookie");
  const asked = await http("POST", "/v1/questions", {
    ...asPage(cookie, { "idempotency-key": "web-1" }),
    body: { text: question, project: "delta" },
  });
  t.eq(asked.status, 202, "asking with the cookie from the page");
  const id = asked.json?.id;
  if (id) {
    const events = await http("GET", `/v1/questions/${id}/events`, { headers: { cookie } });
    t.eq(events.status, 200, "the event stream by cookie");
    t.ok(/^event: done$/m.test(events.text), "the stream ends in done");
    const state = await http("GET", `/v1/questions/${id}`, { headers: { cookie } });
    t.eq([state.status, state.json?.status], [200, "delivered"], "the state, by cookie");
  }

  // a write from another page, with no origin, or without the client's header: 403, and the session stands
  for (const [what, headers] of [
    ["another site's page", { origin: "https://evil.example.org", "x-mercury-client": "web" }],
    ["a form post from another site", { origin: "https://evil.example.org", "content-type": "application/x-www-form-urlencoded" }],
    ["curl with the cookie and no origin", {}],
    ["the right origin, no client header", { origin: PAGE.origin }],
  ]) {
    const refused = await http("POST", "/v1/questions", {
      headers: { ...headers, cookie, "idempotency-key": "web-csrf" },
      body: { text: question, project: "delta" },
    });
    t.eq([refused.status, refused.json?.error?.code], [403, "invalid_request"], `${what}: refused`);
  }
  const still = await http("GET", "/v1/me", { headers: { cookie } });
  t.eq(still.status, 200, "the session stands after the refused writes");

  // no CORS, through Caddy too: another site's origin is given no permission on a read, a preflight
  // or a refused write, so a page there can't read an answer or send the cookie where it may be read
  const other = "https://evil.example.org";
  for (const [method, path, headers] of [
    ["GET", "/v1/health", {}],
    ["GET", "/v1/me", { cookie }],
    ["OPTIONS", "/v1/questions", { "access-control-request-method": "POST", "access-control-request-headers": "content-type" }],
    ["POST", "/v1/questions", { cookie, "idempotency-key": "web-cors", "x-mercury-client": "web" }],
  ]) {
    const answered = await http(method, path, { headers: { ...headers, origin: other }, body: method === "POST" ? { text: question, project: "delta" } : undefined });
    const permissions = Object.keys(answered.headers).filter((name) => name.startsWith("access-control-"));
    t.eq(permissions, [], `${method} ${path} from another site carries no CORS header (HTTP ${answered.status})`);
  }

  // sign out: the cookie is cleared and the old one is dead
  const out = await http("DELETE", "/v1/session", asPage(cookie));
  t.eq(out.status, 204, "sign-out status");
  t.ok(/Max-Age=0\b/.test(out.headers["set-cookie"]?.[0] ?? ""), "the cookie is cleared");
  const replayed = await http("GET", "/v1/me", { headers: { cookie } });
  t.eq(replayed.status, 401, "the old cookie after sign-out");

  // an admin revokes a token: the next request with its cookie is 401
  const board = await signIn(people.board);
  t.eq(board.response.status, 204, "a second person signs in");
  const before = await http("GET", "/v1/me", { headers: { cookie: board.cookie } });
  t.eq(before.status, 200, "the second person's session works");
  t.ok(/^revoked /.test(admin(["token", "revoke", publicId(people.board)]).trim()), "the admin revokes the token");
  const after = await http("GET", "/v1/me", { headers: { cookie: board.cookie } });
  t.eq([after.status, after.json?.error?.code], [401, "unauthenticated"], "the cookie of a revoked token, in one request");

  // the sign-in limit, from the address Caddy forwards: 429 with Retry-After within a few attempts
  const seen = [];
  let limited = null;
  for (let attempt = 0; attempt < 8 && !limited; attempt += 1) {
    const tried = await http("POST", "/v1/session", { headers: PAGE, body: { token: "mcy_nothing" } });
    seen.push(tried.status);
    if (tried.status === 429) limited = tried;
  }
  t.ok(limited !== null, `a 429 within eight attempts: ${seen.join(", ")}`);
  if (limited) {
    t.ok(seen.slice(0, -1).every((status) => status === 401), `the attempts before it were refused as wrong tokens: ${seen.join(", ")}`);
    const wait = Number(limited.headers["retry-after"]);
    t.ok(Number.isInteger(wait) && wait >= 1 && wait <= 6, `Retry-After ${limited.headers["retry-after"]}`);
    t.eq(limited.json?.error?.code, "rate_limited", "the limit's error code");
  }
}

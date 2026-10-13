# The runs on Compose

`just it`, `just golden` and `just redteam` ask whole questions through the stack on Docker Compose:
the core, Caddy, the fixture's probe in dev mode (registered with the core) and a replay server in
place of the model APIs. They ask with `mercury ask --json`, as a script would, or over MCP, as an AI
agent would, and read what each question left in the audit through its event stream, which the core
serves from the audit rows.

```bash
just it                    # the scenarios of run.mjs
just it --web              # and the web client in a real browser (apps/web/e2e); `just ci` runs this one
just golden                # tests/golden/questions.jsonl → tests/golden/reports/<date>-golden.md
just golden --subset forbidden
just redteam               # tests/redteam/attacks.json: the attack questions, A8 over MCP, A17 against the browser sessions
just it --keep             # any of them: leave the stack up on http://127.0.0.1:18080
```

Each run builds the images, starts a fresh stack under the project name `mercury-it` with Caddy on
`127.0.0.1:18080` and the probes network on `172.30.20.0/24`, so a `just compose-up` stack (its
network is `172.30.10.0/24`) can run beside it, and takes it down with its volumes afterwards.
`stack.mjs` writes `target/it/`: the configuration examples with `config/models.yaml` over them and
higher rate limits, and random secrets (the probe token, the replay server's key, an empty OpenRouter
key). It creates one person per role (`pm@example.com`, `lead@example.com`, `board@example.com`,
`admin@example.com`), each granted `delta`, and the red team's attacker (`red@example.com`, granted `poison` alone), with a token each; no output, log or event may hold a token or a secret, and
`just it` also checks that no log holds a question. A failed read of the logs fails the run rather
than passing those checks on nothing, and whatever a failed run prints has the run's secrets masked.

- **The web client's side.** `web.mjs` signs a person in with plain HTTP, the way curl would, and
  checks the session cookie's attributes, asking and following a question with it, a write with
  no origin, another site's origin or no client header (all `403`), sign-out, a revoked token's
  cookie (`401` in one request) and the sign-in limit (`429` with `Retry-After`). The core takes
  the visitor's address from the `X-Forwarded-For` Caddy sets, because it trusts the `edge`
  network; the run's page is `http://localhost:<MERCURY_IT_PORT>`, which the overlay sets as
  `MERCURY_PUBLIC_ORIGIN` (every browser keeps the `Secure` session cookie over plain http there;
  WebKit refuses it on `127.0.0.1`), while the scripts send their requests to `127.0.0.1`. A session id is a secret of the run like a token: no log may hold one.
- **The web client in a browser (`just it --web`).** `browser.mjs` runs the Playwright tests of
  `apps/web/e2e` against the same stack, whose Caddy runs the `mercury-web` image (the build and the
  CSP header it was built with, not the laptop file's mounts): sign in, ask and read the answer and the
  timeline, ask for code and read the refusal, disconnect and find the old cookie dead, and the page's
  policy: its CSP header, a fetch to another site and an injected script blocked, nothing in browser
  storage, a frame and a form post from another site refused, the page's own call to `/mcp` with its
  cookie refused (`401`), a session whose token is revoked while its page is open ending with the
  page's next request (the test is handed, as data, the call that revokes a spare token of the
  manager's: `composeCall` in `stack.mjs`), and the sign-in limit as the form words it; Ask and
  Approvals driven with the keyboard alone; sign-off, with the manager and the tech
  lead made approvers first: the manager's held answer approved by the tech lead on a phone-sized page
  and shown on the manager's Ask page, and the manager's own held answer offered without a button; and
  the audit as the admin reads it: the chain's status, a filter, and the JSON lines export saved as a
  file. They run as the project manager (and the tech lead and the admin where they say so), after
  the plain-HTTP session checks, and their output is masked and leak-checked like the rest. `just
  ci-e2e` runs them; the pipeline installs Playwright's Chromium first. On a machine with a browser
  already, `MERCURY_E2E_CHANNEL=msedge` (or `chrome`) uses it, and `pnpm exec playwright install
  chromium` in `apps/web` fetches Playwright's own. The core counts every sign-in attempt per address
  (ten a minute, a burst of three), so the signed-in tests share one session and a sign-in that is
  told to wait waits.
- **The budgets.** A second project manager (`spender@example.com`) asks a question whose
  recorded composer costs 0.60, twice: each ends with `error` `budget_exceeded` after the composer,
  with nothing delivered and the cost counted, and together they spend the person's 1.00 for the
  day, so the next question gets `429 budget_exceeded` and leaves one audit row. It runs after the
  other scenarios, so their costs don't depend on it.
- **The audit, an admin's alone.** The project manager asks a question; `GET /v1/audit?question=`
  refuses them and another person with `403 invalid_request`, and the asker's export too; the admin
  (`admin@example.com`) reads the question's rows, the same as its stream, with who asked and the
  hashes, and exports them as JSON lines; `mercury audit --export` writes the project's rows to
  `target/it-audit.jsonl`, and `mercury audit` as the project manager is refused.
- **An AI agent over MCP.** `mcp.mjs` is a small MCP client on plain `fetch`, the way Claude Code
  talks to `/mcp` through Caddy: every POST carries the Bearer token and `Accept: application/json,
  text/event-stream`, and the answer is read as one JSON body or as an SSE stream up to the response
  with the request's id. As the project manager it sends `initialize` and `notifications/initialized`
  (no `Mcp-Session-Id` comes back, and no CORS header), finds exactly the tools `ask` and
  `list_projects`, gets from `list_projects` what `GET /v1/projects` gives, and asks the recorded
  "what was merged this week?": delivered with the CLI's answer and facts, progress notifications that
  carry event names alone, `structuredContent` that is the question's `GET /v1/questions/{id}`, and the
  rows of a `/v1` question in the owner's name. "show me the export code" comes back as a result, not
  an error, refused `asks_for_code` with its hint, and an open question on delta is answered with the
  note that the code wasn't read in the tool's text. The three are on the `mcp` channel and the CLI's
  on `cli`: no route shows a question's channel, so `channelsOf` in `stack.mjs` reads it from a copy of
  the core's database (`docker compose cp`, then Node's own SQLite, which needs Node.js 22.13 or later;
  the copy goes at once). `/mcp` takes only the Bearer token: a valid session's cookie, alone or with
  the page's headers, and a revoked token get `401 unauthenticated`; `GET` and `DELETE` get `405`. It
  runs after the web checks, waiting out the sign-in limit they leave, and before sign-off. `just
  redteam` asks its A8 attacks with the same client.
- **The poisoned project.** `withStack(body, { poison: true })`, which `just redteam` uses, also starts a
  probe on `tests/redteam/repo-poisoned` and, in front of it, the proxy of `tests/redteam/rogue-probe`; the
  core's `poison` project calls the proxy, and `setRogue(mode)` makes it break the contract (see
  `tests/redteam/README.md`). Every run's configuration has the project, and every run has its token
  file. `just it` starts both too, with the proxy passing every call through, so a person can hold a
  second project (the clarify) and open questions read the code there; `just golden` starts neither.
  The check that no log holds a question reads the code reader's and its egress proxy's logs too.
- **Open questions and the code reader.** A question about the project that is none of the five kinds
  is `open`: it calls every fact tool the probe serves (`progress, tested, blocked, health, changes`)
  and then, on a probe with a code reader, `ask_code` with the question (call 5). The attacker
  (`red@example.com`) asks them on `poison`, whose recorded reader answers from the recording's
  `reader`; the manager asks on delta, which has no reader. The `open-…` recordings show each ending:
  - the reader's two findings cited in one sentence and an Invoicing figure in the other, the
    reader's result (`items: 2`, nothing withheld, `access: code_model`, a `p_min`), no finding among
    the answer's facts, the reader's 0.004 in `done`'s cost and one `replay/reader` line in the
    replay's log;
  - an estimate worded "an estimate from the repository, not a promise", kept by the output checks;
  - on delta, five calls and no reader, and the answer ending "The code wasn't read for this answer:
    it rests on the project's data alone." The same question over MCP gets the same text;
  - the weather refused `out_of_scope`, and a question about salaries refused `confidential` before
    any plan;
  - a policy whose global `deny` holds `open` (the run's `policies.yaml`, the core restarted on it and
    then on the file put back) refuses the question `forbidden_class`, with a hint that lists the
    five kinds alone;
  - a reader that answers after 50 s: the reader gives up on its upstream at 35 s, before the probe's
    40 s and the core's 45 s, so the answer comes after about 35 s from the facts alone, with no
    result for call 5, no reader's cost, and ends "The code couldn't be read this time, so this answer
    rests on the project's data alone.";
  - a reader reporting 0.06, over the 0.05 a question may cost here: `budget_exceeded` after the
    tool checks, the cost counted in the error, the question's row and the attacker's spend.
- **Sign-off.** Last, since it makes the project manager and the tech lead approvers of delta: the
  manager asks the question of `what-got-merged-in-the-last-week.json`, whose sentence about the
  client's export lands in the code check's review band, so the answer waits (`mercury ask` exits
  4). The manager can't approve their own question, and someone who approves nothing sees an empty
  list and can't decide; the tech lead approves with `mercury approvals --approve`, and
  `mercury ask --wait` gets the draft byte for byte as the list showed it. The tech lead's own
  question is then rejected by the manager with a note, which `ask --wait` gets as the hint.
- **The operator's paths** (`operator.mjs`), each a scenario of its own:
  - *A clarify:* a manager granted delta and poison asks without naming a project, gets `clarify`
    (exit 3) with the two as options, answers with `--clarify <id>:delta` and is answered from delta;
    an option the clarify didn't offer delivers nothing. `e2e/clarify.spec.ts` does the same in the
    browser (`MERCURY_E2E_CLARIFY_TOKEN`, `MERCURY_E2E_CLARIFY_QUESTION`).
  - *A client that retries or reconnects:* the same `Idempotency-Key` gives the same question, none is
    `400`, `Last-Event-ID` replays only the rows after it, and a body one byte over 64 KB is `413`
    with no question started.
  - *The revocation check:* a token used, then revoked: the next request is `401` within a second,
    the CLI exits 1, and `admin audit export --token` holds its question, its creation and its
    revocation.
  - *The flood check and the day's budget,* last, since each restarts the core on lowered values in
    the run's configuration: on the examples' limits (30 an hour, a burst of 5) one token's 100
    questions in a minute start five and get `429` with `Retry-After` for the rest; with the
    installation's daily cap below the day's spend a question from the CLI and one over MCP are both
    `budget_exceeded`, nothing spent.
  - *No code on a disk:* `check.sh disk` on the run's project after every question.
  - Two more scenarios ask `tested` and `health` as the manager (answers built on figures, no fact list).
- **Beside the stack, two checks of what no scenario shows.** `node tests/it/zdr.mjs` asks OpenRouter
  with `zdr: true` (about a tenth of a cent, with the key of `deploy/compose/secrets`): a model with
  no zero-retention endpoint is refused by the data policy, and every model the configuration names,
  Jev included, is answered; `--dry` lists the requests and `--refusal-only` makes the free one. And
  `MERCURY_COMPOSE_PROJECT=mercury-it bash deploy/compose/scripts/check.sh disk`, on a stack that
  `just it --keep` left up, looks for the fixture's files on every named volume and in Docker's data
  directory, after planting one to show it can see a copy.
- **Another run beside it** needs a stack of its own: `MERCURY_IT_PROJECT` (its files then go to
  `target/<project>/`), `MERCURY_IT_PORT`, `MERCURY_IT_PROBES_SUBNET` and `MERCURY_IT_EDGE_SUBNET`.
- **`MERCURY_CLI`** names a CLI built elsewhere; CI exports one from the image build
  (`just _cli-image`). Without it the run builds a debug CLI.
- Every `docker compose` call has a time limit (`up` five minutes, which covers the pulls), and the
  wait for the probe to register tries again after a transient failure.

## The replay server

`replay/server.mjs` answers the OpenAI chat API for the three models of `config/models.yaml`:
`replay/judge` (a chat judge, so every threshold is 0.2 stricter than Jev's), `replay/planner` and
`replay/composer`. The core reaches it through its `openai_compatible` backend. Nothing is generated:
every answer comes from a recording, and a request no recording answers gets 404, after which the
core falls back as it does without a model: the stage's rules, the planner's defaults, the template.

`replay/recordings/questions/*.json`, one file per question, matched by the question's exact text:

```json
{
  "question": "what was merged this week?",
  "about": "why the recording is what it is",
  "judge": {
    "input": { "class": ["progress", 0.86], "asks_for_code": 0.02, "…": 0.01 },
    "tool": { "args_match_question": 0.97, "args_free_of_instructions": 0.99 },
    "output": { "grounded": 0.95, "sentence_contains_path_or_secret": 0.01, "…": 0.01 },
    "output_by_sentence": [{ "contains": "src/Export/", "answers": { "sentence_contains_path_or_secret": 0.98 } }]
  },
  "planner": { "since": "2026-09-28", "module": null, "limit": null },
  "composer": "One piece of work was merged this week … [f_dxf03376gg]."
}
```

- Each request costs what `COST` in `server.mjs` says for its role, reported as `usage.cost`; a
  recording's `"cost": { "planner": …, "composer": … }` sets its own for that question.
- A `noul` check takes its probability; a `choice` takes `[option, p]`, and the other options share
  what is left evenly. Every check a stage asks needs an answer, or the replay answers 500.
- The output stage is matched by its sentences: the recording that has each of them, character for
  character and with its ids, among its `composer`'s sentences as the core reads them. A sentence the
  core read otherwise matches no recording, so the rules decide the stage and the mode says so.
  `output_by_sentence` answers differently for a sentence that contains the given text.
- `replay/citations.mjs` reads a composed answer by the core's rules (`orchestrator/compose.rs` and
  `without_invisible` in mercury-judge, kept in step with them): the sentences, with a stop before a
  closing quote, bracket or bold mark ending one; the citations, nested, listed with commas, semicolons
  or spaces, or empty; and the text the output checks read and the answer delivers. The replay server
  matches the output stage with it, and `run.mjs` works out the answer a recording delivers.
- `replay/recordings/repository-texts.json` answers the data stage for every text a probe returns:
  its own entry, or the default. Every text of the fixture gets the default.
- Fact ids come from the fixture's probe, so a recording's citations hold as long as the fixture
  does; `just it` names the recording that stopped matching.
- The judge's input and tool numbers in `questions/*.json` are a real chat model's answers to the core's
  requests (`node tests/it/replay/record.mjs`, the key from `deploy/compose/secrets/openrouter_api_key`).
  A reading replaces a recorded one only where it keeps the recording's story (the same class, every
  risk check on the same side of its threshold, a tool stage that still passes the chat judge); the
  others stay as written. The output numbers, the planner and the composer are scripted: the sentences
  are written so that each output check has something to refuse, and a real composer would not repeat
  itself. The real models end to end are `just golden --live` and `just redteam --live`.
- The golden questions have no recordings, so `just golden` without `--live` reads the rules' path end
  to end.

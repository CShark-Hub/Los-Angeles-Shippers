# Probe spec v0.1

The contract between a Mercury core and any probe: what a probe for another git host has to
implement. Schema id: `mercury.facts/0.1`.

A probe reads one repository and answers a fixed set of tools with **facts**: statuses, numbers,
dates, module names and short checked lines (titles and, under `code_model`, a code reader's
findings). It never returns code, file paths, diffs, commit messages, pull request or issue bodies,
or secrets, and its reply types leave no room for them.

## Transport and authentication

- MCP over **Streamable HTTP** at `/mcp`. Tools return **structured content** that matches the
  tool's `outputSchema`; the text content carries the same JSON. The probe in this repository answers
  statelessly with `application/json` and accepts any `Host` (it sits on an internal network, and the
  token guards it).
- The caller authenticates with `Authorization: Bearer <probe token>`: one token per probe, shared
  only with its core. A request to `/mcp` without it, or with another token, gets `401` before MCP
  sees it. A probe with no token configured answers `401` to every `/mcp` request: it never serves
  unauthenticated. This repository's probe reads the token from the file `PROBE_TOKEN_FILE` names,
  at start.
- `GET /health` answers `{"status":"ok","version":"<x.y.z>"}` without a token, for the container's
  health check; it says nothing about the repository.
- **Errors.** Arguments that don't parse (an unknown field, a `limit` outside 1–50, a malformed
  date), a well-formed but unknown `module` and an unknown tool are a JSON-RPC `invalid_params`
  error; an index that isn't built yet, or a reply over the cap, is `internal_error`. Error messages
  are fixed sentences that never repeat an argument.
- **Limits.** A reply is at most 256 KB of JSON; a larger one is refused, not cut. `limit` is 1–50,
  and 50 when it is left out.

## Tools

| Tool | Arguments | Returns |
|---|---|---|
| `describe` | — | `schema`, `project`, `display_name`, `access`, `access_limited_by`, `modules[]`, `tools[]`, `snapshot {commit_short?, indexed_at, meta_at}` |
| `progress` | `since?: date`, `module?: Module`, `limit?: 1..50` | `as_of`, `items: Fact[]` |
| `tested` | `module?: Module` | `as_of`, `modules: [{module, tests_total?, tests_changed_14d?, last_ci {status, at}, pass_rate_7d, flaky_7d}]` |
| `blocked` | `since?: date` | `as_of`, `items: Fact[]` (status `blocked`, with `waiting_on`) |
| `health` | — | `as_of`, `build {status, at}`, `ci_pass_rate_7d`, `open_prs {count, oldest_days}`, `todo_fixme? {count, trend_14d}`, `last_release {name?, at?}`, `score 0..100` |
| `changes` | `since: date` (its midnight UTC no earlier than 90 days before `as_of`), `module?: Module` | `as_of`, `modules: [{module, commits, merged_prs, notable: SafeText[]}]` |
| `ask_code` | `question: RedactedQuestion` | `as_of`, `findings: [{text: SafeText, module?, confidence 0..100}]` (at most 12), `withheld`, `cost_usd`, `prompt_version`; served only in `code_model` |

`Module` is one of the names `describe` lists; an unknown value is rejected, never guessed. A field
marked `?` is `null` when the probe can't know it; in `metadata` mode that is `tests_total`,
`tests_changed_14d`, `todo_fixme` and `snapshot.commit_short`.

`changes` counts from midnight UTC of `since` up to `as_of`: every module (`Other` included), with
zeros where nothing changed, or the one `module` asked for. `commits` are the default branch's
commits that touched the module (a commit touching two modules counts in both; in `metadata` mode,
the commits of the module's merged pull requests), `merged_prs` the pull requests merged into it,
and `notable` the titles of up to five of them, the most recent first, leaving out a withheld title.
A `since` whose midnight UTC is more than 90 days before `as_of` is `invalid_params`, compared to
the instant: the date 90 days back is refused unless `as_of` is itself at midnight. The probe keeps
90 days, and an answer that counted only part of the period, or part of its first day, would be
wrong. The core never asks for one: it moves an older start up to 89 days before today.

`ask_code` is served only in `code_model`: the code reader reads the snapshot for the one question
and answers with findings, at most 12, each a SafeText line with its `module` (one of `describe`'s,
or `null`) and its `confidence` (an integer, 0..100). A finding whose text fails SafeText is
dropped and counted in `withheld`. `as_of` is the snapshot the findings were read from, `cost_usd`
what the reader's call cost in US dollars (≥ 0), and `prompt_version` the version of the reader's
prompt.

Every reply, not only `describe`'s, carries `schema: "mercury.facts/0.1"`. A reply with another
schema id, a missing field or an extra field is rejected.

## Access modes

How much of the repository the probe reads.

| `access` | The probe reads |
|---|---|
| `metadata` | pull requests, issues, CI runs and releases; no file contents and no diffs. `tested` comes from CI alone and is never `missing` |
| `code` | the above, plus a snapshot of the default branch, analysed by rules |
| `code_model` | the above, plus a code reader that reads the snapshot for one question and answers with findings (`ask_code`) |

`access` is the effective mode: the strictest of the git host token's permissions, the manifest's
`access:` and the core's `access_max:` for the project. `access_limited_by` says what lowered it:
`null`, or `token` · `manifest` · `access_max` · `size_cap` (the snapshot is above the probe's size
cap) · `no_code_reader` (`code_model` was asked for, but no code reader is configured).

## Fact

```json
{
  "id": "f_7Qx2",
  "kind": "feature",
  "title": "Invoice list with filters",
  "module": "Invoicing",
  "status": "done",
  "tested": "passing",
  "confidence": 0.93,
  "waiting_on": null,
  "people": [],
  "evidence": [
    { "type": "pr", "ref": "#412", "state": "merged", "at": "2026-10-04" },
    { "type": "ci", "ref": null, "state": "passed", "at": "2026-10-04" }
  ],
  "title_withheld": false,
  "updated_at": "2026-10-04T15:12:00Z"
}
```

| Field | Values |
|---|---|
| `kind` | `feature` · `fix` · `chore` · `unknown` |
| `title` | SafeText |
| `module` | a module name from the manifest, or `Other` |
| `status` | `done` · `in_review` · `in_progress` · `blocked` · `not_started` · `unclear` |
| `tested` | `passing` · `failing` · `missing` · `unknown` (`missing` only where the probe counts tests, never in `metadata`) |
| `confidence` | 0..1 |
| `waiting_on` | `null`, or `review` · `dependency` · `environment` · `external` · `decision` · `unknown` |
| `people` | `DisplayName[]`, only when the effective policy has `show_people: true`; otherwise empty |
| `evidence[].type` | `pr` · `issue` · `ci` · `release` |
| `evidence[].state` | pr: `open` · `draft` · `merged` · `closed`; issue: `open` · `closed`; ci: `passed` · `failed` · `running`; release: `published` |
| `evidence[].ref` | `#<number>` for pull requests and issues when `show_evidence_refs: true`, otherwise `null`; always `null` for CI (always the default branch) and releases. Branch names are never returned: they often hold module or ticket names |
| `title_withheld` | `true` when the title failed a rule or a check and was replaced (see SafeText) |

Other enums in tool replies: `build.status` and `last_ci.status` are `passed` · `failed` ·
`running` · `none`. `last_release.name` is SafeText.

## Value formats

Apart from SafeText and DisplayName, every text value in a reply has a fixed shape, so none of them
can carry free text.

| Field | Format |
|---|---|
| a fact's `id` | `f_` and 4–16 letters or digits, e.g. `f_7Qx2` |
| `project` | a lowercase letter, then up to 39 lowercase letters, digits or hyphens, e.g. `delta` |
| `display_name` of `describe` | SafeText |
| `module`, `modules[]` | 1–40 characters: a letter first, then letters, digits, spaces, `&`, `+` or `-`; `Other` for work outside every module |
| `evidence[].ref` | `#` and a positive number, e.g. `#412` |
| `snapshot.commit_short` | 7–12 lowercase hexadecimal digits |
| `evidence[].at` | a date (`2026-10-04`); every other time is an RFC 3339 timestamp in UTC |

One format is an argument only, never in a reply: **`RedactedQuestion`**, `ask_code`'s `question`.
It is the asker's question after the redaction rules (the core's global rules and the manifest's
`redact`), on one line, 1–1,000 characters, with no control or invisible format characters; a
longer question is refused, never cut, and a `question` of another shape is `invalid_params`.

| Number | Values |
|---|---|
| a fact's `confidence`, `pass_rate_7d`, `flaky_7d`, `ci_pass_rate_7d` | 0..1 (`flaky_7d` is the share of flaky CI runs in the last 7 days) |
| `score`, a finding's `confidence` | an integer, 0..100 |
| `todo_fixme.trend_14d` | the change in the count over the last 14 days, negative when it fell |
| counts (`tests_total`, `commits`, `open_prs.count`, …) | whole numbers ≥ 0 |

`build.at` and `last_ci.at` are `null` when the status is `none`.

## SafeText

- One line, at most 120 characters, after redaction (the core's global rules and the manifest's
  `redact`).
- It must not contain:
  - file paths, including a word that ends in a slash (`Invoices/ folder`) and look-alike slashes
    (`∕`, `⁄`, `／`, `⧸`, `＼`);
  - code, identifiers (`CamelCase` class names, `snake_case` functions) or backticks;
  - URLs, IP addresses (`10.0.12.5`) or a host with a port (`localhost:8080`);
  - e-mail addresses or secrets;
  - control or invisible characters: every Unicode format character (category Cf, such as the tag
    characters), the variation selectors and the Hangul fillers.
- A title that fails any rule or check is replaced with `Work item in <module>`, and the fact gets
  `title_withheld: true`.

## DisplayName

A person's display name from the git host profile, redacted like SafeText, at most 60 characters;
never an e-mail address or a login.

- **Characters:** letters (Unicode category L, so no combining marks), hyphens and apostrophes
  (`'` or `’`).
- **Words:** separated by single spaces, with no leading or trailing space.
- **At least one letter** somewhere in the name.

The schema's `pattern` says exactly this.

## Registration

1. At start and every 60 seconds the probe calls `POST <core>/internal/probes/register` with its
   probe token (`Authorization: Bearer`) and `{project, schema, access, access_limited_by,
   manifest_hash, manifest, tools}`. `manifest_hash` is the SHA-256, in lowercase hexadecimal, of
   the manifest file's bytes as read; `manifest` is the parsed manifest; `tools` are the tools it
   serves. The core stores `access` and quarantines a probe that reports more than the project's
   `access_max`, or whose manifest has a redaction pattern that doesn't compile. This repository's
   probe takes `<core>` from `PROBE_CORE_INTERNAL_URL` (e.g. `http://core:8081`) and doesn't register
   when it is unset. A refused or failed registration is logged by its status only, never with the
   body, and the probe keeps serving and tries again at the next tick.
2. The core then runs MCP `initialize` and `tools/list`. The tool set must be one of three: the
   six (every tool above but `ask_code`); the five without `changes`; or the seven (the six and
   `ask_code`), only for a probe registered with `access: code_model`. Otherwise the probe is
   **quarantined** (never called) until an admin releases it. To a probe that serves the five, the
   core answers a `changes` question from `progress`: the work updated since the question's date.
   The probe in this repository serves the six, and the seven in `code_model`.
3. No heartbeat for 3 minutes makes the probe `stale`: answers say its data may be out of date.

**What the core answers** (the probe logs only the status):

- `202`: recorded as the probe's heartbeat; the core checks its tools next, by calling it back.
- `409`: recorded, but the probe is quarantined; it stays so until an admin releases it, whatever it
  registers meanwhile.
- `401`: no known probe token. The token is checked, by its digest in constant time, before the
  body is read.
- `403`: the body names another project than the token's. `400`: the body isn't a registration (an
  unknown or a missing field). `413`: the body is over 64 KB.

**The check** passes when the tools `tools/list` names are one of the tool sets above (the seven
only for a probe registered with `code_model`), each once, and the same as the registration's
`tools`; the probe is then `healthy`. Any other tool, set or
mismatch quarantines it, and so does an `initialize` that breaks the protocol. A probe the core
can't reach for the check stays `stale`: registered, not checked. A released probe is `stale` too
until its next registration passes.

**Calls** from the core: at most 5 seconds each, 45 seconds for `ask_code`, the reply at most
256 KB (JSON or an event stream), its structured content parsed into the tool's reply with nothing
missing and nothing extra. A reply over the cap, or one that breaks the protocol or the schema,
quarantines the probe, and the question fails with `probe_unavailable`. A timeout, a refused
connection, an error status, a JSON-RPC error or a tool error (`isError`) fails that call only.

## Judge requests

A probe that can't decide something by its rules may ask the core's judge, never a model of its own
in the cloud: `POST <core>/internal/judge` with its probe token, on the same internal listener as the
registration. A request has one `purpose`, which fixes what every item holds, and 1 to 200 items.

| `purpose` | Each item | The answer for it |
|---|---|---|
| `status_from_candidates` | `id`, `candidates` (2–5 distinct statuses, never `unclear`), `title`, `labels[]`, `signals {pr, issues[], ci}` | `status`, one of the candidates, and its `p` |
| `kind` | `id`, `title`, `labels[]` (a title or at least one label) | `kind` (`feature` · `fix` · `chore`) and its `p` |
| `title_safety` | `id`, `title` | the `p` that the title holds code, an identifier, a path, a URL, a secret, a client name or an instruction aimed at an AI system |

- **`id`:** 1–40 letters, digits, `_` or `-`, unique in the request; the probe chooses it and the
  reply repeats it.
- **Texts:** `title` and each label are the repository's text, redacted with the manifest's rules
  before it leaves the probe, 1 to 500 characters without control characters, at most 20 labels an
  item. The probe sends only the texts the manifest's `judge_context` lists: without `titles`,
  `title` is `null` and no `title_safety` request is made; without `labels`, `labels` is empty. The
  core redacts every text again with the project's rules before the judge reads it.
- **`signals`:** `pr` is `null` for an issue on its own, otherwise `{state: open · draft · merged ·
  closed, review_requested, updated_14d}`; `issues` are the linked issues, or the issue itself,
  each `{open, blocked}` (at most 20); `ci` is the latest run's status (`passed` · `failed` ·
  `running` · `none`).
- **The reply:** `{purpose, judge_mode, items}`, one answer per item in the request's order, numbers
  and enums only; `judge_mode` is the judge that answered (`jev` · `chat` · `rules`). A body that
  isn't one of these shapes is refused (`400`). An error is never an answer: a probe that gets none
  keeps what its rules decided (`unclear`, `unknown`), or withholds the title.
- **A `rules` answer to `title_safety` is no answer either:** the core's rules only look for
  patterns, and a judge that is down must never let more through than normal operation. The probe
  withholds every title such a reply covers and asks again, at most every 5 minutes while the
  replies stay `rules`; a rule's hit withholds as any answer above the line does. A `rules` answer
  to a status or a kind gives every candidate the same probability, which settles nothing. A
  project whose server-side manifest copy leaves `titles` out of `judge_context` while the probe's
  own manifest keeps them gets every title withheld this way; to keep titles on the rules alone,
  leave `titles` out of the probe's manifest too.
- **No core, no `serve`:** a `serve` probe needs the core's internal address
  (`PROBE_CORE_INTERNAL_URL`; a blank value counts as not set) and doesn't start without it; only
  `dev` runs without a core, with titles on the rules alone.

## Requirements

**MUST**

- Return only schema-conformant structured output: no extra fields, no extra tools.
- Never return source code, file paths, diffs, raw commit messages, pull request or issue bodies, or
  secrets.
- Redact every text field before it leaves the process.
- Authenticate every call; reject calls without the probe token.
- Report `as_of` truthfully.
- Report `access` truthfully. In `metadata` mode, read no diffs and no file contents except the
  manifest.

**SHOULD**

- Split fetching (`sync`: credentials and network) from answering (`serve`: neither, and no route
  out). In `code_model`, run the code reader as a process of its own whose only route out is its
  preset's egress allowlist.
- Keep repository code only in memory, never on a persistent disk.
- Keep the data fresher than 10 minutes.
- Answer any tool in under 300 ms from a local index.

**MAY**

- Leave out `people` and evidence references entirely.
- Use the core's judge for ambiguous decisions; never a cloud model directly.
- In `code_model`, answer `ask_code` from a code reader that `serve` asks: a process of its own
  whose model is reached only through its preset's egress allowlist, while `serve` keeps no route
  out; the findings leave only as SafeText.

## Checking a probe

The probe in this repository is held to this contract by its own tests: the schema types in
`crates/mercury-schema`, and a leak oracle in `crates/mercury-probe/tests` over the synthetic
fixture repository (`tests/fixtures/repo-delta`). Any probe can be held to it from outside, over
the wire, with the conformance runner:

```
mercury-probe-conformance --url <probe>/mcp --token <t> --fixture tests/fixtures/repo-delta
```

Run the probe on the fixture first (`mercury-probe dev` does, with `--repo` and `--meta` pointing at
it), and give the runner the same folder. `--token-file <file>` reads the token from a file, so it
stays off the process list; `--help` names every flag. The runner prints one line per check, `ok` or
`FAIL` with the reason, and ends with a verdict. It checks the mode the probe serves: run it once
for each access mode the probe supports (the fixture's `meta/` is the same in both).

| Check | What it asks |
|---|---|
| `health` | `GET /health` answers 200 with a status and a version, without a token |
| `auth` | a request with no token, a wrong one, an empty one or another scheme gets `401`; the right token is served |
| `initialize` | `initialize` answers with the tools capability, at a protocol revision up to `2025-11-25` (a client that proposes a later one is answered with `2025-11-25`) |
| `tools` | the tool list is exactly `describe`, `progress`, `tested`, `blocked`, `health` and `changes`, and `ask_code` as a seventh when `describe` says `code_model`, each once, each with an input and an output schema; `describe`'s own `tools` and `modules` must name the same six or seven tools and the fixture's modules, and the other checks call the fixture's modules whatever `describe` lists |
| `schemas` | about a hundred calls over every tool, module, date and `limit`: each reply is structured content that parses into its tool's reply type with nothing missing, nothing extra and no free text, carries `mercury.facts/0.1`, and has the same JSON in its text content |
| `invalid_params` | arguments that don't parse (an unknown field, a `limit` outside 1–50, a malformed date), a well-formed but unknown module and an unknown tool are `invalid_params`, and the error never repeats an argument |
| `limits` | `limit` 1 gives one item, 50 at most fifty, no `limit` at most fifty; no reply is over 256 KB; a request body of 1 MiB (a valid `describe` padded with JSON whitespace, so only a body cap can refuse it) is refused, not served |
| `leak_oracle` | none of the fixture's paths, file names, code lines or distinctive identifiers (CamelCase, snake_case and SCREAMING_SNAKE of 8 or more characters) is in any reply, nor in the tool list |

The exit code is `0` when every check passed (the probe is v0.1-compatible), `1` when a check
failed and `2` when the command line is wrong; that message names the flag, never its value. A probe that can't be reached fails `health` and
nothing else runs. The runner checks what the spec lets a caller see over the wire; what it
cannot (that `serve` has no network, that a snapshot never reaches a disk) is the deployment's to
show.

# Changelog

Every change that matters to someone running or using Mercury. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once the first one is tagged.

## [Unreleased]

The first version: everything below is new.

### Asking

- Questions in plain language about a project's progress, tests, blockers, health and changes, and
  any other question about the project that isn't off limits, answered from facts with their
  confidence, freshness and evidence; an estimate is always worded as one, never a promise.
  Questions about code or people's performance, asking for a commitment to a client or for
  confidential information are refused, with what to ask instead; a question not about the project
  is out of scope.
- Everything Mercury writes is in English: the answer, a refusal and its hint, a clarifying
  question and the notes it adds to an answer.
- A web client: sign-in with a personal token exchanged for a session cookie the page can't read,
  Ask with a live stage line ("Reading the code" while the code reader works), a short view of the
  checks under each answer for a project manager and the full trail for a tech lead or an admin;
  conversations in the tab's side panel, Approvals, Settings with two looks, and the audit for
  admins.
- The `mercury` CLI: `login` and `logout` (the token in the OS keychain), `me`, `projects`, `ask`
  with the timeline on standard error and `--json` for scripts, `--wait` for a question held for
  sign-off, `approvals`, `audit`; exit codes that say how a question ended.
- MCP at `/mcp` for AI agents such as Claude Code, with the person's own token: `list_projects` and
  `ask`, through the same checks, limits and audit.
- Sign-off: answers of the classes a role's policy names, or of a doubtful review, wait for an
  approver of the project, who approves them as they are or rejects them with a note.

### The gateway

- A judge model checks every question, every tool call and every sentence of an answer before it is
  shown, with a chat-model fallback and a rules-only mode when no model answers; a judge error never
  passes.
- Tools from a fixed table per question class; the planner fills validated arguments only.
- An input check refuses a question that asks for confidential information (the refusal
  `confidential`); the output check on commitments to clients lets an internal estimate worded as
  one pass.
- Open questions use every fact tool of the probe, plus the code reader's when the project has one;
  `tool_result` and `judged` carry the stage's lowest probability; a question's cost includes the
  reader's, and the `demo` preset caps a question at 0.15 US dollars.
- Redaction rules, a deny list and roles in `policies.yaml`; per-token rate limits; daily and
  per-question budgets for the models.
- An append-only audit with a hash chain: `admin audit verify` and `audit export`, retention that
  empties old texts and keeps the chain.

### The probe

- `mercury-probe` next to the repository: `sync` reads a GitHub repository through its API into
  memory, `serve` answers the core's tool calls over MCP with facts only (enums, numbers, dates and
  checked text, never code), `dev` runs on a local folder. The repository's `.mercury/probe.yaml`
  maps files and labels to modules and says what may be asked.
- A conformance runner, `mercury-probe-conformance`, for anyone writing a probe of their own.
- An optional code reader (`mercury-probe reader`, the `code_model` mode) and a seventh tool,
  `ask_code`, served only in that mode: the code after `exclude`, under a size cap, and the redacted
  question go to one model in one request; at most 12 findings come back, through the same text
  checks as titles and a check that they repeat none of the code; its own container, key file, daily
  cap and egress allowlist of one host; no disk.

### Running it

- Docker Compose files for a laptop and for a server with TLS: Caddy is the only service that
  publishes a port, a `serve` probe has no route out, a `sync` probe reaches only the git host
  through an egress proxy.
- `just compose-up` with a bundled fixture repository and no model key, and on a database with
  nobody yet one demo person of each role, their tokens in `deploy/compose/secrets/demo-tokens`;
  presets for a demo, a private and a strict setup (`config-examples/presets/`).
- `code-reader` and `reader-egress` in Compose when a reader is configured; `check.sh` checks that
  `serve` still has no route out and the reader reaches only its host.
- `admin` commands for people, tokens, access, probes, the configuration check, the daily status
  and backups made while the core serves; scripts for a daily smoke test and a nightly backup.

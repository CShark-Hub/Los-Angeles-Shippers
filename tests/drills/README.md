# Degraded-mode drills

Each drill breaks one thing on a stack of its own, checks that Mercury degrades the way
`docs/deploy.md` says (never letting more through than normal operation), puts it back, and checks
the audit chain still holds. They are run by hand, before a release or after a change to what they
cover, not by `just ci`.

Every drill is a Node.js script on the integration runs' stack (`tests/it/stack.mjs`: the fixture,
the it overlay, a fresh database, the people of the runs), under its own Compose project, port and
subnets, so the drills can run side by side and never touch the project `mercury` (the local stack).
A drill's temporary Compose override lives next to it here, never in `deploy/compose/`. A drill
leaves the stack down and its volumes removed, its own project's only.

## Running one

From the repository root, in Git Bash, after the images are built once (`just _images`) and with a
CLI to ask with:

```bash
export MSYS_NO_PATHCONV=1
export MERCURY_CLI=target/debug/mercury
node tests/drills/<drill>.mjs            # --keep leaves the stack up to look at
```

Each script prints what it did, what it saw and `ok` or `FAIL` per check, and exits non-zero on a
failure. A drill that needs the real models says so and takes `--live` (the key in
`deploy/compose/secrets/openrouter_api_key`); it costs a few cents.

## The drills

- **`decisions-endpoint-down.mjs`** — the judge model's endpoint cut off (project
  `mercury-drill-decisions`, port 18101). It points only the Decisions address in the run's
  `models.yaml` at a port that refuses connections and restarts just the core; the chat endpoint,
  the planner and the composer keep working. Expected: a baseline judged by `jev`; then every stage
  judged by `chat` on thresholds 0.2 stricter (checked verdict by verdict), the answer still judged
  before it is shown and a code question still refused, `judge_mode: chat` in the `done` row, on
  the CLI's `done` line and in `mercury-core admin status`, and a core log naming the failed
  connection, not a missing key. The circuit breaker is watched with the core's log: three failures
  of Jev, then none while it is open, and a minute after it opened Jev is asked again with no
  restart. The alert is watched on the clock: `admin status --check` says nothing until the first
  degraded question is 5 minutes old, then exits 1 naming the judge, `admin status` lists it under
  Alerts, and the core logs it once, not again the next minute (about 9 minutes in all). `jev`
  again once the address is back, and the alert gone. `--live` asks five questions (a few cents; the
  run's daily caps are lowered to $0.10); `--dry-run` runs the same steps on the replay server for
  nothing and skips the checks that need the real judge (its degraded run starts at the baseline).
- **`probe-down.mjs`** — a probe stopped (project `mercury-drill-probe`, port 18102, two projects:
  the fixture's delta and poison, with its code reader; replay models, about 20 minutes). With
  delta's probe stopped, its questions fail with `probe_unavailable`, no fact read and nothing
  delivered, while poison answers as before; `probe list` and `admin status` show delta `stale` 2
  to 3 minutes after the stop (the probe registers every minute; stale is 3 minutes without one).
  Started again, delta is healthy within a second and gives the baseline's answer and facts.
- **`disk-full.mjs`** — the core's disk full (project `mercury-drill-disk`, port 18103; replay
  models, about 2 minutes). `disk-full.override.yml` puts the core's `/data` on a 16 MB tmpfs volume,
  held by a second container so the database outlives a restart of the core. Filled: the question
  fails with `internal`, nothing is written or delivered, `/v1/health` stays `ok`, `audit verify`
  holds, and `admin status` says the disk is full and shows nothing. Freed: the chain still
  verifies, `admin status` shows again, and questions are answered again without a restart (and
  after one). Then a question that runs out of space part-way: nothing delivered, it ends with
  `internal`, and it reads `failed` once the space is back and after a restart.

- **`rotations.mjs`** — each secret rotated as `docs/deploy.md` says (project
  `mercury-drill-rotate`, port 18104; replay models, about a minute). The models' key (the run's
  `replay_key`, read like the provider's key file) replaced and the core restarted; a probe's token
  replaced and the core and that probe restarted; a person's token replaced by a new one and the old
  one revoked. After each restart the core answers, delta is registered and gives the baseline's
  answer, judged and written by the models, within a minute (the core reads its secrets at start, so
  a rotation is a restart, and a minute is what it may take).
  The git host's token is not rotated here: only a stack that syncs has one, and rotating it is up
  to whoever runs that stack (restart its `sync` after replacing the file, as `docs/deploy.md` says).
- **`suspected-leak.mjs`** — the steps to take when an answer is suspected of a leak (project
  `mercury-drill-leak`, port 18105; replay models, about two minutes): revoke the token that asked
  (`token revoke`), quarantine the project's probe (`probe quarantine`), export the audit rows of the
  question and of the token (`audit export --question`, `--token`), verify the export, and release
  the probe (`probe release`). Each step is checked: the token refused with `401` at once; the probe
  quarantined (`probe list` says so, and another person's question delivers nothing and never
  reaches the probe); the export verified in a scratch folder with nothing but its files (each row's
  hash recomputed from its fields, consecutive rows linked); the probe released and the project
  answering as before. It prints two `KNOWN` lines when it meets them: an exported `at` on a whole
  second lacks the milliseconds the hash covers, and a payload the export wrote again can't be
  re-digested from the file.
- **`reader-upstream-down.mjs`** — the code reader's model out of reach (project
  `mercury-drill-reader`, port 18106; the red team's repository with its reader, `code_model`;
  replay models, about two minutes). An open question on poison is answered from the code first:
  `ask_code` in the plan with its result, the findings' sentence and the figure's. On this stack the
  reader's upstream is the replay server, a host inside your network that the reader calls
  directly on the probes network, never through `reader-egress`, so stopping that proxy alone would
  change nothing; the drill stops it and also disconnects the replay container from the stack's
  probes network (the core still reaches it over `egress`, and no probe calls it), then puts both
  back. With the upstream gone, then refusing (a recording whose reader answers 503), the same kind of
  question is still delivered: no result for the reader's call, the figure's sentence kept, the
  findings' sentence stripped for citing what was never read, and "The code couldn't be read this
  time…"; the probe stays `healthy` with `code_model` and is not quarantined, the reader's own health
  check answers, and delta's answer doesn't move. The first read after the cut waits about 30 s on
  the connection the reader kept open to the vanished address (under the probe's 40 s and the core's
  45 s); the next fails at once, as does the refusal. Back, the reader reads again within a minute.
  Neither a question nor a planted text appears in the logs of `code-reader` and `reader-egress`.
  `--live` runs it on the real models (project `mercury-drill-reader-live`, port 18107; Jev, the
  chat models and the reader on OpenRouter; four or five questions, under 10 cents). There the
  reader's upstream is outside your network and reached only through `reader-egress`, so stopping that
  proxy is the real cut: the reader finds no proxy and fails at once. The second way down is the
  reader's own daily cap: only `code-reader` is recreated with `MERCURY_IT_READER_DAILY_USD=0.00`
  (the it overlay's `READER_DAILY_USD`), and it refuses with `daily_cap` before anything reaches
  `reader-egress`; recreated with the normal cap, it reads again. The live answers are the models'
  own, so the live run checks what holds whatever they say (the reader's result there or not, the
  note when the answer is delivered, the probe's state, the audit, the logs) and prints the wording;
  an ending other than delivered, or a judge other than Jev, is printed as a `NOTE`.

`audit verify` holds after every phase of every drill. Cutting off OpenRouter as a whole has no
script: it is done by hand, with a temporary `extra_hosts` override on a local stack.

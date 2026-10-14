# Load and cost

`just bench` asks questions from several askers at once and reports how long the answers took, what
they cost, how they ended and which judge answered them, with the probes' GitHub requests per hour
and snapshot RAM. It is a measurement, not a gate: it compares its figures with the targets and
says so, but it fails only when it can't trust them (below).

## Running it

```bash
just bench                                   # the replay stack: a fresh stack of its own, $0
just bench --live                            # the same stack with the real models, a few cents
node tests/bench/run.mjs --url http://localhost:8080 --token mcy_… --token mcy_… --project delta
```

- **Replay (the default).** A fresh Compose stack of its own (project `mercury-bench`, Caddy on
  `127.0.0.1:18121`, its own subnets), with the fixture repository and its probe in dev mode, and the
  replay server (`tests/it/replay`) as the models. The questions are the recordings that answer a
  question all the way: the input judge, the planner and the composer recorded, the red team's
  project left out, and the one recording that overspends on purpose left out (it would end
  `budget_exceeded` and spend the asker's day). It costs nothing. The stack goes down with its volumes
  at the end, unless `--keep`.
- **`--live`.** The same stack with the models of `config-examples/models.yaml` (Jev and the chat
  models on OpenRouter), the key in `deploy/compose/secrets/openrouter_api_key`. The questions are the
  golden set's allowed ones for delta (`expect.refuse: false`, labelled for a project manager or a
  tech lead). About $0.004–0.008 a question, so the default 50 questions cost about $0.20–0.40.
- **`--url URL`.** An existing stack, such as the one on a laptop; nothing on it is started or changed.
  Its tokens come from `--token` (repeated, one per asker), or from `MERCURY_BENCH_TOKENS` (separated
  by commas or spaces) to keep them out of the shell's history, or from `MERCURY_TOKEN`. `--project`
  names the project (`delta` by default); the golden set's allowed questions for it are asked, or
  delta's when the set has none for it, or those of `--questions FILE` (JSON lines like
  `tests/golden/questions.jsonl`, of which the allowed ones are asked). Its cost is the stack's
  models': with the real ones, as `--live`. `--compose-project NAME` lets the run read that project's
  probes (`docker compose -p NAME` from `deploy/compose`, whose `.env` names the files unless
  `--compose-file` does); without it the probes are reported as not measured. `just bench` builds the
  images first; against an existing stack `node tests/bench/run.mjs` skips that.

Every mode: `--askers N` (5), `--per N` (10 questions each), `--seed N` (13: the order the questions
are dealt in, the same on every run), `--label NAME` (the report's name in place of the mode).
`MERCURY_CLI` names a CLI built elsewhere, as for `just it`.

## Tokens and the rate limit

On its own stack every asker is a project manager with a token of its own (the runs' `pm` and
`spender`, then `asker3@example.com` on), and the runs' limits are raised to 3,000 questions an hour
per token, so the limit never holds an asker back. Against an existing stack the stack's own limit
applies (300 an hour with a burst of 50 in `config-examples/policies.yaml`, 30 and 5 in the
private and strict presets): give each asker its own
token. A question the limit holds back (`429 rate_limited`, before any question starts) waits as the
server says and is asked again; the report counts those waits, and they are in no time it measures.

## What it reports

`tests/bench/reports/<date>-<mode>.md` (`replay`, `live`, `url` or the `--label`), and the same
summary on standard output. Each figure comes from the question's state (`GET /v1/questions/{id}`,
read with the asker's token), which the run also checks against what `mercury ask --json` said.

- **Server latency:** p50, p95 and the highest `duration_ms` of the `done` row: the time from the
  question's start to its end, inside the core. Only a question that reaches `done` has one.
- **Client latency:** the wall time of `mercury ask --json`, every question that started and the
  delivered ones apart: the server's time plus the CLI's start, the request and the event stream.
- **Cost:** the mean, the highest and the total `cost_usd` (every ending carries one, a refusal
  too), and the mean of the delivered questions, against the cap per question (`per_question_usd`)
  and a mean of $0.02.
- **Endings:** how many questions were delivered, refused (by code), asked back, held for sign-off,
  failed (by code) or never started (by why). None is left out.
- **Judge modes and the degraded share:** each ending by the `judge_mode` it names (`none` when no
  judge answered), and `chat` or `rules` over all of them, the way `mctl status` counts it. On its own
  stack the run reads `admin status` afterwards and fails when the two disagree; on an existing stack
  the 24-hour window of `mctl status` holds other questions too, so read it beside the report. On the
  replay stack the judge is a chat model, so the share is 100% by construction.
- **Probes:** GitHub API requests per hour (against the 5,000 an hour a token may make) and each
  probe's snapshot RAM, from `tests/bench/probes.mjs`, which reads the probes' logs and `docker stats`.
- **Each question:** its asker, its id in the set, its ending, its judge, its times and its cost.

The targets the figures are compared with: p50 ≤ 8 s and p95 ≤ 15 s with five concurrent askers, and
a mean cost of at most $0.02 a question. The report holds no token and no answer's text; an address
given with `--url` is written down only when it is local.

It exits 1 when a token shows in an output, when a question's state can't be read, when the CLI's
JSON and the state disagree, when nothing started, or when its degraded share and `mctl status`'s
disagree; a figure over its target is reported, not failed.

# Demo: the bundled repositories

The way on from Step 2 of [SKILL.md](SKILL.md), under its rules (one question at a time, secrets
never in the chat, nothing of theirs overwritten without asking). Everything runs on this machine,
with repositories bundled in `tests/`: no GitHub, no git host token. Number these steps on from
Step 2, as one sequence with the rest.

## D1: Which demo

Ask:

- **The whole stack** `(Recommended)`: the core, the probes and the web client in Docker on
  http://localhost:8080. Real questions go through every check, on the bundled repositories. → D2.
- **The web client alone**: the screens on http://localhost:5173 with recorded answers. No Docker,
  no core, no checks, no people to create: only to see what the pages look like. → D7.

## D2: What the stack shows

Ask both questions together, before anything starts, so the stack starts once:

**Which projects:**

- **`delta` only** `(Recommended)`: a synthetic invoicing repository (`tests/fixtures/repo-delta`)
  with its pull requests, issues and CI runs, in three modules: Invoicing, Export and Auth.
- **`delta` and `poison`**: also "Ember — reports", the red team's repository
  (`tests/redteam/repo-poisoned`), whose README and titles carry hidden instructions for AI agents,
  to see Mercury withhold them. It runs with the `demo` preset as it is, where a project manager's
  health answers wait for an approver.

**Which models:**

- **No key** `(Recommended)` to start: the rules judge every step and a template writes the answer.
  It costs nothing; the repositories' titles are withheld, since no judge clears them.
- **An OpenRouter key**: real models, Jev as the judge, a few cents a question within the budgets
  of `config/models.yaml` ($0.15 a question, $1 a person and $2 in all a day). The person sets a
  credit limit on the key at OpenRouter.

## D3: Prepare

1. **`deploy/compose/config/`.** Both ways below copy settings into it only when it is empty. If it
   isn't (an earlier demo, or a configuration of theirs), check it holds what D2 chose: `delta` in
   `config/projects.yaml`, and `poison` too for the second choice. If it doesn't, ask: **move it
   aside** (to `deploy/compose/backups/config-<date>/`, which git ignores, so the demo starts from
   its own settings) or **stop**. Never delete it.
2. **The key**, for the second choice of models: the person runs
   `mkdir -p deploy/compose/secrets` and writes the key into
   `deploy/compose/secrets/openrouter_api_key` in their own terminal. Check it isn't empty. The
   stack reads it when it starts, so no restart is needed later.

## D4: Start

In the background; the first time it builds the images (Rust) and the web client, which takes a
while:

- **`delta` only:** `just compose-up`. It writes `config/` (`config-examples/`, the `demo` preset's
  settings without `poison` and without its sign-off on health) and `secrets/` (an empty key file
  unless they wrote one, the probe's random token), and waits for http://localhost:8080/v1/health.
  On a database that has nobody yet it then adds the demo's people, one of each role
  (`deploy/compose/scripts/demo-people.txt`), with their tokens in
  `deploy/compose/secrets/demo-tokens`.
- **`delta` and `poison`:** `COMPOSE_FILE=docker-compose.yml:docker-compose.fixture.yml just demo-local`.
  It writes `config/` from `config-examples/presets/demo/` and the probes' tokens, checks the
  configuration with the core itself, and waits until both probes are healthy. If
  `secrets/reader_api_key` holds a key, it also starts a code reader that sends `poison`'s code to
  OpenRouter: say so before it starts.

## D5: Check

- `docker exec mercury-core-1 mercury-core admin probe list`: `delta` (and `poison`) `healthy`.
- `docker exec mercury-core-1 mercury-core admin status`: say which judge runs (`rules` without a
  key) and today's spend.

**The people**, by the way it started:

- **`delta` only:** the demo's people are there; show them as a table (`pm@example.com` `pm`,
  `lead@example.com` `tech_lead` and approver, `board@example.com` `board`, `admin@example.com`
  `admin`; all on `delta`, the board also on `portfolio`). Their tokens are in
  `deploy/compose/secrets/demo-tokens`, a secret file: never read it yourself; the person opens it
  in their own terminal (`cat deploy/compose/secrets/demo-tokens`) and pastes the `pm`'s token to
  ask, the `admin`'s for the admin panel. If the line "the demo's people are in" didn't appear, the
  database had people already: say so, and go to Step 3 of SKILL.md for theirs. Otherwise Step 3
  only when they want more people; else go to Step 4.
- **`delta` and `poison`:** `just demo-local` adds nobody. Look at
  `docker exec mercury-core-1 mercury-core admin access list`:
  - the demo's people are there (an earlier `just compose-up` on this database): grant them
    `poison` (`user grant --email <e-mail> --projects delta,poison`; the lead's with `--approver`
    again, since a plain grant clears the flag) and tell the person their tokens are in
    `deploy/compose/secrets/demo-tokens`;
  - nobody is there: Step 3 of SKILL.md, granting `delta`, and `poison` to whoever should see it.

  A `pm`'s health questions wait for an approver here: make sure a second person, a `pm` or a
  `tech_lead`, approves both projects (`--approver`).

## D6: What to ask

For Step 4 of SKILL.md, the questions that show what Mercury does, after signing in:

| Question | What it shows |
|---|---|
| *what is blocked?* | an answer from the facts, with the checks of every stage under it |
| *what is done in Export?* | an answer narrowed to one module |
| *show me the export code* | a refusal: Mercury never shows code, and says what to ask instead |
| *who is the slowest developer?* | a refusal: people aren't evaluated |
| *what is in progress on Ember?* (with `poison`) | the planted instructions never reach the answer |

## D7: The web client alone

Run `just web-dev` in the background (it installs the packages first) and wait for
http://localhost:5173. Tell them: the sign-in page offers a project manager, a tech lead and an
admin to sign in as, the answers are recordings, and nothing is saved. Stop the server when they
are done. There are no people to add: go straight to Step 4 of SKILL.md, with only the address and
what they can look at (Ask, Approvals, the admin panel and the audit as the admin).

# Production: the person's own repositories

The way on from Step 2 of [SKILL.md](SKILL.md), under its rules (one question at a time, secrets
never in the chat, nothing of theirs overwritten without asking). When it ends, go back to Step 3
of SKILL.md for the people, with the projects added here. Number these steps on from Step 2, as one
sequence with the rest.

## P1: Where it runs

Ask:

- **This laptop** `(Recommended)` for a team trying it on real repositories: plain http on
  http://localhost:8080, other devices only over HTTPS (for instance `tailscale serve`).
- **A Linux server**: Caddy with its own TLS certificate for a domain, ports 80 and 443. You set it
  up only if you are running on that server now; otherwise give the person the checklist of P7 and
  `docs/deploy.md` ("A Linux server") and stop after it.

On a laptop, the probes hold the repositories' code in memory, which must never be swapped to the
disk: ask the person to set Docker Desktop's swap to 0 (`swap=0` in `.wslconfig` on WSL 2) before
P5. It is a system setting: they change it, you don't.

## P2: The preset

A preset is a ready set of the files in `deploy/compose/config/`. The core reads whatever is there;
the preset only decides where it starts. Ask, by what may leave their network:

- **`private`** `(Recommended)` for a company's repositories: the models on OpenRouter (zero data
  retention) see the redacted question and facts without titles: statuses, numbers, dates, module
  names. Each repository's manifest says `judge_context: []`.
- **`demo`**: the same models, also shown the titles of pull requests and issues (redacted). For
  repositories whose titles aren't confidential.
- **`strict`**: every model on their own server with the OpenAI chat API (Ollama, vLLM, …); nothing
  leaves their network. It has no Jev, so the judge is a chat model and every threshold is 0.2
  stricter: more refusals, more answers held for an approver.

Then:

1. If `deploy/compose/config/` isn't empty, ask before replacing it (it may be a demo's, or theirs).
   Copy: `cp -R config-examples/presets/<preset>/. deploy/compose/config/`.
2. In `config/projects.yaml`, take out the fixture project `delta`, unless they keep the fixture
   beside their repositories. On a laptop keep `poison` (the `demo` preset has it; for `private` and
   `strict` copy its block from `config-examples/presets/demo/projects.yaml`): `just demo-local`
   always runs its probe and stops without it.
3. The models:
   - `demo` or `private`: the person writes an OpenRouter key, with a credit limit set at
     OpenRouter, into `deploy/compose/secrets/openrouter_api_key`; check it isn't empty. Without
     one, the stack still runs on the rules and a template.
   - `strict`: ask for the server's chat URL, an address the core's container reaches (on Docker
     Desktop the host is `host.docker.internal`), and the model name for each role (`judge`,
     `planner`, `composer`); write them into `config/models.yaml` in place of `http://llm:8000/…`
     and `local/*`. A server that wants a key gets a `key_file` the person fills.

## P3: Each repository

Ask how many, then for each one, in this order:

1. **Its names.** The GitHub repository `owner/name`; the project id (lowercase letters, digits and
   dashes, e.g. `billing`); the display name (e.g. "Billing — invoicing"); the aliases people use
   for it.
2. **The access mode**, the most the probe may read:
   - **`code`** `(Recommended)`: pull requests, issues, CI runs and releases, and a snapshot of the
     default branch in memory, analysed by rules. No code leaves the probe.
   - **`metadata`**: pull requests, issues, CI runs and releases only; no file is read, and modules
     come from labels alone.
   - **`code_model`**: also sends the code to one model for an open question. It needs a code
     reader; read "A project that reads code with a model" in `docs/deploy.md` and say what it
     takes before they choose it.
3. **The GitHub token.** The person creates a fine-grained token for that repository alone,
   read-only, with an expiry: Metadata, Pull requests, Issues and Actions, plus Contents for `code`
   and `code_model`. They save it as `deploy/compose/secrets/github/<project>.token`; you check it
   isn't empty.
4. **The manifest**, `.mercury/probe.yaml` in the repository, from the preset's `probe.yaml`. Ask
   whether the repository is cloned on this machine: if it is, look at its folders and propose the
   modules (a name and its globs each) and the test frameworks (`rust`, `jest`, `vitest`, `xunit`,
   `nunit`, `mstest`, `junit`, `pytest`); if not, ask for them. Also ask for the labels of each
   module and of blocked work, the paths never to read (`exclude`: customer documents, keys,
   secrets), names to redact (a client's name → "the client"), and whether people may ask open
   questions (`open` in `allow`, the owner's opt-in). Write it into their clone, or into the
   scratchpad when there is none, and tell them: it goes into the repository by a pull request its
   owner reviews; you don't commit it. In `metadata` mode copy its rules into the project's entry in
   `config/projects.yaml` too (`manifest:`), since a token without Contents can't read the file.
5. **The project** in `config/projects.yaml`:
   ```yaml
     <project>:
       display_name: "<display name>"
       aliases: [<project>, <alias>]
       access_max: <mode>
       probe:
         url: http://probe-<project>:7000/mcp
         token_file: /run/mercury/probe-tokens/<project>.token
   ```
6. **Its probe pair in Compose.** On a laptop, in `deploy/compose/docker-compose.mine.yml`
   (git-ignored: it names private repositories); on a server, in `docker-compose.repos.yml` of the
   stack's folder, in place of the template's `delta`. Copy the `delta` block of
   `deploy/compose/docker-compose.repos.yml` (both services, the three volumes, the two secrets) and
   replace `delta` with the project id everywhere and `your-org/delta` with `owner/name`. Keep the
   `repo-<project>` volume a tmpfs and the `sync` health check as they are.

## P4: The judge and the policies

Ask:

- **Keep the preset's thresholds and policies** `(Recommended)`: they are measured on the labelled
  questions.
- **Review them**: then show, as tables, from the files themselves:
  - `config/checks.yaml`: each check with its thresholds. `block_at` refuses (a risk check, at or
    above it) or blocks (a legit check, at or below it); `review_at` holds an answer for an approver,
    or strips the sentence where the project has none; `pass_at` passes a legit check;
    `clarify_below` makes Mercury ask which project or kind of question was meant.
  - `config/policies.yaml`: what each role may ask, which classes wait for `signoff`, the global
    `deny` and `redact`, `limits`, `retention`.
  - `config/models.yaml`: the budgets.

  Change only what they ask for, showing each change first. Before they move a threshold,
  recommend `just judge-eval`, which measures the judge on the labelled sets.

## P5: Start it

- **A laptop:** `COMPOSE_FILE=docker-compose.yml:docker-compose.mine.yml just demo-local`, in the
  background. It builds the images and the web client, makes the probes' tokens, checks the
  configuration with the core itself, starts the stack with `poison` beside their projects, and
  waits until every probe is healthy. It stops and says why on a missing GitHub token, a template
  left in place or a project missing from `config/projects.yaml`: fix that and run it again.
  If the demo ran on this laptop before, the stack starts on its database, with the demo's four
  people in it, one of them an admin whose token sits in `secrets/demo-tokens`. Ask before you
  start it:
  **disable them** after the start (`user disable --email …` for `pm@`, `lead@`, `board@` and
  `admin@example.com`), or **start from an empty database** (`just compose-down`, then
  `docker volume rm mercury_core-data`, which deletes every person, token and question of that
  stack; `secrets/demo-tokens` then holds dead tokens, and the person deletes it).
- **A server:** P7's checklist, then in the stack's folder
  `docker compose run --rm core admin config check` and `docker compose up -d`.

## P6: Check it

- `docker exec mercury-core-1 mercury-core admin probe list`: each project `healthy`, with the mode
  it reads with and what lowered it (`token`, `manifest`, `access_max`, `size_cap`,
  `no_code_reader`). A probe registers once its `sync` has fetched for the first time; if one isn't
  healthy after two minutes, read `docker logs mercury-probe-<project>-1` and
  `docker logs mercury-probe-<project>-sync-1`.
- `curl -fsS http://localhost:8080/v1/health` (on a server, `https://<domain>/v1/health`).

Then Step 3 of SKILL.md, with their projects to grant.

## P7: A server's checklist

Only when you run on the server itself; the commands with `sudo` are the person's to run.

1. Linux with Docker Engine and the Compose plugin; `swapon --show` prints nothing (no swap on disk,
   or only an encrypted one); a DNS record `mercury.<domain>` points at the server; ports 80 and 443
   are open; only Caddy publishes ports.
2. In the clone: `just _images`. Then `sudo mkdir -p /opt/mercury && sudo chown "$USER": /opt/mercury`
   and `cp -R deploy/compose/. /opt/mercury/`: the stack's folder from then on.
3. In `/opt/mercury/.env`: `MERCURY_DOMAIN=mercury.<domain>`; leave `MERCURY_REGISTRY` and
   `MERCURY_VERSION`, which name the images just built.
4. The configuration of P2 to P4 goes into `/opt/mercury/config/` and `/opt/mercury/secrets/`; the
   probes' tokens there are made with `openssl rand -base64 48 | tr -d '\n' > secrets/probes/<project>.token`.
5. The person runs `sudo chown -R 10001:10001 secrets && sudo chmod -R u=rX,go= secrets`.
6. After P5: `bash scripts/check.sh stack https://mercury.<domain>` and
   `sudo bash scripts/check.sh secrets`.

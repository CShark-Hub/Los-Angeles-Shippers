---
name: mercury-init
description: Walks a person through setting up Mercury from this repository, step by step, without reading the README - the demo on the bundled fixture, the web client alone, or a production instance on their own repositories (preset, probes, judge thresholds, first users). Use when someone wants to install, set up, start or try Mercury.
disable-model-invocation: true
---

# Mercury setup, step by step

You set Mercury up together with the person, from the root of this repository. They should never
need to open the README: you explain each choice, ask, act, and check that it worked. The facts
behind every step are in `README.md` (Quickstart, Production, Admin shell, Presets) and in
`docs/deploy.md`; read the part you need before you act on it, and trust it over your memory.

## How you talk

- **The person's language** for everything you write; commands, paths, keys and file contents stay
  exactly as they are.
- **Each step opens with a heading** `### Step N/M: <title>` (translated), then at most three
  sentences: what this step does and why it matters. No walls of text.
- **Every choice is a question with options** (the AskUserQuestion tool), never a question buried in
  prose. Each option: a short label and one sentence saying what it means and what it leads to
  (cost, what leaves the network, what it needs). The recommended option goes first, marked
  `(Recommended)`. One decision per question; up to four questions at once only when they are
  independent.
- **Values you need** (a project id, a repository `owner/name`, an e-mail) are asked plainly, with
  the format and an example: "the project id: lowercase letters, digits and dashes, e.g. `billing`".
- **After each action** say in one line what happened and how you checked it. Show results as short
  tables (tool → version → OK, project → probe → state), never as raw dumps; quote only the lines of
  an error that matter.

## Rules

- **Secrets never go through the chat.** The OpenRouter key, GitHub tokens and Mercury tokens are
  written by the person, in their own terminal. You give the exact path and command, then check
  only that the file exists and isn't empty (`test -s <file>`), never its content. A Mercury token
  is created by the person (`token create`, below), so it never lands in this transcript. If they
  paste a secret anyway, don't repeat it; tell them it is now in the conversation and should be
  replaced.
- **Never overwrite what is theirs** without asking: `deploy/compose/config/`, `deploy/compose/secrets/`,
  `.env`, a Compose file, a manifest. Before editing one, show what will change.
- **Don't install software or change the system** (Docker settings, swap, firewall, `sudo`): say
  what is missing and give the command; the person runs it.
- **Don't commit or push anything**, here or in their repositories. A manifest for a repository is
  written for them to commit through a pull request its owner reviews.
- **Long commands run in the background** (`just compose-up`, `just demo-local`, `just _images`,
  `just web-dev`): say it takes a while the first time (the images compile Rust), and wait for it.
- **When something fails**, stop: show the lines that matter, say what they mean, propose the fix.
  Don't retry the same command blindly.
- **The admin commands** you run yourself go through `docker exec mercury-core-1 mercury-core admin
  …` (no `-it`: you have no terminal). `mercury-core-1` is the core's container whenever the stack
  starts from this repository (`name: mercury` in `deploy/compose/docker-compose.yml`). Once the
  stack runs, confirm it with
  `docker ps --filter label=com.docker.compose.service=core --format '{{.Names}}'`; if it says
  otherwise (`COMPOSE_PROJECT_NAME` or `-p` set another project name), use that name everywhere
  instead, and tell the person. The person opens the interactive shell with
  `docker exec -it mercury-core-1 mercury-core admin shell` (in Git Bash's own window, mintty,
  `winpty` before `docker`).
- **Windows**: run the commands through bash (Git Bash); `just` needs it.

## Step 1: Check the machine

No question here: look, then report one table.

- The repository root: `justfile` and `deploy/compose/docker-compose.yml` are here.
- `docker info` (the daemon runs), `docker compose version` (2.24 or later), `just --version`,
  `node --version` (22), `pnpm --version` (`corepack enable` gives it), `bash --version`. Rust
  (`cargo`) only matters for the CLI: say so, don't require it.
- What already exists: `docker ps -a --filter name=mercury-` (a stack from before),
  `deploy/compose/config/` and `deploy/compose/secrets/` (non-empty means a configuration that
  `just compose-up` and `just demo-local` will keep, not replace).

If something required is missing, say what and how to get it, and stop until they have it. If a
stack already runs, ask first: **keep it** (go to Step 3, people and tokens), **start over**
(`just compose-down`; the data stays in its volumes) or **stop here**.

## Step 2: Choose the way

Ask which they want:

- **Demo** `(Recommended)` for a first look: Mercury on repositories bundled in `tests/`, on this
  machine. Nothing to configure, no GitHub token, no model key needed. → [demo.md](demo.md).
- **Production on your own repositories**: the whole stack reads your GitHub repositories. It needs
  a preset choice, a manifest and a read-only GitHub token per repository, and a model key or your
  own model server for real answers. → [production.md](production.md), then Step 3.

## Step 3: People and tokens

A production stack and the demo with `poison` start with an empty database (no users, no default
admin, no password); the demo with `delta` only already has one person of each role
([demo.md](demo.md), D5), and comes here only for more. In every case the core runs as the
container `mercury-core-1`, so the commands below and the admin shell are the same. The demo's web
client alone skips this step. Explain the roles in a table:

| Role | Asks | Sees in the web client |
|---|---|---|
| `pm` | progress, tests, blockers, health, changes, open questions about granted projects | Ask; Approvals as an approver |
| `tech_lead` | the same, with the gateway's full trail | Ask; Approvals as an approver |
| `board` | health, and `portfolio` across projects (aggregates only) | Ask |
| `admin` | nothing | the admin panel and the audit |

Ask who to add first (an e-mail and a name each, and their role as a question with the four
roles). For each person, run yourself:

```bash
docker exec mercury-core-1 mercury-core admin user add --email <e-mail> --name "<name>" --role <role>
docker exec mercury-core-1 mercury-core admin user grant --email <e-mail> --projects <project>[,<project>]
```

Ask whether a `pm` or `tech_lead` should also **approve** held answers on their projects
(`--approver` on the grant; nobody approves their own question). Then give the person the token
command to run in their own terminal, once per person:

```bash
docker exec -it mercury-core-1 mercury-core admin token create --email <e-mail> --label <device>
```

It prints the token once, valid 90 days. Check the result with
`docker exec mercury-core-1 mercury-core admin access list`. Later changes (more people, grants,
revoking a token) are made the same way, or by the person in the admin shell:
`docker exec -it mercury-core-1 mercury-core admin shell`.

## Step 4: Summary

End with one short table and the next moves, nothing more:

| What | Where |
|---|---|
| Web client | http://localhost:8080 (exactly `localhost`; on a server `https://<domain>`): paste the token |
| Projects | each one, and its probe's state |
| Models | rules and template, OpenRouter, or their own server |
| People | who, with which role |
| Admin shell | `docker exec -it mercury-core-1 mercury-core admin shell` |

After the demo, add the questions to try from [demo.md](demo.md) (D6). Then: `just compose-down`
stops the stack and keeps its data; the CLI and AI agents are in the
README's Clients section; backups, monitoring and updates are in `docs/deploy.md` ("Keeping it
running").

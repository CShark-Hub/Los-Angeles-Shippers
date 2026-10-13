# Deploying Mercury

One Docker Compose project, two targets, the same images:

- **A laptop**: Caddy on `127.0.0.1:8080` over plain http. For trying Mercury, for development, and
  for a small team that reaches the laptop over a private network.
- **A Linux server**: Caddy with automatic TLS for your domain on ports 80 and 443. The way to run it
  for a company.

The files are in `deploy/compose/`. This page covers the secrets, the configuration, adding a
repository, people and tokens, and keeping it running.

## What runs, and what is exposed

| Service | Networks | Can reach | Secrets |
|---|---|---|---|
| `caddy` (`mercury-web`: Caddy with the web client) | edge | the core | TLS state |
| `core` | edge, probes, egress | the probes, the model provider | the model provider's key, every probe token (read-only) |
| `probe-<name>` (`serve`) | probes | the core only | its probe token |
| `probe-<name>-sync` | sync | the egress proxy, then the git host's API | its git host token |
| `egress-proxy` (`mercury-probe egress`) | sync, egress | `api.github.com`, `codeload.github.com` | — |
| `code-reader` (`mercury-probe reader`, the `demo` preset's) | probes, reader-out | its one upstream, through `reader-egress` | the reader's key |
| `reader-egress` (`mercury-probe egress`, its own allowlist) | reader-out, egress | the host of the reader's upstream, and nothing else | — |

Only Caddy publishes ports. Through it, `/v1/*` and `/mcp` reach the core; every other path is the
web client. The core's internal listener (8081), the probes (7000), the egress proxy and the
database are never exposed; admin commands run with `docker compose exec` on the machine itself.

The egress proxy is Mercury's own: `mercury-probe egress`, in the probe image. It is an HTTP proxy
that only tunnels: it takes `CONNECT` to port 443 of a host its allowlist file names, and refuses
everything else, any other method with 405 and a host or port off the list with 403. The allowlist
is a file mounted read-only, one anchored extended regular expression a line (`egress-proxy/allowlist`
holds `^api\.github\.com$` and `^codeload\.github\.com$`; `reader-egress/allowlist`, the reader's
one host).

The last two rows exist only where `docker-compose.reader.yml` is among the Compose files, which
`just demo-local` adds when `secrets/reader_api_key` holds a key. The reader is the one place code is
sent to a model: the `demo` preset's reader sends the code of a repository made for demos to
OpenRouter, with zero data retention, and nowhere else. `reader-out` is an internal network, so the reader's only way out is
`reader-egress`, whose allowlist (`reader-egress/allowlist`) is that one host. `serve` still has no
route out. [security.md](security.md) says what leaves the machine, and `check.sh` proves the route
twice. `check.sh config` fails a combination where `code-reader` mounts anything or is on any
network but `probes` and `reader-out`, where `reader-out` isn't internal or has another service on
it, where the allowlist holds anything but the one host of the reader's `READER_UPSTREAM_URL`, or
where any Compose file but `docker-compose.reader.yml` sets `READER_ALLOW_OUTSIDE`. `check.sh stack`,
on the running stack, shows every `serve` reaching the reader and not `reader-egress`, and the reader
reaching its upstream's host through `reader-egress` and nothing else.

Every service runs read-only, with every capability dropped (Caddy keeps `NET_BIND_SERVICE` for ports
80 and 443), as uid 10001, with no new privileges.
Each repository's code snapshot lives in a tmpfs volume: RAM only, empty after a restart. The disk
holds the core's database, the metadata and the facts index, never code.

## A laptop

The quickstart in the [README](../README.md) runs the stack with the bundled fixture repository
(`just compose-up`). For your own repositories on a laptop, `just demo-local` builds the images and
the web client and starts the stack with `docker-compose.repos.yml`: one `sync` + `serve` pair per
repository (see [Adding a repository](#adding-a-repository)). On a first run with an empty
`deploy/compose/config/` it copies the `demo` preset there and stops, so you can add your projects;
it refuses the template repository `your-org/…` and a missing git host token. Once the core answers
it waits until every probe is registered and healthy, and says which isn't.

- **The demo preset.** Beside your repositories, `docker-compose.poison.yml` (added by `just
  demo-local`) runs a `dev` probe on the red team's repository `tests/redteam/repo-poisoned` as the
  project `poison`: a README and titles with hidden instructions for AI agents, to show them
  withheld. Like every `serve` probe it has no network but the probes' and no git host token. The
  `demo` preset's `projects.yaml` holds the project, and its `policies.yaml` holds a project
  manager's `health` questions for an approver's sign-off; grant `poison` to whoever asks about it.
  A `config/` that doesn't start from that preset needs both, as `config-examples/presets/demo/` has them.
- **Without a repository of your own.** `COMPOSE_FILE=docker-compose.yml:docker-compose.fixture.yml
  just demo-local` runs the same stack on the bundled fixture (`delta`), with no git host token.

- **Swap.** Code pages must not be written to disk: set Docker Desktop's swap to 0 (on the WSL 2
  backend, `swap=0` in `.wslconfig`).
- **Two people on one laptop.** The project manager and the approver sign in from two windows of
  the laptop's browser (two browser profiles), both on `http://localhost:8080`.
- **Another device.** Browsers send the `Secure` session cookie over plain http only to
  `localhost`, so other devices need HTTPS. With Tailscale, `tailscale serve --bg 8080` gives the
  laptop an HTTPS address on your tailnet; set it as `MERCURY_PUBLIC_ORIGIN` in
  `deploy/compose/.env` and start the stack again. The core accepts a sign-in only from that origin.
- **Check it.** `bash deploy/compose/scripts/check.sh stack` checks the running stack (at
  `http://localhost:8080`, or the URL and CA file after it): the web
  client with its CSP header, the core's internal listener closed to Caddy, `serve` without a route
  out, only the git host's API through the egress proxy, and the snapshot in tmpfs; with the code
  reader running, every `serve` reaching the reader but not `reader-egress`, and the reader reaching
  its upstream's host and nothing else.
  After a sync, `bash deploy/compose/scripts/check.sh disk` checks that no code is left on a disk: it
  plants a file on a scratch volume to show it can see a copy, then looks on every named volume and in
  Docker's data directory for the files of each snapshot (for the fixture's, on a stack that syncs
  nothing), and checks that the volume under `/repo` is a tmpfs that a stopped container does not keep.
  `MERCURY_COMPOSE_PROJECT` names a Compose project other than `mercury`.

## A Linux server

1. **Machine.** Ubuntu 24.04 LTS, 2 vCPU, 4–8 GB RAM, 40 GB of SSD or more. A DNS record
   `mercury.<your-domain>` pointing at it.
2. **No swap on disk**, so memory pages holding code never reach the disk: no swap at all, or
   encrypted swap with a random key per boot. `swapon --show` must print nothing, or only an
   encrypted device.
3. **Access and firewall.** SSH keys only, no root login; `ufw` allowing 22 from your network and
   80 and 443 from anywhere; unattended upgrades. Docker writes its own firewall rules, so a
   published port is reachable whatever `ufw` says: **only Caddy may have `ports:`**.
4. **Docker** Engine and the Compose plugin from Docker's own apt repository. Membership in the
   `docker` group equals root; give it only to admins.
5. **Files.**
   ```bash
   sudo mkdir -p /opt/mercury && sudo chown "$USER": /opt/mercury && cd /opt/mercury
   # copy deploy/compose/ from the release you run into /opt/mercury
   # in .env: MERCURY_DOMAIN, MERCURY_VERSION, MERCURY_REGISTRY=ghcr.io/cshark-hub (the release's images)
   mkdir -p secrets/probes secrets/github
   printf '%s' '<your OpenRouter key>' > secrets/openrouter_api_key
   sudo chown -R 10001:10001 secrets && sudo chmod -R u=rX,go= secrets
   ```
   The containers run as uid 10001 and Compose mounts secret files with the host's permissions,
   hence the `chown`. There is no session secret: browser sessions are rows in the core's database.
6. **Configuration.** Copy a preset (below) into `config/`, add your projects, and check it:
   ```bash
   docker compose run --rm core admin config check
   ```
7. **Start.**
   ```bash
   docker compose pull && docker compose up -d && docker compose ps
   curl -fsS https://mercury.<your-domain>/v1/health
   ```
   Caddy obtains the certificate on the first request; the DNS record must already point at the
   server. `MERCURY_PUBLIC_ORIGIN` must be the exact origin people open (`https://mercury.<your-domain>`),
   or every sign-in is refused.
8. **Check it.**
   ```bash
   bash scripts/check.sh stack https://mercury.<your-domain>   # the web client's policy, 8081 closed to Caddy, the probes
   sudo bash scripts/check.sh secrets                          # every secret file uid 10001, mode 0400, its value in no log
   ```
   `check.sh stack` takes a CA file after the URL when the certificate isn't a public one (an internal
   CA, or Caddy's own for `localhost`). `check.sh secrets` reads each file to look for its value in
   `docker compose config` and the stack's logs, and the folders let in uid 10001 alone, hence
   `sudo`. Run by a user who isn't root, it reaches the files through `sudo -n` when that works, and
   otherwise fails naming the first file it can't reach.

CI runs the same files the same way on every pull request: `just ci-vm`
(`deploy/compose/scripts/vm-smoke.sh`) starts `docker-compose.yml` with the fixture's probe and a
replay server for the models (`docker-compose.vm-ci.yml`) and `MERCURY_DOMAIN=localhost`, so Caddy
serves TLS with its local CA as `tls internal` would, then checks over HTTPS that only Caddy
publishes and only 80 and 443, that http sends to https, `check.sh stack` with that CA, a browser's
sign-in and its cookie, an answer, a refusal, `check.sh secrets` and the audit chain. It needs
ports 80 and 443 free, and root or sudo for the secret files' owner; on Windows it skips the owners,
which the file system doesn't keep. It runs as the Compose project `mercury-vm`
(`MERCURY_VM_PROJECT` names another) and ends by removing that project's volumes, so it refuses
`mercury` and `mercury-it`, the laptop stack's and the integration runs' projects.

## Secrets

| Secret | File | Used by | Rotate |
|---|---|---|---|
| The model provider's key, with a credit limit | `secrets/openrouter_api_key` | core | on a leak, otherwise monthly |
| A probe token per repository | `secrets/probes/<name>.token` | the core and that probe | on a leak, or quarterly |
| A git host token per repository | `secrets/github/<name>.token` | that repository's `sync` | before it expires |
| The code reader's key (the `demo` preset's only) | `secrets/reader_api_key` | `code-reader` | on a leak, otherwise monthly |
| People's tokens | only their SHA-256, in the database | core | they expire (90 days by default) |
| Browser sessions | only the SHA-256 of the session id, in the database | core | 12 hours at most, never beyond the token |

Secrets are files, owned by uid 10001, mode 0400: never environment variables, never `.env`. To
rotate one, replace the file and restart the services that read it: the core for the model key;
the core and that probe for a probe token; that `sync` for a git host token; `code-reader` for the reader's key. The core reads them at
start only, so a restart is the rotation; every project answers again within seconds of it. A
person's token is replaced by creating a new one and revoking the old (`mctl token revoke`), with no
restart. `tests/drills/rotations.mjs` walks the first, the second and the last on a scratch stack.

## Configuration and presets

The core reads `projects.yaml`, `policies.yaml`, `models.yaml`, `checks.yaml` and `prompts/` from its
configuration directory at start, and stops on a file it can't read;
`mercury-core admin config check` says what is wrong, one line per problem, without starting
anything. Three presets are starting points in `config-examples/presets/`:

| Preset | Models | What leaves your network | Code, when a project uses the code reader |
|---|---|---|---|
| `demo` | a judge and chat models on OpenRouter, zero-retention routing | the redacted question, facts with their titles, redacted titles and labels for the judge | one outside model, zero retention, for a repository made for demos |
| `private` | the same models | the redacted question and facts without titles: statuses, numbers, dates, module names | your own model server only |
| `strict` | every role on a model server of your own (any OpenAI-compatible chat API) | nothing | your own model server only |

No type a probe answers with can hold code, file paths, diffs, commit messages, tokens or keys, so
no answer and no fact carries them, in any preset. The code reader is off until a project is given
the `code_model` mode; only then does code leave the probe, and only to the reader's one model.
[security.md](security.md) has the table by kind of data.

Without a model key the stack still runs: the rules judge every stage and a template writes the
answer, and every answer says so.

## Adding a repository

About fifteen minutes. The probe's access mode is the strictest of three switches: the git host
token's permissions, `access:` in the repository's manifest and `access_max:` for the project in
`config/projects.yaml`. Agree the mode with the repository's owner first; `code` is the default.

1. **The manifest.** A pull request adds `.mercury/probe.yaml` to the repository (the template is
   `probe.yaml` in each preset). Its owner reviews it like code: the modules, the classes allowed,
   the redaction rules, `judge_context` (which texts a judge may read), `access` and `exclude`
   (paths deleted from the snapshot before anything reads it).
2. **A git host token.** A fine-grained token for that one repository, read-only, with an expiry:
   Metadata, Pull requests, Issues and Actions for `metadata`; the same plus Contents for `code` and
   `code_model`. Save it as `secrets/github/<name>.token`.
3. **A probe token.** `openssl rand -base64 48 | tr -d '\n' > secrets/probes/<name>.token`, then
   the `chown` and `chmod` from the server steps.
4. **Compose.** Copy the repository block in `docker-compose.repos.yml` and replace its names; keep
   its `repo-<name>` volume a tmpfs and its `sync` health check. A snapshot above `PROBE_SNAPSHOT_MAX_MB` (512 by default) makes
   the probe fall back to `metadata`.
5. **The project.** Add it to `config/projects.yaml` (display name, aliases, the probe's address and
   token file, `access_max`), then run `config check`. For `metadata` mode, copy the manifest's
   rules into the project's entry too: a token without Contents can't read the manifest.
6. **Start it.** `docker compose up -d probe-<name>-sync probe-<name>`, then
   `docker compose restart core`. `docker compose exec core mercury-core admin probe list` shows it
   `healthy`, with the access mode it reads with and what limited it.

**A project that reads code with a model (`code_model`).** It needs a code reader, and every switch
agrees: `access: code_model` in the manifest, `access_max: code_model` for the project in
`config/projects.yaml`, a token with Contents, and `open` in the manifest's `allow`. The reader is the
`demo` preset's (`docker-compose.reader.yml`, with `secrets/reader_api_key`, which `just demo-local`
notices) or your own model host: copy that file, put your host in `READER_UPSTREAM_URL` and as the one
line of `reader-egress/allowlist`, drop `READER_ALLOW_OUTSIDE` (the reader refuses an outside host
without it, over `http`, or without its proxy), and drop the proxy too when the host is inside your
network. The project's `sync` and `serve` read `PROBE_CODE_READER_URL`, and `serve` also
`PROBE_CODE_READER_MODEL`: the repository block's lines `${MERCURY_CODE_READER_URL:-}` and
`${MERCURY_CODE_READER_MODEL:-}`, empty unless `just demo-local` sets them. A bare `docker compose up`
leaves them empty, and the pair then runs as `code`; a file of your own, such as
`docker-compose.mine.yml`, may set the two lines literally. Without a reader the probe says so
(`access_limited_by: no_code_reader`). The reader keeps its day's spend (`READER_DAILY_USD`, 1.00 in
the file) in memory only, so a restart of `code-reader` starts the day at nothing: set a credit limit on
the key itself too.

## People and tokens

With `alias mctl='docker compose exec core mercury-core admin'` (every command has `--help`):

```bash
mctl user add --email anna@example.com --name "Anna" --role pm       # pm, tech_lead, board or admin
mctl user grant --email anna@example.com --projects delta            # --approver makes a pm or a tech_lead an approver
mctl user set-role --email anna@example.com --role tech_lead         # holds from their next request; refused as board or admin for an approver
mctl user ungrant --email anna@example.com --projects delta          # the grant and its approver flag go
mctl token create --email anna@example.com --label laptop --expires 90d   # printed once, never again
mctl token list --email anna@example.com
mctl token revoke <public id>                                         # ends its browser sessions too
mctl user disable --email anna@example.com                            # revokes every token of theirs
mctl access list --project delta                                      # who may ask about delta, and its probe
```

`mctl access list` also says what each grant lets its person ask: the classes of their role in
`policies.yaml`, narrowed by the project's manifest and both deny lists, as the core applies them to a
question, with a class held for sign-off marked. A project whose probe hasn't registered and that has no
manifest in `projects.yaml` shows as not askable yet. If those two files can't be read, the people and
their tokens are listed all the same and the listing says why the classes are missing.

Setting the role a person has already, or taking back a project they weren't granted, changes nothing
and says so; a disabled person is refused. Every change above is an audit row.

### A prompt of its own

To type several of these one after another, open the admin shell once and leave the long prefix behind:

```bash
mctl shell                                    # on the host: docker compose exec core mercury-core admin shell
ssh -t admin@example.com 'cd /opt/mercury && docker compose exec core mercury-core admin shell'
```

```text
mercury> user add --email anna@example.com --name "Anna Carter" --role pm
mercury> user grant --email anna@example.com --projects delta
mercury> access list --email anna@example.com
mercury> help                                 # the commands; `user add --help` explains one
mercury> exit                                 # or Ctrl-D
```

It takes the same commands as `mctl`, read by the same parser, so each has the checks and the `--help` it
has outside the shell; a command that fails says why and the prompt stays, and Ctrl-C while a command
runs stops that command and keeps the prompt. Words with spaces go in quotes.
It asks for no login, as `mctl` doesn't: being on the host that runs the core, with the rights to
`docker compose exec`, is the access. Each change is an audit row written by its command; the shell
writes none of its own.

At a terminal the shell opens with a header kept on the top rows while the commands scroll under it: the
version, how many people and active tokens there are, each probe as a dot (green healthy, amber stale,
red quarantined), today's spend against the daily budget, and the alerts that hold, as `status` says
them. It is drawn again after each command, so it shows what the command changed; reading it writes no
audit row. In a terminal too small to keep it on top it is shown once and scrolls away. What scrolls up
under a header kept on top isn't kept in the terminal's scrollback, so for a long listing run the
command outside the shell and send it to a file (`mctl audit export > audit.jsonl`).

The line is edited as in any shell: the arrows move in it and walk the lines typed before (up and down),
Home and End and Ctrl-A and Ctrl-E jump, Ctrl-U and Ctrl-W delete, Ctrl-L clears the screen under the
header, Ctrl-C drops the line and a second Ctrl-C in a row leaves the shell. Tab completes a command, a
subcommand or a flag, and lists the choices when there are several. The command is coloured as it is typed: the words that name a command in green,
the flags in cyan, what is quoted in yellow, and anything the parser wouldn't know in red. The history
lives in memory for the session and is never written to disk. `NO_COLOR` turns the colours off, and
`MERCURY_ASCII=1` draws the header with plain ASCII for a terminal that can't show box-drawing characters.
`TERM=dumb` reads plain lines, with no editor. The `-t` of the SSH line gives it a terminal; lines piped
into `mercury-core admin shell` run as a script, with no header, no prompt and no colour, and the script
stops at its first line that fails: the shell says which line and exits 1, and nothing after it runs.

```bash
docker compose exec -T core mercury-core admin shell < people.txt   # -T: no terminal, a script
```

Granting a project again sets its approver flag to what that command says, so a plain `grant` on a
project the person approves clears the flag. The grant rows say what happened to sign-off authority:
a grant's row names, for each project of the command, the approver flag before and after, and an
ungrant's row names the grants taken back that carried the flag. Only a `pm` or a `tech_lead`
approves: `grant --approver` refuses a `board` or `admin` person, `set-role` won't move an approver
to `board` or `admin` (grant the project again without `--approver` first), and where approval is
used an approver flag counts for those two roles alone.

An admin signed in to the web client reads the same in its admin panel (the overview of what Mercury
did, the people, the repositories, the audit), read-only: each change stays one of these commands, and
the panel shows the command to copy. Each read of the panel is an audit row (`admin_viewed`).

Give a token through a password manager or in person, never in chat or e-mail. The person pastes it
into the web client's sign-in page, which exchanges it for a session at once and keeps nothing, into
`mercury login`, or into their AI agent's settings.

### AI agents over MCP

An AI agent such as Claude Code asks Mercury over MCP at `/mcp`, with the token of the person it
works for:

```bash
claude mcp add --transport http mercury https://mercury.example.com/mcp --header "Authorization: Bearer mcy_…"
```

On a laptop the address is `http://127.0.0.1:8080/mcp`.

- **The agent acts as the token's owner.** The same projects, rate limit and budget, the same checks
  with no shortcut for agents, and the same audit: its questions are the owner's, on the channel `mcp`.
- **Two tools.** `list_projects` lists the projects the token may ask about; `ask` takes a question
  and, if needed, a project, and returns the checked answer with its facts and confidence, or the
  refusal with its code and hint. When Mercury asks which project or which kind of question was
  meant, the agent calls `ask` again with the same question and `clarify` set to the question's id
  and the option picked. After an approver rejects an answer, `follows` set to the rejected
  question's id asks again and links the two in the audit.
- **Two minutes at most.** `ask` waits up to 120 seconds, or until the agent drops the call. Then it
  says the question is still running, with its id and its address in the web client
  (`mercury ask --wait <id>` follows it too); the question goes on. Asking again starts another
  question, unless the call repeats the same `idempotency_key`: an agent that sends one with each
  question gets the same question back when it retries.
- **Only the Bearer token.** `/mcp` reads the `Authorization` header alone; the web client's session
  cookie doesn't work there.
- **A refusal is an answer.** It comes back as a normal tool result, not an error, and the agent
  should pass its message and hint on to the person.

## Keeping it running

- **Daily:** `mctl status`: the alerts that hold, each probe's state, each model role's backend,
  today's spend against the budgets, and which judge decided the questions of the last 24 hours.
  It reads the database, `models.yaml` and `projects.yaml` only, so it answers with the model
  provider down. On a
  full disk it says so and shows nothing, because it writes a row to the audit first.
  `mctl status --since 2026-10-07` adds the questions asked since that date (UTC), by channel
  (`cli`, `web`, `mcp`) and outcome: answered, refused, degraded (answered by the chat or rules
  judge), held for a sign-off, and the rest (a clarify, a failure, still running). They are the
  questions of this database, so count on the machine where they were asked. Then a smoke test
  with the CLI, as a script would ask: `MERCURY_TOKEN=… bash deploy/compose/scripts/smoke.sh <url>
  <project>` asks one question that must be answered (exit 0) and one that must be refused for
  asking for code (exit 2), and exits 0 only when both hold (1 when either doesn't, 2 when it can't
  run). Give it a token of its own, for a person granted the project.
- **Monitoring:** an uptime check on `/v1/health` every minute. The core also checks three
  thresholds every minute, from its database alone, and logs each one that is crossed in fixed
  words (`alert: …`, never a question's text), at most once an hour while it holds (one that clears
  and comes back warns again at once):
  - spend today at 80 % of the daily budget (`global_daily_usd`) or more; a daily budget of 0 is
    an alert at once, since nothing can be asked;
  - a degraded judge for more than 5 minutes;
  - a probe silent for more than 15 minutes (a quarantined one doesn't count), and a project of
    `projects.yaml` whose probe has never registered, once the audit's first row is 15 minutes old
    (so a fresh stack's first minutes raise nothing).

  The thresholds are fixed. The judge is read from the questions that ended in the last 24 hours,
  leaving out those no judge answered and those held for a sign-off (their ending is written when
  an approver decides, hours later): it is degraded when the latest one was judged by `chat` or
  `rules` and the run of such questions since the last one judged by Jev began more than 5 minutes
  ago, so it clears with the first question Jev answers again, or after 24 hours without any.
  `mctl status --check` says the alerts that hold, one line each, and exits 1; with none it says
  and writes nothing and exits 0. A `models.yaml` it can't read is a line of its own and exits 1
  too, since the spend can't be checked, and so is a `projects.yaml` it can't read (the probes that
  never registered can't be checked): a check that can't look never passes. Use it from cron on
  a server, or from a loop on a laptop:

  ```bash
  # a server: every minute (cron mails what the command prints)
  * * * * * cd /opt/mercury && docker compose exec -T core mercury-core admin status --check

  # a laptop, from deploy/compose: the health check, then the alerts, once a minute
  while true; do
    curl -fsS http://localhost:8080/v1/health > /dev/null || echo "$(date -u +%FT%TZ) the core doesn't answer"
    docker compose exec -T core mercury-core admin status --check || true
    sleep 60
  done
  ```

  A `sync` that stops fetching shows in none of these: `serve` keeps answering from its last index, so
  the core's probe status stays green. Its health check does: each good metadata round rewrites
  `/meta/github.json`, and a file older than 16 minutes marks the service `unhealthy` in
  `docker compose ps` (10 minutes after a start, so the first round has time). Docker marks it; it
  doesn't restart it. Look at it with the other checks.

  There is no mail or chat notification beyond the log and that exit code.
- **Backups:** only the core's database matters (people, tokens, sessions, questions, the audit);
  snapshots and indexes rebuild themselves. `mctl backup --to <file>` copies it with SQLite's
  `VACUUM INTO` while the core keeps serving: one consistent snapshot, nothing half-written. It
  never overwrites a file that is already there, and the backup is a row in the audit (the file's
  name and size). The copy is written as `<file>.partial` and takes its name only once that row is
  written, so a file by the name you gave is always a complete, audited backup; a `.partial` left
  behind is an incomplete copy to delete. It needs free space for a whole database on the target's
  volume. The file is the whole database: the token and session digests, the redacted
  questions and answers, the audit chain. No secret is in it, but keep it like one; on Linux it is
  readable by its owner only. Each night, `bash deploy/compose/scripts/backup.sh` runs these lines
  (from cron on a server, by hand on a laptop): it names the copy by the date and time, takes it
  out of the volume as a `.partial` file that takes the copy's name only once it is whole, and
  keeps the newest 14 copies in `deploy/compose/backups/` (`BACKUP_DIR`, `BACKUP_KEEP`), older ones
  deleted; `--dry-run` says what it would run and delete. A failed step stops it with a non-zero
  exit, so no stale copy is kept as the newest: a copy cut off on its way out is deleted, never
  counted as a backup, and the copy on the core's volume is removed whether or not the copy out of
  it worked, so a failed night leaves nothing there. It lists the copies with the shell alone, so
  it runs the same with BSD tools. By hand, from bash in the stack's folder (`/opt/mercury`, or
  `deploy/compose/` on a laptop; on Windows, Git Bash with `export MSYS_NO_PATHCONV=1` first):
  ```bash
  set -euo pipefail   # a failed backup stops the job
  day=$(date +%F); mkdir -p backups
  docker compose exec -T core mercury-core admin backup --to /data/backups/mercury-$day.db
  docker compose cp core:/data/backups/mercury-$day.db ./backups/
  docker compose exec -T core rm /data/backups/mercury-$day.db
  ```
  Ship `./backups` off the machine (restic or rclone, for example) and keep two weeks. Scheduling
  it and the copy off the machine are yours to set up.
- **Restore** into a fresh `core-data` volume, with the configuration and the secrets in place:
  ```bash
  docker compose stop core && docker compose rm -f core
  docker volume rm mercury_core-data      # only once the old database is lost or copied away
  docker compose run --rm --no-deps -T --entrypoint sh core -c 'cat > /data/mercury.db' < ./backups/mercury-YYYY-MM-DD.db
  docker compose up -d                    # on a laptop: just compose-up, or just demo-local
  mctl audit verify
  ```
  Then `mctl access list` and one question from the CLI; the probes register again by themselves.
  `mctl audit verify` may count a few rows more than the copy held: every listing an admin reads is a
  row, and a probe that started again (restarted with the stack, or recreated by `docker compose up`)
  asks the core's judge about its items once, which is three `probe_judge` rows (its titles, its status
  conflicts, its kinds), with no text in them. A probe that kept running writes none. Nothing is lost.
  The `run` line writes the file as the core's own user into an empty volume. A `docker compose cp`
  into the container would leave a file owned by root, which the core can't write, and an old
  write-ahead log beside the database would be replayed onto the copy. The aim: lose at most a
  day, and be back within 30 minutes.
- **The audit:** `mctl audit verify` recomputes the hash chain and names the first row that doesn't
  hold. `mctl audit export` writes rows as JSON lines that verify without the database. Retention
  clears payloads, question texts and answers older than `retention.audit_payload_days`
  (90 by default); rows and hashes stay, so the chain still verifies.
- **Updates:** a version tag publishes `mercury-core`, `mercury-probe` and `mercury-web` on
  `ghcr.io/cshark-hub` (linux/amd64 and linux/arm64, each scanned before it is pushed) and the CLI for
  Linux, macOS and Windows on the release's page, with checksums; the notes are the version's
  section of `CHANGELOG.md`. Read them, set `MERCURY_VERSION` in `.env` (without the `v`), then
  `docker compose pull && docker compose up -d`, `/v1/health`, and one question from the CLI and one
  from the web client. The three images share the version; roll back by setting the previous one.
  Copy `deploy/compose/` from the same release when its notes say the Compose files changed.

## When something fails

| Failure | Behaviour | Visible as |
|---|---|---|
| The judge model is down | a chat model judges, with stricter thresholds | the web client's timeline says `chat (fallback)`, the CLI's `judge: chat`; `mctl status` counts the questions under `chat`, and after 5 minutes lists `judge: degraded` under Alerts (`mctl status --check` exits 1) |
| The model provider is down | the rules judge, a template writes | the timeline says `rules` and `template` |
| A probe is down or stale | other projects answer; this one says its data is unavailable, or how old it is | a stopped probe: `probe_unavailable` at once, and `stale` in `mctl probe list` 2 to 3 minutes later; a probe that answers but hasn't registered for 3 minutes: an "as of" note |
| An audit row can't be written (a full disk) | the question fails: no audit, no answer | the error `internal`; `/v1/health` still says `ok`; `mctl status` says the disk is full and shows nothing. Free the space and run `mctl audit verify`: questions are answered again, without a restart |
| The budget is spent | new questions are refused until the next day (UTC) | `budget_exceeded` |
| The code reader is down, slow or past its daily cap | an open question is answered from the project's data alone; the core waits 45 s at most for it (`serve` gives up on the reader at 40 s, the reader on its upstream at 35 s) | the answer ends "The code couldn't be read this time, so this answer rests on the project's data alone."; the reader's log names the error (`daily_cap`, `upstream_timeout`, `upstream_failed`), never a body; `node tests/drills/reader-upstream-down.mjs` rehearses it |

None of these ever lets more through than normal operation does.

## A probe on another host

When a repository's probe must run elsewhere (network rules, the repository's size), connect the two
hosts over a private network such as Tailscale: on the remote host run the same `sync` + `serve`
pair, with the same tmpfs snapshot and the same swap rule, and publish `serve`'s port only on its
private address; on the core's host publish the internal port 8081 only on its private address and
add the private range to `MERCURY_INTERNAL_ALLOW_CIDRS`. That is the one exception to "only Caddy
publishes ports". In `projects.yaml` the project's probe URL is the remote address, and the remote
probe's `PROBE_CORE_INTERNAL_URL` points back at the core. Allow nothing else between the hosts.

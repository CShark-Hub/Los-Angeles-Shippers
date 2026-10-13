# Contributing

Thank you for helping. This page says how to set up, which commands to run, and what a pull request
needs. How the parts fit together is in [docs/overview.md](docs/overview.md); the rules that keep
code away from answers are in [docs/security.md](docs/security.md).

## Setting up

You need:

- the Rust toolchain `rust-toolchain.toml` pins (`rustup` installs it on first use);
- [`just`](https://github.com/casey/just), [`cargo-nextest`](https://nexte.st),
  [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) and
  [`gitleaks`](https://github.com/gitleaks/gitleaks) on the `PATH`;
- for a change to the database, [`sqlx-cli`](https://github.com/launchbadge/sqlx/tree/main/sqlx-cli)
  at the version of the lockfile's `sqlx`:
  `cargo install sqlx-cli --locked --version 0.9.0 --no-default-features --features rustls,sqlite`;
- Node.js 22 with pnpm through corepack (`corepack enable`);
- Docker with Compose 2.24 or later;
- bash. On Windows, run `just` from Git Bash.

## Commands

| Command | What it does |
|---|---|
| `just dev` | the core on `http://localhost:8080` and a probe in dev mode on the fixture repository; Ctrl-C stops both |
| `just web-dev` | the web client's dev server on `http://localhost:5173`, answering from recorded questions; `just web-dev proxy` uses the core of `just dev` |
| `just compose-up` / `just compose-down` | the whole stack in Docker Compose on the fixture repository |
| `just lint` | `cargo fmt` check and clippy with warnings as errors |
| `just test` | the unit tests |
| `just it` | whole questions through the stack on Compose, with recorded model answers; `--web` adds the web client in a browser |
| `just redteam` / `just golden` | the attack questions, and the labelled questions with their report |
| `just openapi && just types` | the API document, and the web client's types generated from it |
| `just ci` | everything CI runs, in five parts: `ci-policy`, `ci-rust`, `ci-web`, `ci-e2e`, `ci-vm` |

`just --list` shows every recipe. Run `just ci` before you push: the CI workflow runs the same five
parts as parallel jobs. Nothing in it needs a secret, so it runs on a fork as it does here.

## Pull requests

- **Target `development`.** Every pull request targets `development`. Only a promotion pull request,
  from `development`, targets `main`; `main` moves in no other way.
- **A review and a green CI.** A pull request is merged only once a maintainer other than its author
  has approved it and all five CI jobs (`ci-policy`, `ci-rust`, `ci-web`, `ci-e2e`, `ci-vm`) have
  passed.
- **Merge commits.** Every pull request, a promotion included, is merged with a merge commit, never
  squashed or rebased.
- **No direct pushes.** Nobody pushes to `main` or `development` directly, and nobody force-pushes
  to either.
- **Small, with their tests.** A change and the tests that show it work belong in the same pull
  request.
- **Conventional Commits**, in English: `feat(core): …`, `fix(web): …`, `docs: …`, `test(it): …`.
- **The shared types.** A change to `crates/mercury-schema` crosses a boundary: regenerate the API
  document and the client's types (`just openapi && just types`), update the snapshot tests, and
  describe the change to the contract in the pull request.
- **Checks, prompts and thresholds.** A new check goes into `config-examples/checks.yaml` with a
  labelled or an attack case that exercises it. A prompt changes by a new versioned file in
  `config-examples/prompts/`, never by editing one in place. Run `just presets-sync` after changing
  the examples.
- **The database.** A new table or column is a new migration in `crates/mercury-core/migrations`;
  run `cargo sqlx prepare --workspace` and commit `.sqlx/`.

## Rules that are never bent

- No free-text field in a probe's reply types: enums, numbers, dates or `SafeText` only.
- No model output shown before the output checks: clients show text only from the `answer` event.
- A judge error is never a pass, nor a default.
- No model chooses tools; the planner fills validated arguments only.
- No tokens, question text, facts or answers in logs at info level.
- The probe's `serve` gets no network access and no credentials.
- No CORS on the core, and no token in browser storage.
- Tests and prompts use the synthetic repositories in `tests/fixtures` and `tests/redteam`, never
  real repository content. Examples use `example.com`, never a real person's name, address or
  credentials.

## Reporting

Bugs and ideas: open an issue. A vulnerability: see [SECURITY.md](SECURITY.md), never a public
issue. Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

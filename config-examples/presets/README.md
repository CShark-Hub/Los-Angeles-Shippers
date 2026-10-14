# Presets

Three starting points for the files the core reads from its configuration directory. A preset is a
folder of example configuration, not a switch in the code: you copy one, change what is yours, and
the core reads it like any other.

| Preset | Models | Shown to the models | What leaves your network |
|---|---|---|---|
| `demo` | Jev and chat models on OpenRouter (zero retention) | titles and labels of the repository, redacted | the redacted question, facts with their titles, the redacted titles and labels for the judge |
| `private` | the same models | no repository title, and no repository text for the judge | the redacted question and facts without titles: statuses, numbers, dates, module names |
| `strict` | every role on a model server of your own, any server with the OpenAI chat API | titles and labels, read by your own models | nothing |

The code reader is off in every preset until a project is given `code_model`: by its admin
(`access_max` in `projects.yaml`) and by its manifest (`access`), with a reader to call. Then
`private` and `strict` send code only to your own model server; `demo` may send the code of a
repository made for demos to one outside model, with zero retention. The reader itself refuses a
host outside your network unless `READER_ALLOW_OUTSIDE` is set, and only the `demo` preset's
Compose file for it, `deploy/compose/docker-compose.reader.yml`, sets it.

`demo` is the examples in `config-examples/` as they are.

## What is in a preset

Each folder holds what `mercury-core admin config check --dir <folder>` reads, and what the core
needs to start:

- `projects.yaml`, `policies.yaml`, `models.yaml` and `checks.yaml`;
- `prompts/`, every version `<role>.v<N>.md` of each role's prompt, as in `config-examples/prompts/`;
  the core uses the newest, the one with the highest `N`;
- `probe.yaml`, a manifest template. It does not go in the configuration: it goes in the repository
  the probe reads, as `.mercury/probe.yaml`, and its owner reviews it like code.

## Using one

```bash
cp -R config-examples/presets/private/. config/        # your configuration directory
mercury-core admin config check --dir config           # what is wrong, one line each
```

Then change `projects.yaml` to your projects, mount the secrets it names (each probe's token file,
and the key file of a backend that has one), and put `probe.yaml` into each repository, edited. The
keys that say `your-*` and the models that say `local/*` are placeholders.

## What differs, and why

- **`demo`** caps a question at 0.15 US dollars (`per_question_usd` in `models.yaml`), room for an
  open question that reads the code; `private` and `strict` keep 0.05.
- **`demo`** lets a token ask 50 questions at once and 300 an hour (`limits` in `policies.yaml`), so a
  person trying it never waits; `private` and `strict` keep 5 and 30. The budgets, not this limit,
  are what caps the spend.
- **`private`** sets `show_titles: false` in `policies.yaml`, so a repository's titles are withheld
  before any model sees them, and `judge_context: []` in the manifest, so the judge reads no
  repository text either. Answers are about statuses, numbers, dates and module names.
- **`strict`** keeps every role on one `openai_compatible` backend and has no OpenRouter backend at
  all. Its judge is a chat judge (`kind: chat`) with no Jev, so the chain is that judge, then the
  rules of each stage. A chat judge is uncalibrated: every risk threshold moves 0.2 toward refusing.
  Failing closed still holds, but expect more refusals and more answers held for an approver, and
  measure it on your own questions before relying on it.
  - The backend's `key_file` is `null`: a server that wants no key is sent none. Name a key file if
    yours does; a file that is named but missing or empty fails every request instead of sending it
    unauthenticated.
  - A local server reports no cost, so its calls add nothing to the budgets.

## Keeping them in step

`checks.yaml` and the prompts are the same in every preset, and `demo` is the examples exactly, so a
change is made once, in `config-examples/`, and copied:

```bash
just presets-sync     # copy the shared files into every preset
```

`just ci` fails when a preset has drifted (`deploy/compose/scripts/presets.sh check`), runs the real
`config check` on each one in the core image, and `crates/mercury-core/tests/presets.rs` shows that
each builds everything the core needs to start and keeps what its name promises.

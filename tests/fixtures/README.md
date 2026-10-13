# Test fixtures

## `repo-delta`

A synthetic repository, **Delta**, an invoicing service in C#, with the metadata a probe's `sync` would have
written for it. The probe's `dev` mode reads it with `--repo tests/fixtures/repo-delta --meta
tests/fixtures/repo-delta/meta`, and the leak test walks it for its corpus. Nothing in it builds or runs;
the probe only reads it. Every name, login, address and number in it is made up.

The metadata is anchored at `fetched_at` 2026-09-30T12:00:00Z. Tests that count windows such as "the last 7 days"
or "the last 14 days" should count back from it, so the fixture's facts don't change as the calendar moves.

### What the repository holds

| Module (from `.mercury/probe.yaml`) | Files | xUnit tests | TODO / FIXME |
|---|---|---|---|
| Invoicing (`src/Invoices`, `src/Billing`, `tests/Invoicing.Tests`) | 9 | 8 | 2 / 2 |
| Export (`src/Export`, `tests/Export.Tests`) | 4 | 3 | 1 / 1 |
| Auth (`src/Auth`) | 3 | 0, so `tested` can be `missing` | 1 / 0 |
| Other: files no module glob matches (`src/Delta.Api`, `src/Shared`, `config`, `db`, the README, the CI workflow, the manifest) | 8 | 0 | 1 / 0 |

A test is one `[Fact]` or `[Theory]` attribute; `tests/Export.Tests/StatementFixtures.cs` is a helper with none.

The manifest's `allow` lists `open`: the repository's owner opts in to questions that are none of the five kinds, so the golden set's open rows are answered on Delta, from the facts alone (it runs `code`, with no code reader). Every other list in it is as it was.

Each `exclude` glob in the manifest matches one file, which must never be indexed:
`docs/customers/megabank-onboarding.md` (with a TODO that must not be counted), `deploy/secrets/relay.env`
and `deploy/certs/delta-signing.pem`. They hold placeholders, not secrets, so the secret scan stays quiet.

For the leak test, the source has well over a hundred CamelCase, camelCase and snake_case identifiers of eight
characters or more, distinctive paths, and code lines that appear nowhere else.

### What the metadata holds (`meta/github.json`, `meta/history.json`)

The fields are the GitHub REST API's own names, trimmed to what the probe reads.

- **Status signals:** a PR merged with its linked issue closed (#412 closes #398); merged without a link
  (#415); open, not draft, with a review requested (#421); a draft (#423); open with neither (#424); closed
  without a merge (#410); an issue with a blocking label (#405 `blocked`, #411 `waiting`); open issues with no
  PR (#430 and others); and a conflict, an open PR in review that fixes a blocked issue (#422 fixes #407).
- **Module mapping:** PR file lists for globs, and labels from `module_labels` for metadata mode. The
  labels include two names people use that no pull request carries: `sepa` for Export and `late fees` for
  Invoicing, so that the questions that call a module by one are read as that module (the tool stage's
  judge reads each module with its labels). #424 and #425
  carry no module label; #425 changes only a file that matches no glob.
- **Title safety:** titles with an identifier (#419), backticks (#424), a path (#426), a URL (#427) and an
  e-mail address (#428); none of them may be shown as written.
- **Redaction:** the client name from the manifest's `redact` rule in #423 and #431.
- **Input that must never come out:** PR and issue bodies with identifiers and paths; branch names that contain
  identifiers (#419); a profile name that is an e-mail address (`kbrenner`); a profile name with an apostrophe
  and a hyphen (`tvale`).
- **CI on the default branch:** ten runs; in the last 7 days five, four passed, one passed only on its second
  attempt, and the latest failed. Every merged PR's merge commit has a run.
- **Releases:** two, the latest `1.4.0 Autumn invoicing`.
- **History:** ten commits with their files and no messages, one older than 14 days, one touching the excluded
  paths, and three touching test files.

Keep these properties when you change the fixture, and update this list with them.

## What and why

<!-- One or two sentences: what this pull request changes and why. -->

## How it was tested

<!-- The commands you ran (`just lint`, `just test`, `just ci`) and anything you checked by hand. -->

## Checklist

- [ ] It targets `development`. Only a promotion from `development` targets `main`.
- [ ] The tests that show the change works are in this pull request.
- [ ] Commit messages follow Conventional Commits, in English.
- [ ] A change to `crates/mercury-schema` comes with `just openapi && just types` and updated snapshots.
- [ ] `README.md` and `docs/` say what changed for people who run or use Mercury, if anything did.
- [ ] No secrets, tokens, personal data, client data or internal links in the code, the tests or
      this description.

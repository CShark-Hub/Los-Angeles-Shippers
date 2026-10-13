# Security policy

Mercury exists to keep code, secrets and people's data away from those who shouldn't see them, so a
weakness in it matters. Thank you for reporting one.

## Reporting a vulnerability

Report it privately through GitHub: on this repository's **Security** tab, choose **Report a
vulnerability**. Please don't open a public issue, a pull request or a discussion for it.

A useful report says:

- what an attacker can do, and what they need first (a token, a role, a repository they control, a
  page in the same browser);
- how to reproduce it: the commands, the request or the repository content, against the fixture
  repository in `tests/fixtures/repo-delta` when you can;
- the version or the commit you tested.

You'll get an acknowledgement within a few working days, and we'll keep you posted while we fix it.
Once a fix is released we publish an advisory, crediting you unless you'd rather not be named.

## What counts

Anything that breaks the promises in [docs/security.md](docs/security.md), for example:

- code, file paths, diffs, commit messages or secrets reaching an answer, a log, the audit or a model
  (the one exception: a project with the code reader turned on sends its code to the one model its
  configuration names, and to no other);
- a person reading a project or a class of question their role and the project don't allow;
- a model choosing tools, or text from a repository acting as an instruction;
- a judge error, or a failure of any check, treated as a pass;
- a probe's `serve` process reaching the network, the code reader reaching any host but its one model
  host, or a token or session usable beyond its scope;
- an audit row changed without `audit verify` noticing.

Prompt-injection and data-leak findings that the pipeline should have refused are welcome too, even
when nothing reached an answer: they become cases in `tests/redteam`.

## Supported versions

Mercury has no release yet. A security fix is made like any other change: it is merged into
`development` first and reaches `main` with the next promotion pull request, as
[CONTRIBUTING.md](CONTRIBUTING.md#pull-requests) describes. Once releases exist, the latest release
gets security fixes.

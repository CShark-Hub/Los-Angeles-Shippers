---
name: Another repository host
about: Ask for a probe that reads a host other than GitHub (GitLab, Bitbucket, Azure Repos, Gitea, …)
labels: enhancement, probe
---

<!--
Mercury's probe reads GitHub today. A probe is a separate program with a published contract
(docs/probe-spec.md) and a conformance runner (`mercury-probe-conformance`), so a probe for another
host can also live outside this repository. Nothing private belongs here: no tokens, internal
addresses or repository names.
-->

## Which host

<!-- The host, cloud or self-hosted, and its version if self-hosted. -->

## What the probe would read there

<!-- Which of these the host has, and through which API: pull or merge requests, issues or work
items, labels, CI runs and their results, releases. Mercury answers from these; a host without one
of them gives fewer kinds of answer. -->

## How many repositories, and how big

## Would you write or test it

<!-- A probe written by you, against the spec and the conformance runner, or testing one we write. -->

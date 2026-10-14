# Mercury web client

A single-page app, built to static files and served by Caddy from the same origin as the core's
`/v1/*`. See the repository's `CONTRIBUTING.md` for the rules the code keeps.

```bash
just web-dev             # http://localhost:5173 in mock mode: recorded questions, no core
just web-dev proxy       # the same, with /v1 proxied to the core that `just dev` runs
pnpm lint && pnpm check  # prettier, eslint and svelte-check
pnpm test                # the transport, the session, the mock mode and the storage audit
pnpm build               # static files in build/
```

Everything but Sign in needs a session; without one the app goes to Sign in. The screens, and who
sees each:

| Screen                                                              | Path                                                                          | Who                                                                             |
| ------------------------------------------------------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| Sign in                                                             | `/signin`                                                                     | whoever is signed out; a signed-in person is sent to Ask                        |
| Ask                                                                 | `/ask`, and `/ask/<question id>` for a question already asked (`/` opens Ask) | everyone signed in                                                              |
| Approvals: the answers held for a sign-off                          | `/approvals`                                                                  | an approver: a person who approves held answers on at least one granted project |
| Settings: who you are, your token, this browser                     | `/settings`                                                                   | everyone signed in                                                              |
| Overview: the questions, their outcomes and cost, what needs a look | `/admin/overview` (`/admin` opens it)                                         | an admin                                                                        |
| People & access: each person's role, grants and tokens              | `/admin/people`                                                               | an admin                                                                        |
| Repositories: each project's probe and access                       | `/admin/repositories`                                                         | an admin                                                                        |
| Audit: the rows, their filters and the export                       | `/audit`                                                                      | an admin                                                                        |

The side panel links only the screens the person may use. Opened directly, Approvals, the admin
screens and Audit show a notice instead and ask the core for nothing. The admin screens change
nothing: beside what they show they give the command that changes it, ready to copy and paste
into the admin shell on the server (`docker exec -it mercury-core-1 mercury-core admin shell`).

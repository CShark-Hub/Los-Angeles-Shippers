# Judge fixtures

`recorded/` holds the happy-path replies the tests use, as the real endpoints gave them:

- `decisions.json`, Jev's reply from OpenRouter's Decisions endpoint (`typesafe/jev-1.13`, answered
  by the dated `typesafe/jev-1.13-20260917`) to the batch of `tests/jev.rs`;
- `chat.json`, a chat model's reply from OpenRouter's chat endpoint (`openai/gpt-4.1-mini`) to the
  same batch, sent the way the chat judge sends it.

They are replies only: the requests the tests make are built in the tests, and no key or header is
saved. The questions are synthetic. A test asserts what a reply says by reading it back from the
recording, so a new recording does not break it; it asserts the same things for any real reply: the
strict parse accepts it, the mode, the model and the cost are the reply's own, and the pick is the
class the question is about.

Record again, from the root of the repository, when a reply no longer fits its test or the pinned
model moves:

```bash
node crates/mercury-judge/tests/fixtures/record.mjs
```

It reads the key from `deploy/compose/secrets/openrouter_api_key` and costs a few thousandths of a
cent. The failure-mode tests (a status, a timeout, a malformed or out-of-range body) build their
replies in code on purpose: a real endpoint does not misbehave on request.

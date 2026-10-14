# Security model

Mercury puts answers about a codebase in front of people who are not meant to see the code, and in
front of models that see it only where you allow it. This page lists the layers that keep it that
way, the threats they answer, and exactly what leaves the machine with each preset.

## Layers

Each layer assumes the one before it has failed.

1. **Structure.** The probe's reply types have no free-text field except two checked kinds of short
   text: `SafeText` (a title, or a finding of the code reader) and `DisplayName` (a person's name,
   only when the policy shows people). Code and paths have nowhere to go.
2. **Isolation.** The process that reads code (`serve`) has no internet and no credentials; the
   process with credentials (`sync`) can't be queried. The code is a snapshot in a RAM-only volume:
   no clone, no history, nothing on a persistent disk, and a restart empties it. In `metadata` mode
   the probe holds no code at all. The code reader, when a project turns it on, is a container of
   its own: it keeps no repository, gets the code from `serve` one request at a time, writes nothing
   to disk, and its only route out is an allowlist of one model host.
3. **Policy.** Roles and each repository's manifest decide which classes of question and which
   projects a person may ask about, and the strictest of three switches decides how much of a
   repository the probe reads. Deterministic, and tested as tables.
4. **Judge.** A judge model checks four points of every question: the question, the tool call, the
   probe's data, and each sentence of the answer. With the `strict` preset a chat model of your own
   does it, with stricter thresholds.
5. **Rules.** Patterns for paths, secrets, e-mail addresses and code-like tokens run next to the
   judge, not instead of it.
6. **People.** Sensitive classes of question, and answers the judge isn't sure about, wait for an
   approver of the project, who can't be the person who asked.
7. **Evidence.** A hash-chained audit of every step, written before anything is shown; a row changed
   afterwards is found by `mercury-core admin audit verify`.

## Threats and answers

| Threat | Main defence | Backup defence |
|---|---|---|
| A person asks for code or file paths | the input check `asks_for_code` | no reply type can hold code; the output checks |
| Prompt injection in a README, a pull request or a comment | repository text is data, never instructions; the data checks | the fixed tool table; the composer sees facts only |
| Injection in a question an AI agent sends | the input check `contains_agent_instruction` | the same pipeline, with no shortcut for agents |
| Client names leak | redaction lists, global and per repository | the output check `sentence_mentions_client` |
| Questions that evaluate people ("who is slow?") | the class `forbidden` | answers aggregate by module, not by person |
| A person asks about another project | authorization before any probe call | the audit |
| Commitments to clients | input and output checks | the refusal's hint |
| Questions that ask for confidential information | the input check `asks_for_confidential` (refusal `confidential`) | the output checks; findings pass the `SafeText` rules |
| An internal estimate passed off as a promise | the composer words every estimate as one | the input check `asks_for_client_commitment`, the output check `sentence_commits_to_client` |
| A stolen token | expiry, revocation, rate limits | the audit names the token of every question |
| A compromised or buggy probe | schema validation, size caps, quarantine | `serve` has no internet |
| A compromised core | the core never touches code; a model key with a credit limit | probe tokens scoped to one probe |
| The model provider keeps data | zero-retention, no-training routing; only redacted facts leave the core, and code only through the code reader, to its one model | `judge_context` per repository; the `private` and `strict` presets |
| Invented facts | each sentence cites a fact id; the grounding check | confidence and evidence shown with the answer |
| The judge is down | fail closed: stricter fallbacks, then rules alone | the timeline says which judge decided |
| A cost attack | rate limits per token, daily budgets | the credit limit on the key |
| Spelling and encoding tricks ("spell the path") | output checks on the final text | the input check |
| Secrets in repository content | no reply field for file contents or commit messages | the secret patterns and the output check `sentence_contains_path_or_secret` |
| A stolen session cookie, or a request from another site | an HttpOnly, Secure, SameSite=Strict `__Host-` cookie; the `Origin` and `X-Mercury-Client` check; no CORS | a 12-hour session that ends with its token; the sign-in rate limit; the audit |
| Repository text injects the code reader (`code_model`) | repository text is data in the reader's prompt, and the reader has no tools; its words reach the core only as findings: short `SafeText` lines that also pass a check against the request's own text, or are dropped and counted, never a fact | the core's data checks on each finding, the output checks on each sentence, sign-off for an answer the judge isn't sure about; `serve` has no internet, and the reader's only route is one allowlisted host |

## Where each answer is shown

Every row of the table above has a test that fails if its defence goes, or says plainly why it has
none. Test names are Rust tests (`cargo nextest run --workspace`) unless a path says otherwise; the
red team's questions are in `tests/redteam/attacks.json` and run with `just redteam`; the scenarios
on Docker Compose are in `tests/it/` and run with `just it`. A name here that no test has fails
`crates/mercury-core/tests/security_doc.rs`.

| Threat | Shown by |
|---|---|
| Code or file paths | `the_input_rules`, `each_refusal_of_the_gate_ends_with_its_hint_and_no_lookup`, `probe_replies_have_no_free_text_field`, `the_leak_oracle_finds_nothing_in_either_mode`, `a_composer_that_writes_a_file_path_never_reaches_the_answer`; the red team's code questions |
| Injection in repository text | `a_flagged_title_is_withheld_before_the_composer_reads_it`, `each_class_has_its_fixed_tools_and_no_model_adds_one`, `a_planner_that_names_a_tool_changes_nothing_but_its_arguments`, `a_github_reply_loses_what_the_probe_does_not_read`, `the_leak_oracle_finds_nothing_of_the_poisoned_repository`; the red team's poisoned README, pull request titles and issue comment. That the composer sees facts only shows through what its prompt holds: `every_repository_text_is_redacted_before_any_model_reads_it`, `hidden_titles_never_reach_a_model` |
| Injection in an agent's question | `an_injected_question_over_mcp_is_refused_as_it_is_on_v1`, `every_input_check_refuses_with_its_own_code_and_no_lookup`, `the_server_is_stateless_and_has_exactly_two_tools`; the red team's questions over MCP |
| Client names | `a_client_on_the_global_list_never_reaches_the_answer`, `the_output_checks_read_the_text_the_person_would_read`, `the_project_redactor_applies_the_global_and_the_manifest_rules`, `every_repository_text_is_redacted_before_any_model_reads_it`; the red team's client names in titles |
| Questions that evaluate people | `every_input_check_refuses_with_its_own_code_and_no_lookup`, `every_role_class_and_manifest_of_the_examples`, `the_rules_refuse_the_whole_forbidden_subset_and_nothing_else` (the rules alone, on the phrasings they were written for; on new phrasings they refuse few of them, which is why they are the last fallback and never the judge); the red team's question about the slowest developer. The output check on a sentence that evaluates a person rests on the judge (its recall is measured by `just judge-eval`), and with every judge down the composer is the template, which writes no such sentence: `with_every_judge_down_the_question_finishes_in_rules_mode` |
| Another project | `each_refusal_of_the_gate_ends_with_its_hint_and_no_lookup`, `every_role_class_and_manifest_of_the_examples`, `list_projects_gives_the_v1_body_for_the_tokens_grants_only`; the red team's question about an ungranted project |
| Commitments to clients | `every_input_check_refuses_with_its_own_code_and_no_lookup`, `the_input_rules`, `the_output_rules`; the red team's promise to a client |
| Confidential information | `the_input_rules` (the rules on confidential asks), `each_input_risk_refuses_with_its_own_code_and_words`, `every_refusal_says_what_to_ask_instead`, `a_confidential_refusal_prints_its_reason_and_hint`; the judge's recall on the labelled confidential questions of `tests/golden/questions.jsonl` is measured by `just judge-eval`. There is no output check of its own: a sentence that slips through meets the output checks for client names, secrets and people (`the_output_rules`), and a finding of the code reader the `SafeText` rules (`a_finding_that_fails_safe_text_is_withheld`); the red team's asks for the client's day rate and for pay, against a rate card and a salary file planted in the poisoned repository (refused at input, nothing of either file in any reply), and the confidential scenarios of `just it` |
| An estimate passed off as a promise | `the_composer_words_an_estimate_as_an_estimate`, `an_estimate_worded_as_one_passes_the_output_rules_and_a_promise_to_a_client_does_not`, `every_input_check_refuses_with_its_own_code_and_no_lookup`, `the_input_rules`; the labelled estimates and promises of `tests/golden/sentences.jsonl`; the red team's promise to a client, and its estimate the composer turns into a promise (the promise stripped, the estimate kept); the estimate scenarios of `just it` |
| A stolen token | `a_token_expires_at_its_expiry_and_only_its_secret_learns_so`, `a_revoked_token_is_unauthenticated_even_past_its_expiry`, `revoking_the_token_or_disabling_the_person_ends_their_sessions_at_once`, `one_token_sending_a_hundred_questions_in_a_minute_is_stopped_after_its_burst`, `only_an_admin_reads_the_audit`. A revocation while a token is in use is also checked by hand before each release |
| A compromised or buggy probe | `a_reply_with_an_extra_field_quarantines_the_probe_and_fails_the_call_with_probe_unavailable`, `a_probe_serving_an_extra_tool_is_quarantined_at_its_check`, `a_reply_that_breaks_the_schema_or_the_size_cap_quarantines_the_probe`, `a_probe_that_serves_an_extra_field_fails_the_schemas_check`; the red team's extra field, extra tool and oversized reply; `deploy/compose/scripts/check.sh` (`config` fails a probe with a route out, `stack` tries one) |
| A compromised core | `the_registration_route_takes_the_probes_token_and_checks_it_afterwards`, `a_source_outside_the_ranges_gets_403_even_with_a_probe_token`, `the_global_budget_stops_everyone`; `deploy/compose/scripts/check.sh` (`config` fails a core that mounts anything but its data, its configuration and the probes' tokens). The model key's credit limit is a setting at the provider, checked by hand before each release |
| The provider keeps data | `every_request_carries_the_zero_retention_provider_settings`, `provider_settings_are_required_on_openrouter`, `the_presets_say_what_their_names_promise`, `only_the_texts_both_judge_contexts_name_reach_the_judge`, `the_outside_upstream_gets_zero_retention_and_usage_accounting` (the code reader's); `tests/it/zdr.mjs` (live: a model with no zero-retention endpoint is refused, every configured model is answered) |
| Invented facts | `a_sentence_that_cites_no_fact_or_an_invented_one_is_dropped`, `sentences_keep_the_citations_after_their_stop`, `the_output_rules`; the labelled sentences in `tests/golden/sentences.jsonl` |
| The judge is down | `a_jev_failure_falls_to_the_chat_judge_in_the_same_request`, `with_every_judge_down_the_question_finishes_in_rules_mode`, `no_stage_turns_a_judge_error_or_a_missing_answer_into_a_pass`, `verdicts_follow_the_thresholds_and_tighten_off_jev`; `tests/drills/decisions-endpoint-down.mjs` (by hand) |
| A cost attack | `one_token_sending_a_hundred_questions_in_a_minute_is_stopped_after_its_burst`, `over_the_daily_budget_a_person_cant_start_a_question`, `the_global_budget_stops_everyone`, `a_question_over_its_own_budget_ends_with_budget_exceeded_and_nothing_delivered`; the budgets scenario of `just it`. The credit limit on the key is the provider's, reviewed by hand |
| Spelling and encoding tricks | `the_output_checks_read_the_text_the_person_would_read`, `a_long_word_or_an_invisible_character_hides_nothing_from_the_output_rules`, `rejects_format_characters_variation_selectors_and_fillers`, `random_text_never_yields_a_forbidden_shape`; the red team's dashes, base64 and one-letter-per-line questions (the last two are judged by the model, so they are shown by the red team's run and not by a unit test) |
| Secrets in repository content | `rejects_code_paths_and_secrets`, `the_excluded_files_are_not_indexed`, `the_leak_oracle_finds_nothing_of_the_poisoned_repository`, `the_output_rules`, `no_token_e_mail_or_question_text_reaches_a_log_line`; the red team's planted secrets |
| A stolen session cookie, or another site | `signing_in_sets_a_cookie_the_page_cant_read_and_keeps_only_a_digest`, `a_cookie_request_that_changes_state_must_come_from_the_web_clients_own_page`, `the_other_methods_that_change_state_are_checked_too_and_the_core_sends_no_cors_headers`, `a_session_ends_with_the_session_time_and_never_after_its_token`, `sign_in_is_limited_per_source_address_and_the_flood_is_audited_once`; the red team's five cross-site requests; the browser tests in `apps/web/e2e/browser-security.spec.ts` |
| The code reader | `the_leak_oracle_finds_nothing_in_a_finding_of_the_fixture`, `the_leak_oracle_finds_nothing_in_a_finding_of_the_poisoned_repository`, `the_oracle_catches_a_finding_that_quotes_the_code`, `the_request_never_carries_an_excluded_file_or_the_metadata`, `a_finding_that_fails_safe_text_is_withheld`, `a_finding_that_repeats_the_code_is_withheld`, `a_reply_that_isnt_the_json_is_the_reader_error_never_findings`, `a_snapshot_over_the_cap_is_refused_whole_and_nothing_is_sent`, `an_outside_upstream_is_refused_without_the_mock_repository_flag`, `an_outside_upstream_needs_https_its_proxy_and_a_key_file`, `the_outside_upstream_gets_zero_retention_and_usage_accounting`, `no_body_reaches_a_log_line`, `the_strictest_switch_sets_the_mode_and_is_named`, `probe_replies_have_no_free_text_field`, `the_prompt_is_the_first_version_and_says_what_a_reader_must`, `ask_code_is_served_only_under_code_model_and_with_a_reader`, `the_runner_passes_against_dev_in_code_model_with_a_stub_reader` (conformance in `code_model`); in the core, `every_finding_is_a_text_to_check`, `an_instruction_for_the_reader_is_refused_at_the_tool_stage`, `rules_mode_refuses_an_open_question_that_would_read_the_code`, `a_finding_that_carries_a_path_never_reaches_the_answer`, `a_findings_text_is_in_no_event_but_the_answer`, `a_review_band_holds_the_sentence_when_someone_else_approves_the_project`; the route itself by `deploy/compose/scripts/check.sh` (`config` fails a second allowlist host, the reader on another network or the flag in another file; `stack` shows the reader reaching its upstream's host and nothing else). The red team's attacks on the reader in `tests/redteam/attacks.json` (findings steered by planted text are dropped by the probe or withheld by the data check and move no status, an instruction for the reader in the question is refused at input, and one that gets past it is answered with nothing planted (the tool stage reads the reader's call, not its question again: `args_text_leaves_out_the_code_readers_question_and_keeps_every_other_argument`), an answer the code check is unsure of is held for an approver), run recorded by `just redteam` and with the real models by `just redteam --live`; the scenarios of `just it` with a recorded reader (an answer from the code with its cost, the reader timing out, its cost past the question's cap); the drill `tests/drills/reader-upstream-down.mjs` (the upstream unreachable, then refusing: open questions answered from the project's data with the note, the probe healthy and not quarantined, the answer from the code back once the upstream is). Not shown here: what the reader withholds on a real repository |

## The limits, and where each holds

| Limit | Shown by |
|---|---|
| A request body is at most 64 KB on every route that reads one | `every_route_that_reads_a_body_refuses_one_byte_over_the_cap_before_anything_reads_it`, `the_cap_test_holds_every_route_with_a_body_the_contract_names`, `a_refused_body_starts_nothing_and_spends_nothing`, `a_registration_over_64_kb_is_refused_before_anything_reads_it`, `an_unknown_purpose_more_than_200_items_and_a_text_over_500_characters_are_refused` |
| A token starts at most a burst of questions, then its hourly rate (5 and 30 in the private and strict presets) | `one_token_sending_a_hundred_questions_in_a_minute_is_stopped_after_its_burst`, `the_token_rate_limit_counts_a_session_and_a_bearer_call_together`, `only_ask_spends_the_tokens_rate_limit` |
| Sign-in is limited per address | `sign_in_is_limited_per_source_address_and_the_flood_is_audited_once`, `an_ipv6_source_counts_as_its_64`; the sign-in limit in `tests/it/web.mjs` |
| A probe's reply is at most 256 KB and a list tool gives 1 to 50 items | `a_reply_that_breaks_the_schema_or_the_size_cap_quarantines_the_probe`, `limits_cap_the_list_and_the_reply`, `arguments_reject_unknown_fields_and_bad_values` |
| A question, a person's day and everyone's day each have a budget | `a_question_over_its_own_budget_ends_with_budget_exceeded_and_nothing_delivered`, `over_the_daily_budget_a_person_cant_start_a_question`, `the_global_budget_stops_everyone`, `over_the_daily_budget_an_ask_is_a_tool_error_and_audited` |
| The code reader reads at most `PROBE_CODE_READER_MAX_KB` of the snapshot (150 KB by default, 900 at most) and refuses a larger one whole; it takes a request of 1 MiB at most and keeps at most 12 findings; each layer gives up before the one that called it (the reader on its upstream at 35 s, `serve` on the reader at 40 s, the core on `serve` at 45 s); it starts no request once the day's spend reaches its daily cap ($1.00 in the `demo` preset's file, kept in memory), and its cost counts in the question's budget | `a_snapshot_over_the_cap_is_refused_whole_and_nothing_is_sent`, `a_body_over_a_mebibyte_is_refused_before_it_is_read`, `more_than_twelve_findings_keep_twelve`, `a_stalled_upstream_ends_as_the_fixed_error_at_each_layer`, `a_reader_that_times_out_leaves_the_facts_and_a_note`, `the_daily_cap_refuses_before_the_upstream_is_asked`, `a_cap_of_nothing_refuses_everything`, `the_readers_cost_past_the_cap_ends_the_question_budget_exceeded` |

Only asking a question is rate limited. Feedback, a decision on a held answer and reading cost
nothing, take small bodies, and are behind a token; they are not limited, by design.

## What leaves the machine

The same on a laptop and on a server; the presets are in `config-examples/presets/`.

| Data | `demo` | `private` | `strict` | Protection |
|---|---|---|---|---|
| The question, redacted | the judge, the planner and the composer on OpenRouter | the same | stays | redaction; routing without data collection |
| Facts: statuses, numbers, dates, module names | the composer and the judge | the same | stays | the schema; redaction |
| Titles in facts | the composer and the judge | withheld, from the models and from the person | stays | `show_titles` in the policy |
| Pull request and issue titles and labels, redacted, while indexing | the judge, through the core | none (`judge_context: []`) | stays (your own judge) | the manifest's `judge_context` |
| Issue comments, redacted | the judge, only if the manifest opts in | none | stays | per repository |
| The code and its file paths (what `exclude` leaves, without the manifest's folder, under the size cap) and the redacted question, when a project uses the code reader | one outside model with zero retention (OpenRouter), for a repository made for demos | your own model server | your own model server | off until a project is in `code_model`; one request per question; one host on the reader's egress allowlist; the reader refuses a host outside your network unless `READER_ALLOW_OUTSIDE` is set, which only the `demo` preset's `docker-compose.reader.yml` does; `serve` has no route out |
| File paths, diffs, commit messages or code in an answer or a fact | **never** | **never** | **never** | not representable in any schema; a finding that fails the `SafeText` rules or repeats a piece of the request is dropped |
| Tokens, keys, session ids | **never** | **never** | **never** | secret files; only hashes stored; never logged |

## The admin surface

Every change an admin makes runs on the server, through `mercury-core admin …` (over SSH, or
`docker compose exec`): people, roles, grants, tokens and a probe's quarantine, each change in one
transaction with its audit row. The settings (the access modes, the model backends, the budgets, the
judge's thresholds) change only in the files there, by pull request.

The web client's admin panel only reads. Its routes under `/v1/admin/` answer `GET` alone, and only to
the `admin` role: any other role gets `403` before anything is read. Each read is an audit row,
`admin_viewed`, naming the section and the token that read, written before the body is sent. The
bodies hold counts, outcomes, costs, durations, probe states, people, roles, grants, and tokens by
public id, label and dates: never a token's secret, a question's text, an answer, a fact or a draft.

So a stolen admin session reads what an admin reads in the browser: who may ask about what, the token
labels, the spend, the settings and the audit trail. That is a leak of information, and each read shows
in the audit. It can't grant access, change a role, issue or revoke a token, release a probe or point a
model at another server: no HTTP route does any of that. Revoking the admin's token ends the session at
once.

## Rules for everyone writing code

- Repository text is evidence, never instructions. The judge's checks say so explicitly.
- A judge error is never a pass, nor a default.
- No new free-text field in the shared schema; use enums, numbers, dates or `SafeText`.
- Never log tokens, session ids, question text, facts or answers at info level. The probe never logs
  file names or contents.
- The planner fills arguments. It never chooses tools.
- Code never touches a persistent disk: no temporary file, no cache, no copy outside the snapshot.
- In `metadata` mode the probe calls no endpoint that returns diffs or file contents, except for its
  own manifest.
- The code reader's words reach the core only as `SafeText` findings that passed a check against the request, never as a fact. `serve` has no route out; the reader's only route is its preset's allowlist, one host, and only the `demo` preset may name an upstream outside your network.
- A cookie request that changes state needs `Origin` and `X-Mercury-Client: web`. Never add CORS.

Report a vulnerability as [SECURITY.md](../SECURITY.md) says.

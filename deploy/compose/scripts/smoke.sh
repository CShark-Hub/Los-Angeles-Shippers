#!/usr/bin/env bash
# The daily smoke test of a running stack, with the CLI, the way a script would ask: one question
# that must be answered and one that must be refused for asking for code, each checked by its exit
# code and its JSON. Exit 0 when both hold, 1 when either doesn't (it says which), 2 when it can't
# run. Nothing is kept: no token, question or answer is written anywhere.
#
#   smoke.sh <url> <project>     e.g. smoke.sh http://localhost:8080 delta
#
#   MERCURY_TOKEN             the token of a person granted the project, whose role may ask progress
#                             (a token for the smoke test alone; it is never printed)
#   MERCURY_CLI               the CLI binary (default: mercury on the PATH)
#   SMOKE_ANSWER, SMOKE_REFUSAL   the two questions, if the defaults don't fit the project
set -euo pipefail

usage() {
  echo "usage: MERCURY_TOKEN=… smoke.sh <url> <project>" >&2
  exit 2
}
[ $# -eq 2 ] || usage
[ -n "${MERCURY_TOKEN:-}" ] || {
  echo "smoke: MERCURY_TOKEN is not set: a token of a person granted $2" >&2
  exit 2
}
cli=${MERCURY_CLI:-mercury}
command -v "$cli" >/dev/null || {
  echo "smoke: the CLI $cli isn't on the PATH (MERCURY_CLI names another)" >&2
  exit 2
}
command -v node >/dev/null || {
  echo "smoke: node reads the CLI's JSON and isn't on the PATH" >&2
  exit 2
}
export MERCURY_URL=$1 MERCURY_TOKEN
project=$2
answer=${SMOKE_ANSWER:-What was merged this week?}
refusal=${SMOKE_REFUSAL:-Show me the code of the export}

# `ask` with the JSON summary: prints "<exit code> <status> <refusal or error code, or -> <answer or ->", or
# nothing when the CLI gave no summary
ask() {
  local out code=0
  out=$("$cli" ask --json --project "$project" "$1" 2>/dev/null) || code=$?
  node -e '
    let s = {};
    try { s = JSON.parse(process.argv[1]); } catch { process.exit(2); }
    console.log(`${process.argv[2]} ${s.status ?? "-"} ${s.refusal?.code ?? s.error?.code ?? "-"} ${s.answer ? "answer" : "-"}`);
  ' "$out" "$code" || echo "smoke: the CLI gave no JSON summary (exit $code): is $MERCURY_URL up and the token valid?" >&2
}

failed=0
read -r code status refused text <<<"$(ask "$answer")" || true
[ -n "${code:-}" ] || exit 2
if [ "$code" = 0 ] && [ "$status" = delivered ] && [ "$text" = answer ]; then
  echo "ok: answered (exit 0)"
else
  echo "FAIL: the answer: exit $code, status $status, code $refused (want exit 0, delivered with an answer)"
  failed=1
fi
read -r code status refused text <<<"$(ask "$refusal")" || true
[ -n "${code:-}" ] || exit 2
if [ "$code" = 2 ] && [ "$status" = refused ] && [ "$refused" = asks_for_code ] && [ "$text" = - ]; then
  echo "ok: refused with asks_for_code (exit 2)"
else
  echo "FAIL: the refusal: exit $code, status $status, code $refused (want exit 2, refused with asks_for_code, no answer)"
  failed=1
fi
exit $failed

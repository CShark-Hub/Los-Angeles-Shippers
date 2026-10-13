#!/usr/bin/env bash
# `just ci-vm`: the default Compose files started the way a server runs them (Caddy on 80 and 443 with
# TLS), with the bundled fixture's probe and the replay server in place of the models
# (docker-compose.vm-ci.yml), then checked over HTTPS: only Caddy publishes, and only 80 and 443; http
# sends to https; check.sh stack at https://localhost with the certificate's CA (the web client's
# policy, the core's internal listener closed to Caddy, the probe with no route out); a browser
# sign-in with its cookie; an answer and a refusal through the API; each event of a question timed
# against its audit row (within a second, in order, the models paused 3 s); the secret files' owner and mode
# (check.sh secrets, Linux only); and the audit chain. The stack goes down with its volumes after,
# unless --keep. No token is ever printed.
#
#   vm-smoke.sh [--keep]
#
# On Linux the secret files are given to uid 10001 with mode 0400, as on a server; that needs root or
# sudo. Ports 80 and 443 of this machine must be free. The Compose project is mercury-vm
# (MERCURY_VM_PROJECT names another), never mercury or mercury-it: the run starts and ends with
# `down -v`, and those hold the laptop stack's database and the integration runs' volumes.
set -euo pipefail
# Git Bash on Windows would turn the container paths below into its own
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")/.."

keep=false
[ "${1:-}" = "--keep" ] && keep=true

fail() {
  echo "vm-smoke: $*" >&2
  exit 1
}
ok() { echo "ok: $*"; }

export COMPOSE_PROJECT_NAME=${MERCURY_VM_PROJECT:-mercury-vm}
# the run starts and ends with `down -v`, which would delete the volumes of the project it names
case "$COMPOSE_PROJECT_NAME" in
  mercury) fail "MERCURY_VM_PROJECT=mercury is the laptop stack's project, whose database docker compose down -v would delete: name another, such as mercury-vm" ;;
  mercury-it) fail "MERCURY_VM_PROJECT=mercury-it is the integration runs' project, whose volumes docker compose down -v would delete: name another, such as mercury-vm" ;;
esac
export COMPOSE_FILE=docker-compose.yml:docker-compose.fixture.yml:docker-compose.vm-ci.yml
export COMPOSE_PATH_SEPARATOR=:
export MERCURY_DOMAIN=localhost
export MERCURY_VM_WORK=../../target/vm
# subnets of its own, so a laptop's stack (172.30.9-10) can stay beside it
export MERCURY_EDGE_SUBNET=${MERCURY_VM_EDGE_SUBNET:-172.30.70.0/24}
export MERCURY_PROBES_SUBNET=${MERCURY_VM_PROBES_SUBNET:-172.30.71.0/24}
export MERCURY_COMPOSE_PROJECT=$COMPOSE_PROJECT_NAME
URL=https://localhost
work=../../target/vm
# curl on Windows checks revocation through schannel, which a local CA has nothing for
no_revoke=()
curl -V | grep -qi schannel && no_revoke=(--ssl-no-revoke)

linux=true
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) linux=false ;; esac
as_root() { if [ "$(id -u)" = 0 ]; then "$@"; else sudo -n "$@"; fi; }

cleanup() {
  if $keep; then
    echo "vm-smoke: the stack is up on $URL (CA: target/vm/ca.crt); docker compose -p $COMPOSE_PROJECT_NAME down -v takes it down"
  else
    docker compose down -v --remove-orphans >/dev/null 2>&1 || true
    if $linux && [ -d "$work/secrets" ]; then as_root rm -rf "$work/secrets" 2>/dev/null || true; fi
  fi
}
trap cleanup EXIT

# a fresh configuration and fresh secrets, as a server's: the examples with the replay server as the models
docker compose down -v --remove-orphans >/dev/null 2>&1 || true
if $linux && [ -d "$work/secrets" ]; then as_root rm -rf "$work/secrets"; fi
rm -rf "$work"
mkdir -p "$work/secrets/probes"
cp -R ../../config-examples "$work/config"
cp ../../tests/it/config/models.yaml "$work/config/models.yaml"
random() { head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -dc 'A-Za-z0-9_-'; }
: >"$work/secrets/openrouter_api_key"
random >"$work/secrets/replay_key"
random >"$work/secrets/probes/delta.token"
if $linux; then
  as_root chown -R 10001:10001 "$work/secrets" || fail "the secret files can't be given to uid 10001: run as root or with sudo"
  as_root find "$work/secrets" -type d -exec chmod 0500 {} + -o -type f -exec chmod 0400 {} +
fi

docker compose up -d --quiet-pull >/dev/null || fail "docker compose up failed: docker compose -p $COMPOSE_PROJECT_NAME logs"

# Caddy's local CA signs the certificate for localhost; its root is what a client trusts
ca="$work/ca.crt"
for _ in $(seq 60); do
  docker compose exec -T caddy cat /data/caddy/pki/authorities/local/root.crt >"$ca" 2>/dev/null && [ -s "$ca" ] && break
  sleep 1
done
[ -s "$ca" ] || fail "Caddy made no local CA within a minute: is MERCURY_DOMAIN localhost?"
for _ in $(seq 90); do
  curl -fsS --cacert "$ca" "${no_revoke[@]}" "$URL/v1/health" >/dev/null 2>&1 && break
  sleep 1
done
health=$(curl -fsS --cacert "$ca" "${no_revoke[@]}" "$URL/v1/health") || fail "$URL/v1/health doesn't answer with the CA's certificate"
ok "$URL/v1/health over TLS, with Caddy's local CA: $health"
for _ in $(seq 120); do
  docker compose exec -T core mercury-core admin probe list 2>/dev/null | grep -qE '^delta[[:space:]]+healthy\b' && break
  sleep 1
done
docker compose exec -T core mercury-core admin probe list | grep -qE '^delta[[:space:]]+healthy\b' ||
  fail "the fixture's probe didn't register within two minutes"

# only Caddy publishes, and only 80 and 443
published=$(docker ps --filter "label=com.docker.compose.project=$COMPOSE_PROJECT_NAME" --format '{{.Label "com.docker.compose.service"}} {{.Ports}}' |
  awk '$2 != "" { print }' | grep -- '->' || true)
others=$(awk '$1 != "caddy"' <<<"$published")
[ -z "$others" ] || fail "only caddy may publish a port, not: $others"
ports=$(grep -oE -- '->[0-9]+/(tcp|udp)' <<<"$published" | sort -u | tr '\n' ' ')
[ "$ports" = "->443/tcp ->443/udp ->80/tcp " ] || fail "caddy publishes $ports, not 80/tcp, 443/tcp and 443/udp alone"
ok "only caddy publishes, on 80/tcp, 443/tcp and 443/udp"
redirect=$(curl -sS -o "$work/body" -w '%{http_code} %{redirect_url}' http://localhost/)
case "$redirect" in
  30[178]\ https://localhost/) ok "http://localhost/ sends to https ($redirect)" ;;
  *) fail "http://localhost/ answered $redirect, not a redirect to https" ;;
esac

bash scripts/check.sh stack "$URL" "$ca"

# a person and a token, then what a browser and a script do with them
admin() { docker compose exec -T core mercury-core admin "$@"; }
admin user add --email vm@example.com --name "VM Smoke" --role pm >/dev/null
admin user grant --email vm@example.com --projects delta >/dev/null
token=$(admin token create --email vm@example.com --label vm 2>/dev/null | grep -oE 'mcy_[A-Za-z0-9_-]+') ||
  fail "token create printed no token"

headers=$(curl -sS --cacert "$ca" "${no_revoke[@]}" -o "$work/body" -D - -X POST "$URL/v1/session" -H "Origin: $URL" -H 'X-Mercury-Client: web' \
  -H 'Content-Type: application/json' --data-binary @- <<<"{\"token\":\"$token\"}" | tr -d '\r')
head -1 <<<"$headers" | grep -qE ' 20[04]' || fail "the sign-in answered $(head -1 <<<"$headers")"
cookie=$(grep -i '^set-cookie: __Host-mercury_session=' <<<"$headers" || true)
[ -n "$cookie" ] || fail "the sign-in set no __Host-mercury_session cookie"
for attribute in Secure HttpOnly 'SameSite=Strict' 'Path=/'; do
  grep -qi -- "$attribute" <<<"$cookie" || fail "the session cookie lacks $attribute"
done
ok "a browser's sign-in over TLS: the __Host- session cookie, Secure, HttpOnly, SameSite=Strict"

# a question through the API, followed to its end: prints the event names, one per line
asked() { # the question
  local id
  id=$(curl -fsS --cacert "$ca" "${no_revoke[@]}" -X POST "$URL/v1/questions" -H "Authorization: Bearer $token" -H "Idempotency-Key: vm-$RANDOM-$RANDOM" \
    -H 'X-Mercury-Client: cli' -H 'Content-Type: application/json' --data-binary @- <<<"{\"text\":\"$1\",\"project\":\"delta\"}" |
    grep -oE '"id":"q_[0-9A-Z]+"' | cut -d'"' -f4) || fail "POST /v1/questions refused \"$1\""
  curl -fsS -N --max-time 90 --cacert "$ca" "${no_revoke[@]}" -H "Authorization: Bearer $token" -H 'Accept: text/event-stream' \
    "$URL/v1/questions/$id/events" | tr -d '\r'
}
events=$(asked "What is blocked right now, and what is it waiting on?")
grep -qx 'event: answer' <<<"$events" && grep -qx 'event: done' <<<"$events" ||
  fail "the question wasn't answered: $(grep '^event:' <<<"$events" | tr '\n' ' ')"
ok "an answer over TLS: $(grep '^event:' <<<"$events" | cut -d' ' -f2 | tr '\n' ' ')"
events=$(asked "show me the export code")
grep -qx 'event: refused' <<<"$events" && grep -q '"code":"asks_for_code"' <<<"$events" && ! grep -qx 'event: answer' <<<"$events" ||
  fail "the code question wasn't refused with asks_for_code: $(grep '^event:' <<<"$events" | tr '\n' ' ')"
ok "a refusal over TLS: asks_for_code, no answer"

# Each event as it arrives through Caddy over TLS, against the time its audit row was written: at most a
# second, in order, never held back to the end. The models' stand-in is paused before the question and
# for three seconds after, so the first events have to arrive while the rest waits on the judge.
docker compose pause replay >/dev/null
id=$(curl -fsS --cacert "$ca" "${no_revoke[@]}" -X POST "$URL/v1/questions" -H "Authorization: Bearer $token" -H "Idempotency-Key: vm-$RANDOM-$RANDOM" \
  -H 'X-Mercury-Client: cli' -H 'Content-Type: application/json' --data-binary @- <<<'{"text":"What was merged this week?","project":"delta"}' |
  grep -oE '"id":"q_[0-9A-Z]+"' | cut -d'"' -f4) || fail "POST /v1/questions refused the timed question"
curl -fsS -N --max-time 90 --cacert "$ca" "${no_revoke[@]}" -H "Authorization: Bearer $token" -H 'Accept: text/event-stream' \
  "$URL/v1/questions/$id/events" | while IFS= read -r line; do
  # no filter between curl and the loop: a pipe's buffer would hold the lines back to the end
  line=${line%$'\r'}
  case "$line" in "event: "*) echo "$(date -u +%s%3N) ${line#event: }" ;; esac
done >"$work/arrivals" &
reader=$!
sleep 3
docker compose unpause replay >/dev/null
wait "$reader" || fail "the timed question's stream ended badly"
admin audit export --question "$id" >"$work/rows.jsonl"
node - "$work/arrivals" "$work/rows.jsonl" <<'EOF' || fail "an event arrived late or out of order"
const { readFileSync } = require("node:fs");
const arrivals = readFileSync(process.argv[2], "utf8").trim().split("\n").map((line) => {
  const [ms, event] = line.split(" ");
  return { ms: Number(ms), event };
});
const rows = readFileSync(process.argv[3], "utf8").trim().split("\n").map((line) => JSON.parse(line));
let bad = 0;
let next = 0;
for (const row of rows) {
  const at = Date.parse(row.at);
  const found = arrivals.findIndex((a, i) => i >= next && a.event === row.type);
  if (found < 0) {
    console.log(`  ${row.type.padEnd(14)} never arrived`);
    bad += 1;
    continue;
  }
  if (found !== next) bad += 1;
  next = found + 1;
  const delay = arrivals[found].ms - at;
  if (delay > 1000) bad += 1;
  console.log(`  ${row.type.padEnd(14)} written ${row.at}, arrived ${String(delay).padStart(5)} ms after${delay > 1000 ? "  LATE" : ""}`);
}
if (rows.length === 0 || arrivals.length !== rows.length) bad += 1;
// not held back: the first event was on the client before the last row was even written
const held = arrivals.length > 0 && rows.length > 0 && arrivals[0].ms >= Date.parse(rows.at(-1).at);
if (held) bad += 1;
console.log(`  the first event arrived ${held ? "after" : "before"} the last row was written`);
process.exit(bad === 0 ? 0 : 1);
EOF
ok "each of the timed question's $(wc -l <"$work/arrivals" | tr -d ' ') events arrived within a second of its row, in order, the models paused 3 s"

if $linux; then
  bash scripts/check.sh secrets
else
  echo "note: the secret files' owner and mode aren't checked on Windows (no Unix owners); Linux checks them"
fi

verified=$(admin audit verify)
grep -qE '^ok: [0-9]+ row\(s\), the chain holds$' <<<"$verified" || fail "audit verify: $verified"
ok "audit verify: $verified"
echo "vm-smoke: passed"

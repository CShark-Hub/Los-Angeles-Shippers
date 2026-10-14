#!/usr/bin/env bash
# `just demo-local`: a laptop stack for your own repositories, started from the clone. The core and
# Caddy on http://localhost:8080 with the web client's build, a sync + serve pair for each real repository
# (docker-compose.repos.yml: its committed block is a template, so this laptop's own blocks go there
# or in a file of its own, never into a commit), and the poisoned repository's probe beside them
# (docker-compose.poison.yml, the project `poison`). The images and the web build (with the demo
# look) are made by the recipe before this runs.
#
# It prepares what a person would otherwise forget and stops, saying what to do, when something that
# only a person can supply is missing: a GitHub token per repository, the template left in place, a
# configuration the core would refuse or that lacks a project a probe serves. It never writes a GitHub
# token, and never overwrites a file of the laptop's own (configuration, secrets). Once the core
# answers, it waits for every probe to register healthy.
#
#   COMPOSE_FILE            the Compose files, colon-separated (the default is .env's: the base file
#                           and the repositories' file); docker-compose.local.yml and
#                           docker-compose.poison.yml are added when they are not in the list, so this
#                           laptop's repositories can live in a file of their own:
#                             COMPOSE_FILE=docker-compose.yml:docker-compose.mine.yml just demo-local
#                           or the bundled fixture stands in for them, with no GitHub token:
#                             COMPOSE_FILE=docker-compose.yml:docker-compose.fixture.yml just demo-local
#   MERCURY_PUBLIC_ORIGIN   the address a browser signs in from; from the environment or .env, else
#                           http://localhost:8080, which both of the demo's browser windows use
#
# The code reader (code_model, docker-compose.reader.yml) comes with a key: when secrets/reader_api_key
# holds one, the file is added and the probes are told where the reader is (MERCURY_CODE_READER_URL and
# MERCURY_CODE_READER_MODEL); the code of the projects whose access_max allows code_model then goes to
# OpenRouter, with zero data retention, through the reader's one-host proxy. Without a key there is no
# reader, and a project that asks for code_model runs code. The key is never written here. A COMPOSE_FILE
# that names the reader's file with no key fails.
set -euo pipefail
# Git Bash on Windows would turn the container paths below (/config) into its own
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")/.."
root=$(cd ../.. && { pwd -W 2>/dev/null || pwd; })

fail() {
  echo "demo-local: $*" >&2
  exit 1
}
note() { echo "demo-local: $*"; }

# the files, as -f arguments
if [ -n "${COMPOSE_FILE:-}" ]; then
  IFS=':' read -ra listed <<<"$COMPOSE_FILE"
else
  listed=(docker-compose.yml docker-compose.repos.yml)
fi
for overlay in docker-compose.local.yml docker-compose.poison.yml; do
  case " ${listed[*]} " in
    *" $overlay "*) ;;
    *) listed+=("$overlay") ;;
  esac
done
# the code reader: named by the person, or added when there is a key for it
reader=false
case " ${listed[*]} " in
  *" docker-compose.reader.yml "*)
    [ -s secrets/reader_api_key ] ||
      fail "docker-compose.reader.yml is in COMPOSE_FILE but secrets/reader_api_key is missing or empty: the demo's reader needs its OpenRouter key there, or the file comes out of COMPOSE_FILE"
    reader=true
    ;;
  *)
    if [ -s secrets/reader_api_key ]; then
      listed+=(docker-compose.reader.yml)
      reader=true
    fi
    ;;
esac
if $reader; then
  # what the probes' PROBE_CODE_READER_* lines read; the reader's own model is in its file
  export MERCURY_CODE_READER_URL=http://code-reader:7100
  export MERCURY_CODE_READER_MODEL=anthropic/claude-4.6-sonnet-20260217
fi
fixture=false
case " ${listed[*]} " in
  *" docker-compose.fixture.yml "*) fixture=true ;;
esac
files=()
for file in "${listed[@]}"; do
  [ -f "$file" ] || fail "$file isn't in deploy/compose: COMPOSE_FILE names it"
  files+=(-f "$file")
done
compose() { docker compose "${files[@]}" "$@"; }

# the web build the local file mounts
[ -f ../../apps/web/csp.caddy ] && [ -f ../../apps/web/build/index.html ] ||
  fail "apps/web has no build: run \`just demo-local\`, which builds it, not this script"

config=$(compose config) || fail "docker compose config failed for ${listed[*]}"

# the repositories: the template's repository is not one; the fixture needs none
if grep -q 'PROBE_GITHUB_REPO: your-org/' <<<"$config"; then
  fail "the repositories' file still holds the template (your-org/…). Copy its block for your repository and replace the names, in docker-compose.repos.yml or a file of your own named in COMPOSE_FILE, but don't commit it (a GitHub token, its Compose block and its project entry are the whole of onboarding it)"
fi
if $fixture; then
  note "the bundled fixture repository stands in for the real ones (the project delta, no GitHub token)"
else
  grep -q 'PROBE_GITHUB_REPO:' <<<"$config" ||
    fail "no repository's sync service is in ${listed[*]}: add a block for each, or run the fixture (COMPOSE_FILE=docker-compose.yml:docker-compose.fixture.yml)"
fi

# the secrets
mkdir -p secrets/probes
[ -e secrets/openrouter_api_key ] || : >secrets/openrouter_api_key
if [ ! -s secrets/openrouter_api_key ]; then
  note "secrets/openrouter_api_key is empty: the judge and the composer run on the rules and the template (the degraded mode), not on the models"
fi
for token in $(grep -oE 'secrets[\\/]github[\\/][A-Za-z0-9_.-]+\.token' <<<"$config" | tr '\\' '/' | sort -u); do
  [ -s "$token" ] || fail "$token is missing or empty: a read-only fine-grained GitHub token for that repository goes there"
done
for token in $(grep -oE 'secrets[\\/]probes[\\/][A-Za-z0-9_.-]+\.token' <<<"$config" | tr '\\' '/' | sort -u); do
  [ -s "$token" ] || head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -dc 'A-Za-z0-9_-' >"$token"
done

# the configuration: the demo preset the first time, the laptop's own after that
mkdir -p config
if [ -z "$(ls -A config)" ]; then
  cp -R ../../config-examples/presets/demo/. config/
  rm -f config/probe.yaml # the repository's manifest template, not part of the configuration
  if $fixture; then
    note "config/ was empty: the demo preset is in it, with the fixture's project delta"
  else
    note "config/ was empty: the demo preset is in it. Add each repository as a project in config/projects.yaml, then run this again"
    exit 1
  fi
fi
if $reader; then
  # the projects whose code may go to the reader: those the admin lets reach code_model
  sent=$(awk '/^  [A-Za-z0-9_-]+:/ { project = $1; sub(/:$/, "", project) } /^    access_max: *code_model/ { print project }' config/projects.yaml | tr '\n' ' ')
  note "the code reader is on: the code of ${sent:-no project (none has access_max: code_model) }goes to OpenRouter (zero data retention), through reader-egress, for a question that asks for it; every other project's code stays here"
else
  note "no secrets/reader_api_key: no code reader, code_model probes run as code"
fi
image=$(grep -m1 -oE 'image: [^ ]*/mercury-core:[^ ]+' <<<"$config" | cut -d' ' -f2)
docker run --rm --network none -v "$root/deploy/compose/config:/config:ro" "$image" admin config check --dir /config ||
  fail "the core would refuse config/ (the lines above say why)"
for project in $(grep -oE 'PROBE_PROJECT: [A-Za-z0-9_-]+' <<<"$config" | cut -d' ' -f2 | sort -u); do
  grep -qE "^  $project:" config/projects.yaml ||
    fail "config/projects.yaml has no project $project, which a sync service is set to read"
done
served=(poison)
if $fixture; then served+=(delta); fi
for project in "${served[@]}"; do
  grep -qE "^  $project:" config/projects.yaml ||
    fail "config/projects.yaml has no project $project, which a probe of ${listed[*]} serves: copy its block from config-examples/presets/demo/projects.yaml"
done

# the address a browser signs in from: both of the demo's windows are on this laptop, on localhost
origin=${MERCURY_PUBLIC_ORIGIN:-$(sed -n 's/^MERCURY_PUBLIC_ORIGIN=\([^ #]*\).*/\1/p' .env | tail -1)}
origin=${origin:-http://localhost:8080}
case "$origin" in
  http://localhost:* | http://127.0.0.1:*)
    note "MERCURY_PUBLIC_ORIGIN is $origin: sign in from this laptop's browser, the manager and the approver in two windows (two browser profiles)"
    ;;
  *) note "MERCURY_PUBLIC_ORIGIN is $origin: the core accepts a sign-in from that page only, so this laptop's windows open it too" ;;
esac

compose up -d --remove-orphans
for _ in $(seq 60); do
  if health=$(curl -fsS http://localhost:8080/v1/health 2>/dev/null); then
    echo
    note "up: $health on http://localhost:8080"
    break
  fi
  sleep 1
done
[ -n "${health:-}" ] ||
  fail "/v1/health doesn't answer on http://localhost:8080; see \`docker compose ps\` and \`docker compose logs core\` in deploy/compose"

# every probe registered and healthy: a probe registers as it starts, a sync probe after its first fetch
expected=$(grep -oE 'PROBE_PROJECT: [A-Za-z0-9_-]+' <<<"$config" | cut -d' ' -f2 || true)
expected=$(printf '%s\n' $expected "${served[@]}" | sort -u)
for _ in $(seq 120); do
  probes=$(compose exec -T core mercury-core admin probe list 2>/dev/null || true)
  waiting=()
  for project in $expected; do
    grep -qE "^$project[[:space:]]+healthy\b" <<<"$probes" || waiting+=("$project")
  done
  [ ${#waiting[@]} -gt 0 ] || break
  sleep 1
done
echo "$probes"
[ ${#waiting[@]} -eq 0 ] ||
  fail "not healthy after two minutes: ${waiting[*]}; see \`docker compose logs probe-<project>\` in deploy/compose"
compose ps --format 'table {{.Service}}\t{{.Status}}'
cat <<'EOF'

Next, on this laptop (deploy/compose):
  docker compose exec core mercury-core admin user add / user grant / token create     the demo people and their tokens; grant poison too
  bash scripts/smoke.sh http://localhost:8080 <project>                         the daily smoke: one answer and one refusal (MERCURY_TOKEN)
  bash scripts/backup.sh                                                        the nightly backup, 14 copies kept in ./backups
  bash scripts/check.sh stack                                                   the network checks
Open http://localhost:8080 in two browser windows: the manager's and the approver's (another profile).
EOF

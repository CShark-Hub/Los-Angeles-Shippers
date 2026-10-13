#!/usr/bin/env bash
# The deployment checks that exist so far.
#   check.sh config   the eight file combinations validate, only caddy publishes ports, no service
#                     is privileged or shares a namespace with the host or another container, and
#                     every service has a health check
#                     the probes and sync networks are internal, a probe that answers questions is on the
#                     probes network alone and a sync probe on the sync network alone, and the core mounts
#                     nothing but its data, its configuration and the probes' tokens (no code); where the
#                     code reader (code-reader) is in the combination, its route is the network's: it is on
#                     the probes and reader-out networks alone and mounts nothing, reader-egress is on
#                     reader-out and egress alone, no other service is on reader-out (internal), the
#                     allowlist holds exactly the host of the reader's READER_UPSTREAM_URL, and
#                     READER_ALLOW_OUTSIDE is set in no shipped file but docker-compose.reader.yml
#   check.sh stack [URL [CA]]
#                     on a running stack (just compose-up, the real repositories' pairs, or a server's
#                     at its https URL, with the CA file that signed its certificate when it isn't a
#                     public one; http://localhost:8080 by default):
#                     /v1/health answers through Caddy, and caddy gets 403 from the core's internal
#                     listener; the web client comes with its CSP header, on its own pages and on an
#                     unknown path, and the core's internal routes and any path under /mcp are not
#                     reachable through Caddy;
#                     then for every probe running, and at least one must: `serve` reaches
#                     the core on the probes network and has no route out; `sync` reaches
#                     api.github.com and codeload.github.com through the egress proxy and nothing else;
#                     its /repo is a tmpfs; and no file of its snapshot is on a named volume or anywhere
#                     in Docker's data directory; with code-reader running, every `serve` reaches its
#                     /health (the designed route) and can't reach reader-egress, and the reader reaches
#                     its upstream's host through reader-egress and nothing else (no key is sent, nothing
#                     is spent)
#   check.sh disk     the same promise on its own, as the script to run after a sync: the check can see a
#                     planted file (a positive control, on a scratch volume it removes), then, for every
#                     `sync` probe running, its /repo is a tmpfs volume that a stopped container does not
#                     keep, and no file of its snapshot is on a named volume or in Docker's data directory;
#                     with no `sync` probe (the fixture stack) the repository searched for is the fixture
#                     the stack serves. A stack that is not the project mercury: MERCURY_COMPOSE_PROJECT
#                     (`just it --keep` leaves one named mercury-it).
#   check.sh secrets  every secret file the Compose files name (COMPOSE_FILE, else .env's list) is there,
#                     owned by uid 10001 with mode 0400 and in folders only that user enters, and no
#                     secret's value is in `docker compose config` or, for a running stack
#                     (MERCURY_COMPOSE_PROJECT), in any of its containers' logs. A user who isn't root
#                     can't enter those folders, so the files are tested, read and stat'ed through
#                     `sudo -n` when it works; with neither, a file it can't reach fails the check.
#                     Linux only: Windows keeps no Unix owners or modes to check.
set -euo pipefail
# Git Bash on Windows would turn the container paths below (/proc/self, /repo) into its own
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")/.."

fixture=(-f docker-compose.yml -f docker-compose.fixture.yml -f docker-compose.local.yml)
repos=(-f docker-compose.yml -f docker-compose.repos.yml -f docker-compose.local.yml)
demo=(-f docker-compose.yml -f docker-compose.fixture.yml -f docker-compose.local.yml -f docker-compose.poison.yml)
demo_repos=(-f docker-compose.yml -f docker-compose.repos.yml -f docker-compose.local.yml -f docker-compose.poison.yml)
runs=(-f docker-compose.yml -f docker-compose.fixture.yml -f docker-compose.local.yml -f docker-compose.it.yml)
reader_fixture=(-f docker-compose.yml -f docker-compose.fixture.yml -f docker-compose.local.yml -f docker-compose.reader.yml)
reader_demo_repos=(-f docker-compose.yml -f docker-compose.repos.yml -f docker-compose.local.yml -f docker-compose.poison.yml -f docker-compose.reader.yml)

# The Compose project whose containers and volumes the checks on a running stack look at.
PROJECT=${MERCURY_COMPOSE_PROJECT:-mercury}

# Where `stack` reaches Caddy, and the CA file curl trusts for it (none: the system's).
URL=http://localhost:8080
CURL_TLS=()

# A running service's container in the project, by the labels Compose sets: nothing when it isn't running.
container_of() { # the service
  docker ps --filter "label=com.docker.compose.project=$PROJECT" --filter "label=com.docker.compose.service=$1" -q | head -1
}

fail() {
  echo "check: $*" >&2
  exit 1
}

# The services, other than caddy, that publish a port in `docker compose config`'s output.
publishers() {
  awk '
    /^services:/ { in_services = 1; next }
    /^[^ ]/ { in_services = 0 }
    in_services && /^  [^ ][^:]*:$/ { service = $1; sub(/:$/, "", service) }
    in_services && /^    ports:/ && service != "caddy" { print service }
  '
}

# The services that are privileged or share a namespace with the host or another container, in
# `docker compose config`'s output. Any network_mode but none counts: it replaces the networks a
# service is given, so `host` publishes every port and `service:core` borrows the core's route out.
unconfined() {
  awk '
    /^services:/ { in_services = 1; next }
    /^[^ ]/ { in_services = 0 }
    in_services && /^  [^ ][^:]*:$/ { service = $1; sub(/:$/, "", service) }
    in_services && /^    privileged: true$/ { print service " (privileged)" }
    in_services && /^    network_mode: / && $2 != "none" { print service " (network_mode " $2 ")" }
    in_services && /^    pid: / { print service " (pid " $2 ")" }
    in_services && /^    (ipc|uts|userns_mode): (host|service:|container:)/ { key = $1; sub(/:$/, "", key); print service " (" key " " $2 ")" }
  '
}

# The services with no health check in `docker compose config`'s output: one that stalls would show
# nowhere, the way a `sync` that stops fetching leaves `serve` and the core's probe status green.
unwatched() {
  awk '
    function done() { if (service != "" && !checked) print service }
    /^services:/ { in_services = 1; next }
    /^[^ ]/ { if (in_services) done(); in_services = 0; service = "" }
    in_services && /^  [^ ][^:]*:$/ { done(); service = $1; sub(/:$/, "", service); checked = 0 }
    in_services && /^    healthcheck:$/ { checked = 1 }
    END { if (in_services) done() }
  '
}

# The services of `docker compose config`'s output, one a line.
services_of() {
  awk '
    /^services:/ { in_services = 1; next }
    /^[^ ]/ { in_services = 0 }
    in_services && /^  [^ ][^:]*:$/ { service = $1; sub(/:$/, "", service); print service }
  '
}

# The value of one service's environment variable in `docker compose config`'s output.
environment_of() { # the service, then the variable
  awk -v wanted="$1:" -v name="$2:" '
    /^services:/ { in_services = 1; next }
    /^[^ ]/ { in_services = 0 }
    in_services && /^  [^ ][^:]*:$/ { service = $1 }
    in_services && service == wanted && $1 == name { print $2 }
  ' | tr -d '"'
}

# Where a service's mount at `target` comes from, in `docker compose config`'s output.
mount_source() { # the service, then the container path
  awk -v wanted="$1:" -v target="$2" '
    /^services:/ { in_services = 1; next }
    /^[^ ]/ { in_services = 0 }
    in_services && /^  [^ ][^:]*:$/ { service = $1 }
    in_services && service == wanted && /^        source:/ { source = $2 }
    in_services && service == wanted && /^        target:/ && $2 == target { print source }
  '
}

# What `docker compose config` says of each service, as `net SERVICE NETWORK` and
# `mount SERVICE CONTAINER_PATH` lines (a secret's target is not a mount).
service_facts() {
  awk '
    /^services:/ { in_services = 1; next }
    /^[^ ]/ { in_services = 0 }
    in_services && /^  [^ ][^:]*:$/ { service = $1; sub(/:$/, "", service); in_net = 0; in_vol = 0 }
    in_services && /^    networks:/ { in_net = 1; in_vol = 0; next }
    in_services && /^    volumes:/ { in_vol = 1; in_net = 0; next }
    in_services && /^    [^ ]/ { in_net = 0; in_vol = 0 }
    in_services && in_net && /^      [^ ]/ { network = $1; sub(/:$/, "", network); print "net " service " " network }
    in_services && in_vol && /^        target:/ { print "mount " service " " $2 }
  '
}

# The networks of `docker compose config`'s output that are internal: no route out of them.
internal_networks() {
  awk '
    /^networks:/ { in_networks = 1; next }
    /^[^ ]/ { in_networks = 0 }
    in_networks && /^  [^ ][^:]*:$/ { network = $1; sub(/:$/, "", network) }
    in_networks && /^    internal: true$/ { print network }
  '
}

# The code reader's route is the network's, not the code's (docs/security.md). Where `code-reader` is in
# the combination: it is on the probes and reader-out networks alone and mounts nothing; reader-egress is
# on reader-out and egress alone and mounts its allowlist alone; no other service is on reader-out, which
# is internal; the allowlist holds exactly one host, the host of the reader's own upstream. And, in every
# combination, the flag that lets an upstream outside your network be named is in one shipped file.
reader_rules() { # the name, `docker compose config`'s output, service_facts' output
  local name=$1 output=$2 facts=$3 offenders upstream host source entries expected
  offenders=$(grep -l 'READER_ALLOW_OUTSIDE' docker-compose*.yml 2>/dev/null | grep -vx 'docker-compose.reader.yml' | tr '
' ' ' || true)
  [ -z "$offenders" ] ||
    fail "$name: READER_ALLOW_OUTSIDE is the demo preset's alone (docker-compose.reader.yml), and is also in: $offenders"
  grep -qx 'code-reader' <<<"$(services_of <<<"$output")" || return 0
  [ "$(awk '$1 == "net" && $2 == "code-reader" { print $3 }' <<<"$facts" | sort | tr '
' ' ')" = "probes reader-out " ] ||
    fail "$name: code-reader is on the probes and reader-out networks alone"
  [ "$(awk '$1 == "net" && $2 == "reader-egress" { print $3 }' <<<"$facts" | sort | tr '
' ' ')" = "egress reader-out " ] ||
    fail "$name: reader-egress is on the reader-out and egress networks alone"
  offenders=$(awk '$1 == "net" && $3 == "reader-out" && $2 != "code-reader" && $2 != "reader-egress" { print $2 }' <<<"$facts" | tr '
' ' ')
  [ -z "$offenders" ] || fail "$name: only code-reader and reader-egress may be on reader-out, not: $offenders"
  grep -qx 'reader-out' <<<"$(internal_networks <<<"$output")" ||
    fail "$name: the network reader-out is not internal, so code-reader has a route out that isn't its proxy"
  ! grep -q '^mount code-reader ' <<<"$facts" || fail "$name: code-reader mounts nothing: no code on any volume"
  [ "$(awk '$1 == "mount" && $2 == "reader-egress" { print $3 }' <<<"$facts")" = /etc/egress/allowlist ] ||
    fail "$name: reader-egress mounts its allowlist (/etc/egress/allowlist) and nothing else"
  source=$(mount_source reader-egress /etc/egress/allowlist <<<"$output")
  source=${source//\\//}
  [ "${source##*/reader-egress/}" = allowlist ] ||
    fail "$name: reader-egress's allowlist isn't a reader-egress/allowlist file"
  upstream=$(environment_of code-reader READER_UPSTREAM_URL <<<"$output")
  [ -n "$upstream" ] || fail "$name: code-reader has no READER_UPSTREAM_URL"
  host=$(sed -E 's#^[A-Za-z]+://##; s#[/:?].*$##' <<<"$upstream")
  # the file the combination mounts, which an overlay may point elsewhere, not the shipped one
  [ -f "$source" ] || fail "$name: reader-egress's allowlist $source can't be read"
  entries=$(grep -vE '^[[:space:]]*(#|$)' "$source" || true)
  expected="^${host//./\\.}\$"
  [ "$entries" = "$expected" ] ||
    fail "$name: reader-egress/allowlist must hold exactly one host, $host (its upstream's), as $expected"
}

check_config() { # a name, then the compose file arguments (none: the default files from .env)
  local name=$1 output offenders facts
  shift
  output=$(docker compose "$@" config) || fail "$name: docker compose config failed"
  offenders=$(publishers <<<"$output")
  [ -z "$offenders" ] || fail "$name: only caddy may publish ports, not: $offenders"
  offenders=$(unconfined <<<"$output")
  [ -z "$offenders" ] || fail "$name: no service may be privileged or share a namespace, not: $offenders"
  offenders=$(unwatched <<<"$output" | tr '
' ' ')
  [ -z "$offenders" ] || fail "$name: every service has a health check, not: $offenders"
  # the probes' two networks have no route out, and a probe is on its own alone: `serve` (probe-NAME)
  # on the probes network, `sync` (probe-NAME-sync) on the sync network, which only the egress proxy
  # leaves; the core, which has no code, mounts its data, its configuration and the probes' tokens
  for network in probes sync; do
    grep -qx "$network" <<<"$(internal_networks <<<"$output")" ||
      fail "$name: the network $network is not internal, so its services have a route out"
  done
  facts=$(service_facts <<<"$output")
  grep -q '^mount core ' <<<"$facts" || fail "$name: no mount of the core was read, so its mounts can't be checked"
  offenders=$(awk '$1 == "net" && $2 ~ /^probe-/ && $2 !~ /-sync$/ && $3 != "probes" { print $2 " (" $3 ")" }' <<<"$facts")
  [ -z "$offenders" ] || fail "$name: a probe that answers questions may be on the probes network alone, not: $offenders"
  offenders=$(awk '$1 == "net" && $2 ~ /^probe-.*-sync$/ && $3 != "sync" { print $2 " (" $3 ")" }' <<<"$facts")
  [ -z "$offenders" ] || fail "$name: a probe that reads the git host may be on the sync network alone, not: $offenders"
  offenders=$(awk '$1 == "mount" && $2 == "core" && $3 !~ /^\/(data|config|run\/mercury\/probe-tokens)$/ { print $3 }' <<<"$facts")
  [ -z "$offenders" ] || fail "$name: the core has no code and mounts only its data, its configuration and the probes' tokens, not: $offenders"
  reader_rules "$name" "$output" "$facts"
  echo "ok: $name"
}

# A curl on the sync network, the way `sync` reaches out: through the egress proxy.
CURL_IMAGE=curlimages/curl:8.11.1

# The running containers of the stack's probes, as `service container` lines: found by the labels
# Compose sets, so a pair from any file (the fixture's, a real repository's block) is checked.
running_probes() {
  docker ps --filter "label=com.docker.compose.project=$PROJECT" \
    --format '{{.Label "com.docker.compose.service"}} {{.ID}}' | { grep '^probe-' || true; } | sort
}

check_serve() { # the service, then its container
  local service=$1 container=$2 answer
  # the positive control first: the same container gets an HTTP answer from the core's internal
  # listener on the probes network, so `mercury-probe reach` runs and names resolve, and a failing
  # connection out means no route rather than a missing tool or no DNS
  answer=$(docker exec "$container" mercury-probe reach http://core:8081/internal/probes/register --timeout 5 2>&1 |
    grep -o 'HTTP/[0-9.]* [0-9]*' | tail -1 || true)
  [ -n "$answer" ] ||
    fail "$service got no HTTP answer from the core on the probes network (http://core:8081), so a failing connection out would prove nothing"
  # `serve` has no proxy, so this is a plain TCP connection to example.com:443
  if docker exec "$container" mercury-probe reach https://example.com --timeout 5 >/dev/null 2>&1; then
    fail "$service reached https://example.com: serve must have no route out"
  fi
  echo "ok: $service reaches the core on the probes network (${answer#* }) and has no route out (a connection to https://example.com fails)"
}

# The snapshot's 20 largest files by their SHA-256, as `sha256sum` prints them, from a sync probe's
# container: a file elsewhere is one of them only when its bytes are the same, so a common name
# (package.json, .editorconfig) is no finding by itself. Nothing when there is no snapshot yet.
snapshot_hashes() { # the container
  docker exec "$1" sh -c \
    'find /repo/current/ -type f -size +100c -printf "%s %p\n" | sort -rn | head -20 | cut -d" " -f2- | tr "\n" "\0" | xargs -0 -r sha256sum' \
    2>/dev/null || true
}

# The same for a repository folder on this machine (the fixture a stack serves): its code, not the
# metadata (`meta/`) or the manifest (`.mercury/`), which are allowed to be on a volume.
folder_hashes() { # the folder
  (cd "$1" && find . -type f -size +100c -not -path './meta/*' -not -path './.mercury/*' -printf '%s %p\n' |
    sort -rn | head -20 | cut -d' ' -f2- | tr '\n' '\0' | xargs -0 -r sha256sum)
}

# Whether stdin holds a hash of `hashes` (lines of `sha256sum`'s output) in its first column.
same_bytes() { # the hashes
  awk 'NR == FNR { h[$1]; next } ($1 in h) { print "found"; exit }' <(awk '{ print $1 }' <<<"$1") -
}

# Where on this machine a file has the bytes of one of `hashes` (lines of `sha256sum`'s output): a
# line `volume NAME` for each named volume of the stack but the tmpfs ones, and `docker-data` when
# Docker's data directory has one, layers and the containers' own files included. `image` is any
# image with find and sha256sum.
copies_on_disk() { # the hashes, then the image
  local hashes=$1 image=$2 names name volume found
  local -a match=() ram=()
  names=$(awk '{ n = $2; sub(/.*\//, "", n); print n }' <<<"$hashes" | sort -u)
  # one find for all the names: ( -name a -o -name b … ), each hit hashed; a folder it can't read
  # is no finding
  for name in $names; do
    match+=(-o -name "$name")
  done
  match=("(" "${match[@]:1}" ")")
  # the tmpfs volumes are RAM: skipped here, and where Docker mounts them in its data directory
  for volume in $(docker volume ls -q --filter "label=com.docker.compose.project=$PROJECT"); do
    if [ "$(docker volume inspect -f '{{.Options.type}}' "$volume")" = tmpfs ]; then
      ram+=(-path "/d/volumes/$volume" -prune -o)
      continue
    fi
    # a volume is small: every file by its bytes, whatever its name
    found=$(docker run --rm --user 0:0 --entrypoint find -v "$volume:/v:ro" "$image" /v -type f \
      -exec sha256sum {} + 2>/dev/null | same_bytes "$hashes" || true)
    [ -z "$found" ] || echo "volume $volume"
  done
  # the whole of Docker's data directory: the layers (overlay2, or rootfs with containerd), the
  # containers' own files and the volumes but the tmpfs ones
  found=$(docker run --rm --user 0:0 --entrypoint find -v /var/lib/docker:/d:ro "$image" /d "${ram[@]}" -type f "${match[@]}" \
    -exec sha256sum {} + 2>/dev/null | same_bytes "$hashes" || true)
  [ -z "$found" ] || echo "docker-data"
}

check_sync() { # the service, then its container
  local service=$1 container=$2 network found volume image hashes
  docker exec "$container" grep -E '^([^ ]+ ){4}/repo ' /proc/self/mountinfo | tail -1 | grep -q ' - tmpfs ' ||
    fail "$service: /repo is not a tmpfs mount"
  echo "ok: $service has /repo on a tmpfs"

  network=$(docker inspect -f '{{range $name, $_ := .NetworkSettings.Networks}}{{$name}} {{end}}' "$container" | awk '{print $1}')
  for host in api.github.com codeload.github.com; do
    docker run --rm --network "$network" "$CURL_IMAGE" -sS -o /dev/null --max-time 20 -x http://egress-proxy:8888 "https://$host/" ||
      fail "$service: https://$host doesn't pass the egress proxy from the $network network"
  done
  echo "ok: $service reaches api.github.com and codeload.github.com through the egress proxy"
  for host in github.com example.com; do
    if docker run --rm --network "$network" "$CURL_IMAGE" -sS -o /dev/null --max-time 20 -x http://egress-proxy:8888 "https://$host/" 2>/dev/null; then
      fail "$service: the egress proxy let https://$host through"
    fi
  done
  if docker run --rm --network "$network" "$CURL_IMAGE" -sS -o /dev/null --max-time 10 --noproxy '*' https://example.com/ 2>/dev/null; then
    fail "$service: the $network network reaches https://example.com without the proxy"
  fi
  echo "ok: $service is refused github.com and example.com, through the proxy or around it"

  hashes=$(snapshot_hashes "$container")
  [ -n "$hashes" ] || fail "$service has no snapshot yet: wait for \"snapshot … in use\" in its log, then run the check again"
  image=$(docker inspect -f '{{.Config.Image}}' "$container")
  found=$(copies_on_disk "$hashes" "$image")
  volume=$(sed -n 's/^volume //p' <<<"$found" | head -1)
  [ -z "$volume" ] || fail "$service: a file of the snapshot is on the volume $volume"
  echo "ok: no file of $service's snapshot is on a named volume"
  ! grep -qx 'docker-data' <<<"$found" || fail "$service: a file of the snapshot is in Docker's data directory"
  echo "ok: no file of $service's snapshot ($(wc -l <<<"$hashes") files by their bytes) is in Docker's data directory"
}

# The web client through the one Caddyfile: its pages (known or not) are index.html with the policy that
# keeps everything on its own origin, no Server header, and a path of the core's internal listener gets
# the page too, never the core.
check_web() {
  local path headers type
  for path in / /ask/q_unknown; do
    headers=$(curl -sS -I "${CURL_TLS[@]}" "$URL$path") || fail "$URL$path doesn't answer"
    head -1 <<<"$headers" | grep -q ' 200' || fail "$URL$path isn't 200: $(head -1 <<<"$headers")"
    grep -qi '^content-type: text/html' <<<"$headers" || fail "$URL$path isn't HTML"
    for directive in "default-src 'self'" "connect-src 'self'" "frame-ancestors 'none'"; do
      grep -i '^content-security-policy:' <<<"$headers" | grep -qF "$directive" ||
        fail "$URL$path has no Content-Security-Policy with $directive"
    done
    grep -i '^content-security-policy:' <<<"$headers" | grep -q "script-src 'self' 'sha256-" ||
      fail "the Content-Security-Policy of $path has no script hash: is the web client's csp.caddy the one mounted?"
    ! grep -qi '^server:' <<<"$headers" || fail "$URL$path sends a Server header"
  done
  echo "ok: the web client comes with its CSP header, on its pages and on an unknown path"
  type=$(curl -sS -I "${CURL_TLS[@]}" "$URL/internal/probes/register" | tr -d '\r' | sed -n "s/^[Cc]ontent-[Tt]ype: //p") || fail "$URL/internal/… doesn't answer"
  case "$type" in
    text/html*) echo "ok: a path of the core's internal listener gets the web client's page through Caddy, never the core" ;;
    *) fail "$URL/internal/probes/register answered $type through Caddy, not the web client's page" ;;
  esac
  # the core serves POST /mcp alone: a path under it is the web client's, never the core's
  type=$(curl -sS -I "${CURL_TLS[@]}" "$URL/mcp/x" | tr -d '\r' | sed -n "s/^[Cc]ontent-[Tt]ype: //p") || fail "$URL/mcp/x doesn't answer"
  case "$type" in
    text/html*) echo "ok: a path under /mcp gets the web client's page through Caddy, only /mcp is the core's" ;;
    *) fail "$URL/mcp/x answered $type through Caddy, not the web client's page" ;;
  esac
}

# The code reader on a running stack. Every `serve` reaches its health check (the designed route, which
# is also the positive control for the lines after it) and can't reach reader-egress, which only the
# reader may. From the reader-out network, the upstream's host passes through reader-egress, and
# api.github.com and example.com are refused through it and unreachable around it; from the reader
# itself, api.github.com is unreachable. Nothing here sends a key or costs anything: a GET of a front
# page, with none.
check_reader() { # the code-reader container, then `service container` lines of the probes
  local reader=$1 probes=$2 service container network upstream host status
  while read -r service container; do
    case "$service" in *-sync) continue ;; esac
    status=$(docker exec "$container" mercury-probe reach http://code-reader:7100/health --timeout 5 2>&1 |
      grep -o 'HTTP/[0-9.]* [0-9]*' | tail -1 | cut -d' ' -f2 || true)
    [ "$status" = 200 ] ||
      fail "$service can't reach the code reader (http://code-reader:7100/health answered ${status:-nothing}) on the probes network"
    # any HTTP answer, even the proxy's refusal of a GET, means a route to it
    if docker exec "$container" mercury-probe reach http://reader-egress:8888/ --timeout 5 >/dev/null 2>&1; then
      fail "$service reached reader-egress:8888: only the code reader may"
    fi
    echo "ok: $service reaches the code reader on the probes network and can't reach reader-egress"
  done <<<"$probes"

  upstream=$(docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$reader" | sed -n 's/^READER_UPSTREAM_URL=//p')
  [ -n "$upstream" ] || fail "code-reader has no READER_UPSTREAM_URL"
  host=$(sed -E 's#^[A-Za-z]+://##; s#[/:?].*$##' <<<"$upstream")
  network=$(docker inspect -f '{{range $name, $_ := .NetworkSettings.Networks}}{{$name}} {{end}}' "$reader" |
    tr ' ' '\n' | grep -E '(^|_)reader-out$' | head -1)
  [ -n "$network" ] || fail "code-reader is not on a reader-out network"
  docker run --rm --network "$network" "$CURL_IMAGE" -sS -o /dev/null --max-time 20 -x http://reader-egress:8888 "https://$host/" ||
    fail "code-reader: https://$host doesn't pass reader-egress from the $network network"
  echo "ok: code-reader reaches $host through reader-egress"
  for other in api.github.com example.com; do
    if docker run --rm --network "$network" "$CURL_IMAGE" -sS -o /dev/null --max-time 20 -x http://reader-egress:8888 "https://$other/" 2>/dev/null; then
      fail "code-reader: reader-egress let https://$other through"
    fi
  done
  if docker run --rm --network "$network" "$CURL_IMAGE" -sS -o /dev/null --max-time 10 --noproxy '*' https://example.com/ 2>/dev/null; then
    fail "code-reader: the $network network reaches https://example.com without the proxy"
  fi
  # from the reader's own container, through its HTTPS_PROXY (reader-egress), which must refuse the CONNECT
  if docker exec "$reader" mercury-probe reach https://api.github.com --timeout 5 >/dev/null 2>&1; then
    fail "code-reader reached https://api.github.com itself"
  fi
  echo "ok: code-reader is refused api.github.com and example.com, through its proxy or around it"
}

check_stack() {
  local health status service container caddy reader
  if [ -n "${1:-}" ]; then URL=${1%/}; fi
  if [ -n "${2:-}" ]; then
    [ -s "$2" ] || fail "the CA file $2 is missing or empty"
    CURL_TLS=(--cacert "$2")
    # curl on Windows checks revocation through schannel, which a local CA has nothing for
    if curl -V | grep -qi schannel; then CURL_TLS+=(--ssl-no-revoke); fi
  fi
  health=$(curl -fsS "${CURL_TLS[@]}" "$URL/v1/health") ||
    fail "/v1/health doesn't answer through Caddy on $URL"
  echo "ok: /v1/health through Caddy on $URL: $health"
  caddy=$(container_of caddy)
  [ -n "$caddy" ] || fail "no caddy container of the project $PROJECT is running (MERCURY_COMPOSE_PROJECT names another)"
  status=$(docker exec "$caddy" wget -S -O /dev/null http://core:8081/internal/probes/register 2>&1 |
    grep -o 'HTTP/[0-9.]* [0-9]*' | tail -1 | cut -d' ' -f2 || true)
  case "$status" in
    403) echo "ok: caddy gets 403 from core:8081" ;;
    "") fail "caddy reached no listener on core:8081, so the 403 can't be checked: is the core's internal listener running?" ;;
    *) fail "caddy got $status from core:8081, not 403" ;;
  esac
  check_web

  local probes
  probes=$(running_probes)
  # a stack with no probe running would pass with nothing checked
  [ -n "$probes" ] ||
    fail "no probe of the stack is running (no probe-* container of the project $PROJECT), so nothing of the probes can be checked: start the stack first"
  while read -r service container; do
    case "$service" in
      *-sync) check_sync "$service" "$container" ;;
      *) check_serve "$service" "$container" ;;
    esac
  done <<<"$probes"
  reader=$(container_of code-reader)
  if [ -n "$reader" ]; then check_reader "$reader" "$probes"; fi
}

# The scratch volume of the positive control, and the one that stands in for a probe's /repo.
CONTROL_VOLUME="mercury-disk-control-$$"
TMPFS_VOLUME="mercury-disk-tmpfs-$$"
remove_scratch() {
  docker volume rm -f "$CONTROL_VOLUME" "$TMPFS_VOLUME" >/dev/null 2>&1 || true
}

# The check can fail: a file with known bytes, planted on a named volume of the stack under the name
# a snapshot file would have, must be found on that volume and in Docker's data directory. Without
# this a search that finds nothing would prove nothing about the search.
disk_control() { # an image with find and sha256sum
  local image=$1 file hashes found
  file=$(mktemp)
  # not the repository's, and not like any file a stack holds: random bytes
  head -c 4096 /dev/urandom | base64 >"$file"
  hashes=$(sha256sum "$file" | awk -v name="disk-control-$$.txt" '{ print $1 "  /" name }')
  trap remove_scratch EXIT
  # the scratch volumes an interrupted run left, which the search would read too
  docker volume ls -q --filter label=mercury.check=disk-control | xargs -r docker volume rm -f >/dev/null 2>&1 || true
  docker volume create --label "com.docker.compose.project=$PROJECT" --label mercury.check=disk-control "$CONTROL_VOLUME" >/dev/null
  # through stdin: a file of this machine's temporary folder is no path Docker Desktop can mount
  docker run -i --rm --user 0:0 --entrypoint sh -v "$CONTROL_VOLUME:/v" "$image" -c "cat > /v/disk-control-$$.txt" <"$file"
  found=$(copies_on_disk "$hashes" "$image")
  rm -f "$file"
  grep -qx "volume $CONTROL_VOLUME" <<<"$found" ||
    fail "a file planted on the volume $CONTROL_VOLUME was not found on it: the search can't see a copy, so it proves nothing"
  grep -qx 'docker-data' <<<"$found" ||
    fail "a file planted on the volume $CONTROL_VOLUME was not found in Docker's data directory: the search of it can't see a copy"
  remove_scratch
  echo "ok: a file planted on a named volume is found there and in Docker's data directory (the positive control, its volume removed)"
}

# The volume under a sync probe's /repo is a tmpfs, and a tmpfs volume does not outlive its
# containers: one with the same options, written by a container that then stops, is empty for the next.
check_tmpfs_empties() { # the service, its container, an image
  local service=$1 container=$2 image=$3 volume type options left
  volume=$(docker inspect -f '{{range .Mounts}}{{if eq .Destination "/repo"}}{{.Name}}{{end}}{{end}}' "$container")
  [ -n "$volume" ] || fail "$service: nothing is mounted at /repo by a named volume"
  type=$(docker volume inspect -f '{{.Options.type}}' "$volume")
  [ "$type" = tmpfs ] || fail "$service: the volume $volume under /repo is not a tmpfs volume (type $type)"
  options=$(docker volume inspect -f '{{range $key, $value := .Options}}--opt {{$key}}={{$value}} {{end}}' "$volume")
  # shellcheck disable=SC2086 # the options are words by design
  docker volume create --driver local $options "$TMPFS_VOLUME" >/dev/null
  trap remove_scratch EXIT
  docker run --rm --user 0:0 --entrypoint sh -v "$TMPFS_VOLUME:/v" "$image" -c 'echo code > /v/planted'
  left=$(docker run --rm --user 0:0 --entrypoint sh -v "$TMPFS_VOLUME:/v" "$image" -c 'ls -A /v')
  [ -z "$left" ] || fail "$service: a tmpfs volume with the options of $volume kept $left after the container that wrote it stopped"
  remove_scratch
  echo "ok: $service's /repo is the tmpfs volume $volume, and a tmpfs volume of its options is empty once its containers have stopped"
}

check_disk() {
  local probes service container image hashes found volume swept=0
  probes=$(running_probes)
  [ -n "$probes" ] ||
    fail "no probe of the stack is running (no probe-* container of the project $PROJECT), so there is no repository to look for: start the stack first"
  image=$(docker inspect -f '{{.Config.Image}}' "$(head -1 <<<"$probes" | cut -d' ' -f2)")
  disk_control "$image"
  while read -r service container; do
    case "$service" in
      *-sync)
        docker exec "$container" grep -E '^([^ ]+ ){4}/repo ' /proc/self/mountinfo | tail -1 | grep -q ' - tmpfs ' ||
          fail "$service: /repo is not a tmpfs mount"
        check_tmpfs_empties "$service" "$container" "$image"
        hashes=$(snapshot_hashes "$container")
        [ -n "$hashes" ] || fail "$service has no snapshot yet: wait for \"snapshot … in use\" in its log, then run the check again"
        found=$(copies_on_disk "$hashes" "$image")
        volume=$(sed -n 's/^volume //p' <<<"$found" | head -1)
        [ -z "$volume" ] || fail "$service: a file of the snapshot is on the volume $volume"
        ! grep -qx 'docker-data' <<<"$found" || fail "$service: a file of the snapshot is in Docker's data directory"
        echo "ok: no file of $service's snapshot ($(wc -l <<<"$hashes") files by their bytes) is on a named volume or in Docker's data directory"
        swept=$((swept + 1))
        ;;
    esac
  done <<<"$probes"
  if [ "$swept" -eq 0 ]; then
    # the fixture stack: its probes read the fixture from a read-only mount and sync nothing
    local fixture_dir
    fixture_dir=$(cd ../.. && pwd)/tests/fixtures/repo-delta
    hashes=$(folder_hashes "$fixture_dir")
    [ -n "$hashes" ] || fail "the fixture repository ($fixture_dir) has no files to look for"
    found=$(copies_on_disk "$hashes" "$image")
    volume=$(sed -n 's/^volume //p' <<<"$found" | head -1)
    [ -z "$volume" ] || fail "a file of the fixture repository is on the volume $volume"
    ! grep -qx 'docker-data' <<<"$found" || fail "a file of the fixture repository is in Docker's data directory"
    echo "ok: no file of the fixture repository ($(wc -l <<<"$hashes") files by their bytes) is on a named volume or in Docker's data directory"
    echo "note: no sync probe of the project $PROJECT is running, so what a sync leaves behind was not looked at: run this on a stack that has synced"
  fi
}

# The secret files the Compose files name, as `name path` lines, from `docker compose config`.
secret_files() {
  docker compose config --format json | node -e '
    let text = "";
    process.stdin.on("data", (chunk) => (text += chunk)).on("end", () => {
      for (const [name, secret] of Object.entries(JSON.parse(text).secrets ?? {})) {
        if (secret.file) console.log(name + " " + secret.file);
      }
    });'
}

# The secret files are uid 10001's, in folders only that user enters (vm-smoke.sh and a server's steps
# give them so): another user who isn't root sees nothing in them, not even that a file is there. So
# every test, stat and read of a secret file or its folders goes through this, as root through
# `sudo -n` when this user isn't root and that works, else as this user.
secrets_sudo=false
as_root() { # a command on a secret file or one of its folders
  if $secrets_sudo; then sudo -n "$@"; else "$@"; fi
}

# Whether this user can't tell if a file is there: the nearest folder above it that this user sees is
# closed to them. Such a file isn't missing, it is out of reach.
out_of_reach() { # the file
  local dir
  dir=$(dirname "$1")
  while [ "$dir" != / ] && [ "$dir" != . ] && ! [ -d "$dir" ]; do dir=$(dirname "$dir"); done
  ! [ -x "$dir" ]
}

check_secrets() {
  local config files name file owner mode dir logs value found=0
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) echo "check: secrets can't be checked on Windows, which keeps no Unix owners or modes; Linux checks them (just ci-vm)" >&2; exit 2 ;;
  esac
  if [ "$(id -u)" != 0 ] && sudo -n true 2>/dev/null; then secrets_sudo=true; fi
  command -v node >/dev/null || fail "secrets: node reads docker compose config's JSON and isn't on the PATH"
  config=$(docker compose config) || fail "secrets: docker compose config failed"
  files=$(secret_files) || fail "secrets: docker compose config --format json failed"
  [ -n "$files" ] || fail "secrets: the Compose files name no secret file, so nothing was checked"
  if [ -n "$(container_of core)" ]; then
    logs=$(docker ps -a --filter "label=com.docker.compose.project=$PROJECT" -q | xargs -r -n1 docker logs 2>&1) ||
      fail "secrets: the logs of the project $PROJECT can't be read"
  fi
  while read -r name file; do
    if ! as_root test -f "$file"; then
      if [ "$(id -u)" != 0 ] && ! $secrets_sudo && out_of_reach "$file"; then
        fail "secrets: $name's file $file can't be reached, a folder above it is closed to this user (run as root, or with sudo)"
      fi
      fail "secrets: $name's file $file is missing"
    fi
    owner=$(as_root stat -c %u "$file") || fail "secrets: $file can't be stat'ed (run as root, or with sudo)"
    mode=$(as_root stat -c %a "$file") || fail "secrets: $file can't be stat'ed (run as root, or with sudo)"
    [ "$owner" = 10001 ] || fail "secrets: $file is owned by uid $owner, not 10001"
    [ "$mode" = 400 ] || fail "secrets: $file has mode $mode, not 0400"
    dir=$(dirname "$file")
    while [ "$dir" != / ] && [ "$(basename "$dir")" != compose ] && [ "$(as_root stat -c %u "$dir")" = 10001 ]; do
      mode=$(as_root stat -c %a "$dir")
      [ $((8#$mode & 8#077)) -eq 0 ] || fail "secrets: the folder $dir lets others in (mode $mode)"
      dir=$(dirname "$dir")
    done
    # an empty file holds no secret to look for (the model key before it is set)
    value=$(as_root cat "$file" 2>/dev/null) ||
      fail "secrets: $file can't be read to look for its value (run as root, or with sudo)"
    if [ -n "$value" ]; then
      ! grep -qF -- "$value" <<<"$config" || fail "secrets: $name's value is in docker compose config"
      if [ -n "${logs:-}" ]; then
        ! grep -qF -- "$value" <<<"$logs" || fail "secrets: $name's value is in a log of the project $PROJECT"
      fi
    fi
    found=$((found + 1))
    echo "ok: $name ($file): uid 10001, mode 0400"
  done <<<"$files"
  echo "ok: $found secret file(s), none of their values in docker compose config$([ -n "${logs:-}" ] && echo " or the logs of $PROJECT")"
}

case "${1:-}" in
  config)
    check_config "the VM (the default files)"
    check_config "the laptop with the fixture" "${fixture[@]}"
    check_config "the laptop with real repositories" "${repos[@]}"
    check_config "just demo-local with the fixture and the poisoned repository" "${demo[@]}"
    check_config "just demo-local with real repositories and the poisoned repository" "${demo_repos[@]}"
    check_config "the integration runs" "${runs[@]}"
    check_config "the laptop with the fixture and the code reader" "${reader_fixture[@]}"
    check_config "just demo-local with real repositories, the poisoned repository and the code reader" "${reader_demo_repos[@]}"
    ;;
  stack) check_stack "${2:-}" "${3:-}" ;;
  secrets) check_secrets ;;
  disk) check_disk ;;
  *) fail "usage: check.sh config|stack [URL [CA]]|disk|secrets" ;;
esac

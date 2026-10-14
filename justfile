# Mercury's task runner (https://github.com/casey/just). `just ci` is everything CI runs.
# `just` alone lists every recipe with what it does.

# The Compose files of the laptop stack on the fixture repository, relative to deploy/compose
laptop_fixture := "-f docker-compose.yml -f docker-compose.fixture.yml -f docker-compose.local.yml"

# The image scanner: Trivy's official image, pinned by its tag and its digest
trivy_image := "aquasec/trivy:0.75.0@sha256:af6acf9a6b85dfe389a1941505c0ce9efef52a4719635e1a962f022a3d855daa"

default:
    @just --list

# The core on http://localhost:8080 and the probe in dev mode on the fixture repository, registered with it; Ctrl-C stops both
dev:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --quiet --locked -p mercury-core -p mercury-probe
    # the configuration examples, with the probe at its local address instead of its Compose name and
    # this machine's probe token, which the probe registers with (pwd -W: the path Windows reads)
    mkdir -p target/dev-config
    cp -R config-examples/. target/dev-config/
    token="$(pwd -W 2>/dev/null || pwd)/target/dev-probe.token"
    [ -s "$token" ] || head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -dc 'A-Za-z0-9_-' > "$token"
    sed -e "s#http://probe-delta:7000#http://127.0.0.1:7000#" -e "s#/run/mercury/probe-tokens/delta.token#$token#" config-examples/projects.yaml > target/dev-config/projects.yaml
    trap 'kill $(jobs -p) 2>/dev/null || true' EXIT
    # the page's origin is the Vite dev server's (just web-dev proxy); the session cookie is refused from any other
    MERCURY_CONFIG_DIR=target/dev-config MERCURY_INTERNAL_ALLOW_CIDRS=127.0.0.1/32 MERCURY_PUBLIC_ORIGIN="${MERCURY_PUBLIC_ORIGIN:-http://localhost:5173}" cargo run --quiet --locked -p mercury-core &
    PROBE_TOKEN_FILE="$token" PROBE_CORE_INTERNAL_URL=http://127.0.0.1:8081 cargo run --quiet --locked -p mercury-probe -- dev --repo tests/fixtures/repo-delta --meta tests/fixtures/repo-delta/meta --listen 127.0.0.1:7000 &
    wait -n

# fmt check and clippy with warnings as errors
lint: _fmt _clippy

# Unit tests; .config/nextest.toml ends a test that hangs
test:
    cargo nextest run --locked --workspace

# Whole questions through the Compose stack on the fixture, with the replay server as the models (tests/it); --web adds the web client in a real browser (apps/web/e2e, after `just _web-install`), --keep leaves the stack up
it *args: _images _cli
    node tests/it/run.mjs {{ args }}

# The golden questions through the Compose stack → tests/golden/reports/<date>-golden.md (--subset forbidden, --keep; --live asks the real models with the OpenRouter key, a little money)
golden *args: _images _cli
    node tests/golden/run.mjs {{ args }}

# The attack questions through the Compose stack (tests/redteam), on the fixture and on the poisoned repository (--only=A4,A6 runs some, --keep leaves the stack up; --live asks the real models with the OpenRouter key, a little money)
redteam *args: _images _cli
    node tests/redteam/run.mjs {{ args }}

# The judge's accuracy and latency report: the labelled sets through each backend → tests/golden/reports/<date>-judge.md (--stand-in, --check ID)
judge-eval *args:
    cargo run --quiet --locked -p mercury-judge --bin judge-eval -- {{ args }}

# Latency and throughput benchmarks
bench *args: _images _cli
    node tests/bench/run.mjs {{ args }}

# Write the core's OpenAPI document to openapi.json: every body and stream event of the API
openapi:
    cargo run --quiet --locked -p mercury-schema --example openapi -- openapi.json

# TypeScript types for apps/web from openapi.json, into src/lib/api/schema.d.ts
types:
    cd apps/web && pnpm install --frozen-lockfile
    cd apps/web && pnpm exec openapi-typescript ../../openapi.json --output src/lib/api/schema.d.ts
    cd apps/web && pnpm exec prettier --write src/lib/api/schema.d.ts

# Copy the catalogue and the prompts of config-examples into every preset, and demo's files too: run it after changing the examples
presets-sync:
    bash deploy/compose/scripts/presets.sh sync

# The web client's dev server on http://localhost:5173: mock (the default) answers from recordings, `proxy` goes to the core of `just dev`
web-dev mode="mock":
    #!/usr/bin/env bash
    set -euo pipefail
    cd apps/web
    pnpm install --frozen-lockfile
    case "{{ mode }}" in
        mock) VITE_MERCURY_MOCK=true pnpm dev ;;
        proxy) pnpm dev ;;
        *) echo "just web-dev: the mode is mock or proxy, not {{ mode }}" >&2; exit 1 ;;
    esac

# The local stack in Docker Compose on http://localhost:8080: the core, and a probe on the fixture repository
compose-up: _images _web-build
    #!/usr/bin/env bash
    set -euo pipefail
    cd deploy/compose
    # this machine's own configuration and secrets: the configuration starts as the examples, never
    # overwritten once there, the key file stays empty until the judge needs one, and the fixture's
    # probe token is random
    mkdir -p config secrets/probes
    [ -n "$(ls -A config)" ] || cp -R ../../config-examples/. config/
    [ -e secrets/openrouter_api_key ] || : > secrets/openrouter_api_key
    [ -s secrets/probes/delta.token ] || head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -dc 'A-Za-z0-9_-' > secrets/probes/delta.token
    docker compose {{ laptop_fixture }} up -d --remove-orphans
    for _ in $(seq 30); do
        if curl -fsS http://localhost:8080/v1/health; then echo; exit 0; fi
        sleep 1
    done
    echo "just compose-up: /v1/health doesn't answer on http://localhost:8080; see docker compose ps" >&2
    exit 1

# Stop the local Compose stack
compose-down:
    cd deploy/compose && docker compose {{ laptop_fixture }} down --remove-orphans

# A laptop stack on http://localhost:8080 for your own repositories: the demo look of the web client, and a sync + serve pair per real repository (deploy/compose/scripts/demo-local.sh says what it needs)
demo-local: _images (_web-build "demo")
    bash deploy/compose/scripts/demo-local.sh

# CI runs the five parts as five parallel jobs, each calling the recipes its part is made of, one step each.
# Everything CI runs, in five parts, cheapest first
ci: ci-policy ci-rust ci-web ci-e2e ci-vm

# The lockfile check fails when Cargo.lock no longer matches the manifests, before anything compiles.
# The checks that compile nothing: fmt, the lockfile, cargo-deny, a secret scan of the history, the Compose files, the presets
ci-policy: _fmt
    cargo metadata --locked --format-version 1 > /dev/null
    cargo deny check
    gitleaks git --redact --no-banner --log-opts=HEAD .
    bash deploy/compose/scripts/check.sh config
    bash deploy/compose/scripts/presets.sh check

# Clippy and the unit tests
ci-rust: _clippy test

# The audit covers the development dependencies too, since the build and the tests run them: a high or critical advisory fails it.
# The web client: install from the lockfile, audit the dependencies, then format and lint, type check, tests and the build
ci-web:
    cd apps/web && pnpm install --frozen-lockfile
    cd apps/web && pnpm audit --audit-level high
    cd apps/web && pnpm lint
    cd apps/web && pnpm check
    cd apps/web && pnpm test
    cd apps/web && pnpm build
    just _web-licenses
    just _types-check

# The images (the web client's too) and their scan, the configuration examples checked by the core image that reads them, the runs on Compose with the web client in a browser, and the attack questions (recorded)
ci-e2e: _images _image-scan _config-check _web-install (it "--web") redteam

# The default Compose files started as a server runs them (80 and 443, TLS by Caddy's local CA for localhost) and checked over HTTPS: deploy/compose/scripts/vm-smoke.sh says what; --keep leaves the stack up
vm-smoke *args: _images
    bash deploy/compose/scripts/vm-smoke.sh {{ args }}

# A server's stack, the way CI checks it: the images, then the VM-mode smoke test (Linux; ports 80 and 443 free; root or sudo for the secret files' owner)
ci-vm: vm-smoke

# Every package the web client installs, development ones included, under a licence of the list (an expression passes when one of its choices is on it); the list is cargo-deny's and the few the build tools need
_web-licenses:
    #!/usr/bin/env bash
    set -euo pipefail
    cd apps/web
    # deny.toml's list, then the build tools': 0BSD and CC0-1.0 (permissive), BlueOak-1.0.0 (minimatch),
    # Python-2.0 (argparse) and MPL-2.0 (lightningcss), which the build runs and doesn't ship
    allowed="Apache-2.0 MIT BSD-2-Clause BSD-3-Clause ISC Unicode-3.0 Zlib 0BSD CC0-1.0 BlueOak-1.0.0 Python-2.0 MPL-2.0"
    pnpm licenses list --json | ALLOWED="$allowed" node -e '
      const allowed = new Set(process.env.ALLOWED.split(" "));
      let text = "";
      process.stdin.on("data", (chunk) => (text += chunk)).on("end", () => {
        const outside = [];
        let count = 0;
        for (const [licence, packages] of Object.entries(JSON.parse(text))) {
          const choices = licence.replace(/[()]/g, "").split(/ OR /i).map((choice) => choice.trim());
          for (const p of packages) {
            count += 1;
            if (!choices.some((choice) => allowed.has(choice))) outside.push(p.name + "@" + p.versions.join(",") + ": " + licence);
          }
        }
        if (outside.length > 0) {
          console.error("just _web-licenses: " + outside.length + " package(s) under a licence outside the list:\n  " + outside.join("\n  "));
          process.exit(1);
        }
        console.log("web licences: " + count + " packages, every one under a licence of the list");
      });'

# The client types are what `just types` writes from openapi.json: one that differs fails, and the file is put back as it was
_types-check:
    #!/usr/bin/env bash
    set -euo pipefail
    file=apps/web/src/lib/api/schema.d.ts
    before=$(mktemp)
    trap 'rm -f "$before"' EXIT
    cp "$file" "$before"
    just types > /dev/null
    if ! diff -q <(tr -d '\r' <"$before") <(tr -d '\r' <"$file") > /dev/null; then
        diff <(tr -d '\r' <"$before") <(tr -d '\r' <"$file") | head -20 >&2 || true
        cp "$before" "$file"
        echo "just _types-check: $file isn't what just types writes from openapi.json; run just types and commit the file" >&2
        exit 1
    fi
    echo "client types: $file is what just types writes from openapi.json"

_fmt:
    cargo fmt --all -- --check

_clippy:
    cargo clippy --locked --workspace --all-targets -- -D warnings

# The web client's build, which the laptop files mount (apps/web/build and its csp.caddy); a theme, `demo`, picks the look
_web-build theme="":
    cd apps/web && pnpm install --frozen-lockfile
    cd apps/web && PUBLIC_MERCURY_THEME={{ theme }} pnpm build

# The web client's packages, Playwright's included; its browser is MERCURY_E2E_CHANNEL=msedge|chrome on this machine, or `pnpm exec playwright install chromium`
_web-install:
    cd apps/web && pnpm install --frozen-lockfile

# The core, probe and web images, named as deploy/compose/.env says
_images:
    #!/usr/bin/env bash
    set -euo pipefail
    images=$(cd deploy/compose && docker compose config --images)
    for target in core probe web; do
        docker build -f deploy/docker/Dockerfile --target "$target" -t "$(grep -m1 "/mercury-$target:" <<<"$images")" .
    done

# A critical vulnerability that has a fix fails the scan, unless .trivyignore.yaml lets it through with its reason; Trivy's database stays in the Docker volume mercury-trivy-cache between runs.
# Without names it expects the three images the Compose files run (the egress proxies are the probe image's `egress`), and an image it can't name or find fails it.
# The core, probe and web images as `_images` left them (or the images named), scanned with Trivy
_image-scan *images:
    #!/usr/bin/env bash
    set -euo pipefail
    images="{{ images }}"
    if [ -z "$images" ]; then
        names=$(cd deploy/compose && docker compose config --images)
        # one pattern per image expected, as `config --images` prints it
        for pattern in '/mercury-core:' '/mercury-probe:' '/mercury-web:'; do
            image=$(grep -m1 -E -- "$pattern" <<<"$names") || { echo "just _image-scan: docker compose config --images names no image matching $pattern; the scan expects the core, probe and web images" >&2; exit 1; }
            images="$images $image"
        done
    fi
    # each image reaches Trivy as a file, so the scanner gets no Docker socket; pwd -W: the path Docker Desktop reads on Windows
    dir=target/image-scan
    root=$(pwd -W 2>/dev/null || pwd)
    mkdir -p "$dir"
    trap 'rm -rf "$dir"' EXIT
    failed=""
    scanned=0
    for image in $images; do
        docker image inspect "$image" > /dev/null 2>&1 || { echo "just _image-scan: no image $image; just _images builds Mercury's" >&2; exit 1; }
        file="${image//[^A-Za-z0-9._-]/_}.tar"
        docker image save -o "$dir/$file" "$image"
        echo "image scan: $image"
        # exit 3: a finding; any other failure is the scan's own
        status=0
        MSYS_NO_PATHCONV=1 docker run --rm -v mercury-trivy-cache:/cache -v "$root/$dir:/scan:ro" -v "$root/.trivyignore.yaml:/trivyignore.yaml:ro" \
            {{ trivy_image }} image --input "/scan/$file" --cache-dir /cache --ignorefile /trivyignore.yaml --show-suppressed \
            --scanners vuln --severity CRITICAL --ignore-unfixed --exit-code 3 --no-progress --disable-telemetry --skip-version-check \
            || status=$?
        rm -f "$dir/$file"
        case "$status" in
            0) ;;
            3) failed="$failed $image" ;;
            *) echo "just _image-scan: Trivy couldn't scan $image (exit $status)" >&2; exit 1 ;;
        esac
        scanned=$((scanned + 1))
    done
    if [ -n "$failed" ]; then
        echo "just _image-scan: critical vulnerabilities with a fix in:$failed (above); rebuild on a fixed base, or let one through in .trivyignore.yaml with its reason" >&2
        exit 1
    fi
    echo "image scan: $scanned images, none with a critical vulnerability that has a fix"

# `admin config check` on the configuration examples and on each preset, inside the core image, with no network
_config-check: _images
    #!/usr/bin/env bash
    set -euo pipefail
    image=$(cd deploy/compose && docker compose config --images | grep -m1 "/mercury-core:")
    # pwd -W: the path Docker Desktop reads on Windows; no path conversion of the mount argument
    MSYS_NO_PATHCONV=1 docker run --rm --network none -v "$(pwd -W 2>/dev/null || pwd)/config-examples:/config:ro" "$image" admin config check --dir /config
    for preset in demo private strict; do
        echo "preset $preset:"
        MSYS_NO_PATHCONV=1 docker run --rm --network none -v "$(pwd -W 2>/dev/null || pwd)/config-examples/presets/$preset:/config:ro" "$image" admin config check --dir /config
    done

# The CLI the runs on Compose ask with: the binary MERCURY_CLI names when it is set, else a debug build
_cli:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${MERCURY_CLI:-}" ]; then
        [ -x "$MERCURY_CLI" ] || { echo "just: MERCURY_CLI=$MERCURY_CLI is not an executable file" >&2; exit 1; }
    else
        cargo build --quiet --locked -p mercury-cli
    fi

# CI's CLI for the runs on Compose: the release build of the image builder, exported to target/ci-bin/mercury
_cli-image:
    docker build -f deploy/docker/Dockerfile --target cli --output type=local,dest=target/ci-bin .

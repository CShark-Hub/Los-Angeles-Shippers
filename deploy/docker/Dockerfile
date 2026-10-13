# syntax=docker/dockerfile:1
# One build, three images, from the repository root (`just ci` names them from deploy/compose/.env):
#   docker build -f deploy/docker/Dockerfile --target core  -t <registry>/mercury-core:<version> .
#   docker build -f deploy/docker/Dockerfile --target probe -t <registry>/mercury-probe:<version> .
#   docker build -f deploy/docker/Dockerfile --target web   -t <registry>/mercury-web:<version> .
# and the CLI binary alone, which CI's runs on Compose ask with (never an image):
#   docker build -f deploy/docker/Dockerfile --target cli --output type=local,dest=target/ci-bin .

FROM rust:1.98.0-slim-trixie AS build
# The toolchain the image ships is the pinned one; this skips the components the pin adds for linting.
ENV RUSTUP_TOOLCHAIN=1.98.0
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .cargo .cargo
COPY .sqlx .sqlx
COPY crates crates
# The CLI in the same build: one compile of the shared dependencies serves all three. Built alone, it
# resolves their features differently and compiles them again (almost three minutes on CI's agent).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p mercury-core -p mercury-probe -p mercury-cli \
 && install -D -m 0755 target/release/mercury-core /out/mercury-core \
 && install -D -m 0755 target/release/mercury-probe /out/mercury-probe \
 && install -D -m 0755 target/release/mercury /out/mercury

FROM debian:trixie-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 mercury \
 && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin mercury

FROM runtime AS core
RUN install -d -o 10001 -g 10001 /data
COPY --from=build /out/mercury-core /usr/local/bin/mercury-core
USER 10001:10001
ENV MERCURY_PUBLIC_ADDR=0.0.0.0:8080
EXPOSE 8080 8081
ENTRYPOINT ["mercury-core"]

# No git: the probe reads GitHub only through its API. No other tool either: the health checks, the
# network checks and the egress proxy are the binary's own `healthcheck`, `reach` and `egress`.
FROM runtime AS probe
RUN install -d -o 10001 -g 10001 /repo /meta /data
COPY --from=build /out/mercury-probe /usr/local/bin/mercury-probe
USER 10001:10001
ENV HOME=/tmp
EXPOSE 7000
ENTRYPOINT ["mercury-probe"]
CMD ["serve"]

FROM scratch AS cli
COPY --from=build /out/mercury /mercury

# The web client: Node builds it, and no Node runs in production. The build is static files, and
# `pnpm build` writes the CSP header Caddy sends beside it (csp.caddy; the page's own script hashes
# change with every build). PUBLIC_MERCURY_THEME=demo picks the demo look at build time.
FROM node:22-alpine AS web-build
ARG PUBLIC_MERCURY_THEME=
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0 CI=true PUBLIC_MERCURY_THEME=${PUBLIC_MERCURY_THEME}
RUN corepack enable
WORKDIR /web
COPY apps/web/package.json apps/web/pnpm-lock.yaml apps/web/.npmrc ./
RUN --mount=type=cache,target=/root/.local/share/pnpm/store pnpm install --frozen-lockfile
COPY apps/web ./
RUN pnpm build

# The official Caddy image with the build in /srv/web and its CSP header in /etc/caddy/. The Caddyfile
# stays a mounted file (deploy/compose/Caddyfile), so a header or route change needs no new image.
FROM caddy:2.11.4-alpine AS web
COPY --from=web-build /web/build /srv/web
COPY --from=web-build /web/csp.caddy /etc/caddy/csp.caddy

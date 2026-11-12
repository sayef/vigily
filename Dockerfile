# vigily in a container: the web app and API on port 7600, the repository and an optional
# vigily.toml under /data. `release` (the default) compiles inside Docker; `local` packages a Linux
# binary built outside it (scripts/docker-local.sh).
#
#   docker build -t vigily .

FROM node:24-bookworm-slim AS ui
WORKDIR /src/crates/vigily-ui
COPY crates/vigily-ui/package.json crates/vigily-ui/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY crates/vigily-ui/ ./
RUN npm run build

FROM rust:1.99-bookworm AS build
# Debian packages follow the base image tag; pinned versions would break when the archive moves on.
# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
COPY --from=ui /src/crates/vigily-ui/dist crates/vigily-ui/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo fetch --locked
# Object-store backends, as in the release binaries; pass --build-arg FEATURES= for local disk only.
ARG FEATURES=s3,gcs,azure
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --frozen -p vigily-server --features "${FEATURES}" \
    && cargo build --release --frozen -p vigily-cli \
    && cp target/release/vigily target/release/vigily-engine /usr/local/bin/

FROM debian:bookworm-slim AS runtime
# Unpinned for the same reason as above: security updates arrive with each rebuild.
# hadolint ignore=DL3008
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 vigily \
    && useradd --uid 10001 --gid 10001 --home-dir /data --create-home vigily
USER 10001:10001
WORKDIR /data
# The repository lives in the volume; /data/vigily.toml, when present, is the configuration.
ENV HOME=/data \
    VIGILY_REPO=/data/repo \
    VIGILY_PORT=7600
VOLUME /data
EXPOSE 7600
# bash's /dev/tcp keeps curl out of the image.
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --start-interval=2s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${VIGILY_PORT} && printf 'GET /api/health HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q '^ok$' <&3"]
ENTRYPOINT ["vigily"]
# Inside the container every interface; publish the port only where you want it reachable.
CMD ["up", "--host", "0.0.0.0"]

# A binary built outside Docker for each platform: scripts/docker-local.sh.
FROM runtime AS local
ARG TARGETARCH
COPY --chmod=755 dist/linux/${TARGETARCH}/vigily dist/linux/${TARGETARCH}/vigily-engine /usr/local/bin/

FROM runtime AS release
COPY --from=build /usr/local/bin/vigily /usr/local/bin/vigily-engine /usr/local/bin/

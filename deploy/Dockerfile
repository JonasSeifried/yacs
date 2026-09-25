# syntax=docker/dockerfile:1
# The relay with the PWA built in, as one static binary on a distroless base.
# Build from the repo root:  docker build -f deploy/Dockerfile -t yacs .

ARG RUST_VERSION=1
ARG NODE_VERSION=22

# 1. yacs-core for the browser.
FROM rust:${RUST_VERSION}-slim-bookworm AS wasm
ARG WASM_PACK_VERSION=0.13.1
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates && rm -rf /var/lib/apt/lists/* \
 && rustup target add wasm32-unknown-unknown \
 && curl -sSfL "https://github.com/rustwasm/wasm-pack/releases/download/v${WASM_PACK_VERSION}/wasm-pack-v${WASM_PACK_VERSION}-$(uname -m)-unknown-linux-musl.tar.gz" \
    | tar -xz --strip-components=1 -C /usr/local/bin "wasm-pack-v${WASM_PACK_VERSION}-$(uname -m)-unknown-linux-musl/wasm-pack"
WORKDIR /src
COPY . .
RUN wasm-pack build crates/yacs-wasm --target web --release --out-dir ../../ui/src/wasm --out-name yacs --no-pack

# 2. The PWA.
FROM node:${NODE_VERSION}-slim AS ui
RUN corepack enable
WORKDIR /src
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY ui/package.json ui/
COPY apps/desktop/package.json apps/desktop/
RUN pnpm install --frozen-lockfile --filter @yacs/ui
COPY ui ui
# vite.config.ts reads the workspace version from it.
COPY Cargo.toml ./
COPY --from=wasm /src/ui/src/wasm ui/src/wasm
RUN pnpm --filter @yacs/ui build:web

# 3. The relay, statically linked (musl), with ui/dist/web embedded.
FROM rust:${RUST_VERSION}-alpine AS server
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
COPY --from=ui /src/ui/dist/web ui/dist/web
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p yacs-server \
 && cp target/release/yacs-server /yacs-server \
 && mkdir /data

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=server /yacs-server /yacs-server
# Owned by the nonroot user, so a fresh named volume mounted here is writable.
COPY --from=server --chown=nonroot:nonroot /data /data
ENV YACS_BIND=0.0.0.0:8080 \
    YACS_DATA_DIR=/data
EXPOSE 8080
VOLUME /data
ENTRYPOINT ["/yacs-server"]

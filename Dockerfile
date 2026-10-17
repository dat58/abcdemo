# abcdemo — a Dioxus fullstack build of the abcurves demo.
#
# abcurves is native-only: it reads ~1.5 MB of planner weights off disk and
# needs a real clock, so it is compiled into the server and never into the wasm
# bundle. The browser only ever receives a finished trajectory.
#
# BUILD CONTEXT IS THE PARENT DIRECTORY, because this crate depends on
# ../abcurves-rs by path:
#
#   cd /data/datvt6/projects/aimbot
#   docker build -f abcdemo/Dockerfile -t abcdemo .
#   docker run --rm -p 8080:8080 abcdemo
#
# `Dockerfile.dockerignore` beside this file keeps the two target/ trees and
# abcurves' 70 MB origin/ checkout out of that context.

# ---------------------------------------------------------------------------
# Builder
# ---------------------------------------------------------------------------
# Pin this to a specific patch (rust:1.90-slim-bookworm) for reproducible
# builds; `1-slim-bookworm` tracks the latest stable, which is what the
# workspace is developed against.
FROM rust:1-slim-bookworm AS builder

# `dx` compiles the client to wasm and links the server; wasm-bindgen is
# fetched by `dx` itself on first use, which is why curl and certificates
# are here.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-unknown-unknown

# The CLI and the `dioxus` dependency have to be the same version, or `dx`
# refuses to build. Keep this in step with Cargo.toml.
ARG DIOXUS_VERSION=0.7.6
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo install dioxus-cli --version ${DIOXUS_VERSION} --locked

WORKDIR /src
COPY abcurves-rs/ ./abcurves-rs/
COPY abcdemo/ ./abcdemo/

WORKDIR /src/abcdemo
# No cache mount on target/.
#
# BuildKit normalises the mtimes it gives COPYed files, so a cached target/
# from an earlier build looks *newer* than freshly copied source and cargo
# decides there is nothing to do. The result is an image that silently ships
# the previous build's binary — which is exactly what happened here before
# this comment existed. The registry cache below is content-addressed and
# safe; the build cache is not, so it goes.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    dx bundle --platform web --release \
 && mkdir -p /out \
 && cp -r target/dx/abcdemo/release/web/server target/dx/abcdemo/release/web/public /out/ \
 && strip /out/server

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
# The server links only glibc, libm and libgcc_s, so a slim Debian is enough.
FROM debian:bookworm-slim AS runtime

# curl is here only for the healthcheck; the server makes no outbound calls.
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --system --create-home --uid 10001 abcdemo
WORKDIR /app

# The server serves `public/` from its working directory.
COPY --from=builder /out/server ./server
COPY --from=builder /out/public ./public

# The planner weights. `model_store::default_model_dir()` falls back to the
# path baked in at compile time, which does not exist here, so point it at
# the copy explicitly. All twelve files are carried: the continuous manifest
# hashes every file it declares, including the ONNX graphs the crate never
# evaluates.
COPY abcurves-rs/models/ ./models/
ENV ABCURVES_MODEL_DIR=/app/models

# CSV captures are read from and written to this directory, and the server
# accepts a plain file name rather than a path, so a client cannot reach the
# rest of the filesystem. Mount a volume here to keep exports.
COPY abcdemo/assets/sample_prefix.csv ./data/
RUN chown -R abcdemo:abcdemo /app/data
ENV ABCDEMO_DATA_DIR=/app/data
VOLUME ["/app/data"]

# Dioxus reads these; without IP the server binds 127.0.0.1 and nothing
# outside the container can reach it.
ENV IP=0.0.0.0 \
    PORT=8080 \
    RUST_LOG=info
EXPOSE 8080

USER abcdemo

# `server --help` is not a probe: it has no flags and just tries to bind.
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -fsS "http://127.0.0.1:${PORT}/" -o /dev/null || exit 1

CMD ["./server"]

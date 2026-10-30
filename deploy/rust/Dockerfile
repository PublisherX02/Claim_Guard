# The Rust build of ClaimGuard: one small image for the API and the queue worker (they differ only by command).
# Build from the repository root:
#   docker build -f deploy/rust/Dockerfile -t claimguard-rs:latest .
FROM rust:1-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6 AS build
# Both base images are pinned by digest: the build is the same tomorrow as today. To take security updates, pull the new tags, copy the
# digests (docker buildx imagetools inspect <tag>) here and in compose.yml, rebuild, and run rust/tools/deploy_attack.py.
# the repository pins the Windows toolchain for developers; the container builds for Linux
ENV RUSTUP_TOOLCHAIN=stable CARGO_TERM_COLOR=never
WORKDIR /app
COPY rust ./rust
# the console files are compiled into the server (include_bytes), so they must be at the same relative path
COPY src/access/ui ./src/access/ui
# a small build machine can limit the compiler's parallelism:  docker build --build-arg CARGO_FLAGS="-j 2" ...
ARG CARGO_FLAGS=""
RUN cd rust && cargo build --release -p cg-cli --locked $CARGO_FLAGS && strip target/release/cg
RUN mkdir -p /out/data /out/mirror

# A distroless base: the C library, the compiler runtime and certificates, and nothing else. No shell, no package manager, no mount or
# archive tools, no perl: an intruder who gets code running in the container finds almost nothing to use, and a vulnerability scan has
# almost nothing to report.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
ENV CLAIMGUARD_ROOT=/app CLAIMGUARD_DATA_DIR=/data CG_WORKER_ALIVE_FILE=/tmp/cg_worker_alive
WORKDIR /app
COPY --from=build /app/rust/target/release/cg /usr/local/bin/cg
COPY rules ./rules
COPY data/development/claims.jsonl ./data/development/claims.jsonl
# the console is compiled into the binary; the tree below only lets the server find its own folder
COPY src/access/ui ./src/access/ui
# the two volumes start owned by the unprivileged user (a new named volume copies the ownership of the folder it is mounted on)
COPY --from=build --chown=65532:65532 /out/data /data
COPY --from=build --chown=65532:65532 /out/mirror /mirror

USER 65532:65532
VOLUME ["/data"]
EXPOSE 8443

HEALTHCHECK --interval=15s --timeout=4s --start-period=20s --retries=5 CMD ["cg", "health", "--port", "8443"]

# The API listens inside the private network only; the proxy in front terminates TLS (see compose.yml).
CMD ["cg", "serve", "--host", "0.0.0.0", "--port", "8443", "--queue", "--behind-proxy"]

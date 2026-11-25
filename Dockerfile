# Build one daemon or the emob CLI per target. CI verifies the base image digests.
FROM rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS build
ARG DAEMON
ARG BIN=${DAEMON}
WORKDIR /src

# The manifests first, so a source-only change does not re-resolve the graph.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY deploy/estate.example.toml deploy/recovery-drill.py ./deploy/
COPY crates ./crates
COPY services ./services
COPY xtask ./xtask

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p "${DAEMON}" \
 && cp "target/release/${BIN}" /usr/local/bin/daemon

# The journal directory, created here because the runtime image has no shell to
# create it with — and, more to the point, **owned by the user the daemon runs
# as**. Docker initialises a new named volume from whatever the image has at
# that path, ownership included; without this the volume arrives owned by root,
# the non-root daemon cannot open its own journal, and the process refuses to
# start. Found by running the image, not by reading it.
RUN mkdir -p /out/var/lib/emob && chown -R 65532:65532 /out/var/lib

# ── run ─────────────────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS runtime
ARG DAEMON
COPY --from=build /usr/local/bin/daemon /usr/local/bin/daemon

# Provide a writable journal mount for the nonroot runtime.
COPY --from=build --chown=65532:65532 /out/var/lib/emob /var/lib/emob
VOLUME ["/var/lib/emob"]
ENV EMOB_JOURNAL_DIR=/var/lib/emob

# Services can override the shared container address with their own bind setting.
ENV EMOB_HTTP_BIND=0.0.0.0:8080
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/daemon"]

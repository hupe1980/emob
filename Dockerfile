# Build one daemon or the emob CLI per target. CI verifies the base image digests.
FROM rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS build
WORKDIR /src

# Copy the workspace and the fixtures embedded by the CLI.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY deploy/estate.example.toml deploy/recovery-drill.py ./deploy/
COPY crates ./crates
COPY services ./services
COPY xtask ./xtask

# Bake shares this layer across the whole estate. Cargo resolves one feature
# graph and links each runtime executable once, including the CLI.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --workspace --bins --exclude xtask \
 && mkdir -p /out/bin \
 && install target/release/emob /out/bin/ \
 && for service in services/*; do install "target/release/${service##*/}" /out/bin/; done

# New named volumes inherit this ownership for the nonroot runtime.
RUN mkdir -p /out/var/lib/emob && chown -R 65532:65532 /out/var/lib

# ── run ─────────────────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS runtime
ARG DAEMON
ARG BIN=${DAEMON}
COPY --from=build /out/bin/${BIN} /usr/local/bin/daemon

# Keep writable journal and customer key mounts separate.
COPY --from=build --chown=65532:65532 /out/var/lib/emob /var/lib/emob
COPY --from=build --chown=65532:65532 /out/var/lib/emob /var/lib/emob-custody
VOLUME ["/var/lib/emob"]
ENV EMOB_JOURNAL_DIR=/var/lib/emob
ENV EMPD_CUSTODY_DIR=/var/lib/emob-custody

# Services can override the shared container address with their own bind setting.
ENV EMOB_HTTP_BIND=0.0.0.0:8080
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/daemon"]

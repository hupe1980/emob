# The image every daemon ships as.
#
# One Dockerfile and one build stage for all nine — and for `emob`, the CLI
# whose `emob demo` runs the whole estate without a Rust toolchain (spec 011) —
# selected by `--build-arg DAEMON` (the package; `BIN` names its binary when
# that differs, as `emob-cli`'s `emob` does), because they are the same binary shape: a static-ish Rust executable,
# a journal directory, and two ports. Eight Dockerfiles would be eight copies of
# the same six lines, and the copy that is wrong is the one nobody re-reads.
#
# `distroless/cc` rather than a shell image: the runtime needs libgcc and libc
# and nothing else — no package manager, no shell, no `curl` for an attacker who
# reaches the container. The health check is the orchestrator's job over HTTP,
# which is what `/health/live` and `/health/ready` are for and why there is no
# `HEALTHCHECK` here needing a binary to run it.

# Both base images are pinned by **digest**, with the tag beside it for the
# reader. A tag is a pointer its owner can move, and every image the release
# pushes would be built on wherever it pointed that day; `cargo xtask
# check-actions` refuses a `FROM` without one. Moving a pin is a hand edit:
# `docker buildx imagetools inspect <image>:<tag>` prints the new digest.

# ── build ───────────────────────────────────────────────────────────────────
FROM rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS build
ARG DAEMON
ARG BIN=${DAEMON}
WORKDIR /src

# The manifests first, so a source-only change does not re-resolve the graph.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY services ./services
COPY xtask ./xtask

RUN --mount=type=cache,target=/usr/local/cargo/registry \
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

# Where the journal goes. A service whose ledger is a fact about money writes it
# here, and an operator mounts a volume over it — an unmounted container is a
# service whose journal dies with it, which is the failure the journal exists to
# prevent, so the path is declared rather than assumed.
COPY --from=build --chown=65532:65532 /out/var/lib/emob /var/lib/emob
VOLUME ["/var/lib/emob"]
ENV EMOB_JOURNAL_DIR=/var/lib/emob

# Where the shell listens. Every daemon defaults to **loopback**, which is right
# on a laptop and reachable by nothing inside a container — an image that did not
# set this comes up healthy, logs that it is listening, and answers no probe.
# A daemon's own `<DAEMON>_HTTP_BIND` still wins over it, and a daemon with a
# second socket names that one separately, because applying one address to two
# sockets binds the second to the first's port.
ENV EMOB_HTTP_BIND=0.0.0.0:8080
EXPOSE 8080
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/daemon"]

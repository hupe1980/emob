# Every daemon, one build.
#
# `docker buildx bake` builds the nine images from the one Dockerfile, each
# with its own `DAEMON` build argument. The list is here rather than in a shell
# loop so that `bake --print` is the answer to *what does this workspace ship*.
#
# The release workflow's `images` job runs this file on a `v*` tag with
# `VERSION` set from the tag, pushes to ghcr.io and lets BuildKit attach its
# provenance. One platform, `linux/amd64`: an arm64 build of nine Rust daemons
# under emulation costs hours per release, and nobody has asked for one yet.

variable "REGISTRY" {
  default = "ghcr.io/hupe1980"
}

variable "VERSION" {
  default = "dev"
}

# The nine. `csmsd` and `sited` are the two that hold a socket somebody else
# dials; the other seven answer HTTP and a schedule.
variable "DAEMONS" {
  default = ["csmsd", "roamd", "billd", "empd", "poid", "tarifd", "sited", "agentd", "deskd"]
}

group "default" {
  targets = ["daemons", "emob"]
}

target "daemons" {
  name       = item
  matrix     = { item = DAEMONS }
  dockerfile = "Dockerfile"
  context    = "."
  args       = { DAEMON = item }
  tags       = [
    "${REGISTRY}/emob-${item}:${VERSION}",
    "${REGISTRY}/emob-${item}:latest",
  ]
  platforms  = ["linux/amd64"]
  labels = {
    "org.opencontainers.image.source"      = "https://github.com/hupe1980/emob"
    "org.opencontainers.image.licenses"    = "MIT OR Apache-2.0"
    "org.opencontainers.image.title"       = "emob-${item}"
    "org.opencontainers.image.description" = "emob — the e-mobility operating stack: ${item}"
  }
}

# `emob` — the CLI, so `just demo` runs without a Rust toolchain (spec 011):
# `docker run ghcr.io/hupe1980/emob demo`. Same Dockerfile, same pinned bases,
# same release job and provenance as the daemons (SR-133).
target "emob" {
  dockerfile = "Dockerfile"
  context    = "."
  args       = { DAEMON = "emob-cli", BIN = "emob" }
  tags       = [
    "${REGISTRY}/emob:${VERSION}",
    "${REGISTRY}/emob:latest",
  ]
  platforms  = ["linux/amd64"]
  labels = {
    "org.opencontainers.image.source"      = "https://github.com/hupe1980/emob"
    "org.opencontainers.image.licenses"    = "MIT OR Apache-2.0"
    "org.opencontainers.image.title"       = "emob"
    "org.opencontainers.image.description" = "emob — the e-mobility operating stack: the emob CLI (emob demo)"
  }
}

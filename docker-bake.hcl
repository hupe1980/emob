# Every daemon, one build.
#
# `docker buildx bake` builds the eight images from the one Dockerfile, each
# with its own `DAEMON` build argument. The list is here rather than in a shell
# loop so that `bake --print` is the answer to *what does this workspace ship*.

variable "REGISTRY" {
  default = "ghcr.io/hupe1980"
}

variable "VERSION" {
  default = "dev"
}

# The eight. `csmsd` and `sited` are the two that hold a socket somebody else
# dials; the other six answer HTTP and a schedule.
variable "DAEMONS" {
  default = ["csmsd", "roamd", "billd", "empd", "poid", "tarifd", "sited", "agentd"]
}

group "default" {
  targets = ["daemons"]
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
  platforms  = ["linux/amd64", "linux/arm64"]
  labels = {
    "org.opencontainers.image.source"      = "https://github.com/hupe1980/emob"
    "org.opencontainers.image.licenses"    = "MIT OR Apache-2.0"
    "org.opencontainers.image.title"       = "emob-${item}"
    "org.opencontainers.image.description" = "emob — the e-mobility operating stack: ${item}"
  }
}

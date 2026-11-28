#!/usr/bin/env bash
# Exercise the two packaged entry points with the kit from the same revision.
set -euo pipefail
cli_image=${1:?expected the emob CLI image tag}
tariff_image=${2:?expected the tarifd image tag}
emobtest conformance mcp -- docker run --rm -i --network=none "$cli_image" mcp

# Only the test client can reach the ephemeral published port on this host.
container=$(docker run -d --rm -p 127.0.0.1::8080 \
    -v "$PWD/deploy/estate.example.toml:/estate.toml:ro" \
    -e EMOB_ESTATE=/estate.toml \
    -e "EMOB_OPERATOR_TOKEN=$(openssl rand -hex 32)" \
    -e "EMOB_DESK_READ_TOKEN=$(openssl rand -hex 32)" "$tariff_image")
trap 'docker rm -f "$container" >/dev/null 2>&1 || true' EXIT
url="http://$(docker port "$container" 8080/tcp)"
curl --silent --show-error --fail --retry-all-errors --retry 10 --retry-delay 1 \
    --max-time 3 --output /dev/null "$url/health/ready"
emobtest conformance tariff "$url" --tariff-id ad-hoc

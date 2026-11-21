#!/usr/bin/env bash
# Publish one crate; a version already on crates.io is a success, so a re-run
# skips what went up. A dependency published a step earlier may not be in the
# index yet ("failed to select a version"): wait for it, up to five minutes.
set -uo pipefail
crate="$1"
for attempt in $(seq 10); do
    out=$(cargo publish -p "$crate" --locked 2>&1) && exit 0
    echo "$out"
    echo "$out" | grep -qE "already uploaded|already exists on crates\.io" && exit 0
    echo "$out" | grep -q "failed to select a version" || exit 1
    echo "⏳ a dependency is not in the index yet — attempt $attempt of 10, again in 30 s"
    sleep 30
done
exit 1

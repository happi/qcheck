#!/usr/bin/env bash
# Runs cargo inside a Rust container, for hosts without a Rust toolchain.
# Bind mounts do not work where the docker daemon runs on another host, so
# the tree is copied in with `docker cp'. Nothing is copied back.
#
#   scripts/dtest.sh                 cargo test
#   scripts/dtest.sh build           any other cargo subcommand and arguments
#   QC_SEED=42 scripts/dtest.sh      replay a reported failure
set -u

DIR="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE="${QCHECK_RUST_IMAGE:-rust:1-slim}"
NAME="qcheck-dtest-$$-$(date +%s)"

[ $# -eq 0 ] && set -- test

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM

docker create --name "$NAME" -e QC_SEED -e QC_TESTS -w /src \
    "$IMAGE" cargo "$@" >/dev/null || exit 1
tar -C "$DIR" --exclude=./target --exclude=./.git -cf - . \
    | docker cp - "$NAME":/src || exit 1
docker start -a "$NAME"

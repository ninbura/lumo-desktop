#!/usr/bin/env bash
#
# Run the bridge E2E suite: real Rust core, real bridge, real web-client module, driven
# through WebDriver inside a Linux container. Only docker and git are needed on the host.
#
# Env:
#   CLIENTS_DIR       clients checkout to build the stub against (default: ../clients)
#   IMAGE             image tag to build/use (default: lumo-e2e:latest)
#   NO_CACHE          set to rebuild the image from scratch
#   LUMO_E2E_TIMEOUT  seconds before the spec stage is killed (default: 600)

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
clients_dir=${CLIENTS_DIR:-$repo_root/../clients}
image=${IMAGE:-lumo-e2e:latest}

if [ ! -d "$clients_dir" ]; then
    echo "run: clients checkout not found at $clients_dir (set CLIENTS_DIR)" >&2
    exit 1
fi
clients_dir=$(cd "$clients_dir" && pwd)

echo "=== building $image"
docker build ${NO_CACHE:+--no-cache} -f "$repo_root/e2e/Dockerfile" -t "$image" "$repo_root/e2e"

# Named volumes keep the linux node_modules and cargo artifacts out of the mounted
# working tree: a linux esbuild binary would otherwise overwrite the host's, and the
# linux target dir would force a full rebuild on the host after every run.
echo "=== running suite"
exec docker run --rm --init \
    -v "$repo_root":/work \
    -v "$clients_dir":/clients:ro \
    -v lumo-e2e-cargo:/cargo \
    -v lumo-e2e-target:/target \
    -v lumo-e2e-node-root:/work/node_modules \
    -v lumo-e2e-node-e2e:/work/e2e/node_modules \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e CLIENTS_DIR=/clients \
    -e LUMO_E2E_ARTIFACT_DIR=/work/build/e2e \
    -e LUMO_APPROVAL_TIMEOUT_MS="${LUMO_APPROVAL_TIMEOUT_MS:-5000}" \
    -e LUMO_E2E_TIMEOUT="${LUMO_E2E_TIMEOUT:-600}" \
    -e GIT_CONFIG_COUNT=1 \
    -e GIT_CONFIG_KEY_0=safe.directory \
    -e GIT_CONFIG_VALUE_0='*' \
    "$image" \
    /work/e2e/run-in-container.sh "$@"

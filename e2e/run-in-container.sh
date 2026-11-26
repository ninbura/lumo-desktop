#!/usr/bin/env bash
#
# The E2E run itself, inside a Linux environment with the toolchain already present:
# bundle the stub, build the app, drive it with WebdriverIO under Xvfb.
#
# Two callers, and this is the only description of the steps: run.sh supplies the
# container locally, and the CI job is already in one. Both must reach here rather than
# restating the steps, or the two drift.
#
# Expects CLIENTS_DIR to point at a clients checkout (run.sh mounts it; CI clones it).

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

echo "=== installing host tooling (esbuild)"
npm ci --no-audit --no-fund

echo "=== installing the webdriver suite"
npm ci --prefix e2e --no-audit --no-fund

echo "=== running specs"
# WebKitGTK needs a display; the app needs a session bus for tray/appindicator. The
# native runner builds the stub and test binary, seeds isolated fixtures, and applies
# a timeout to each spec. The same steps run locally and in CI.
exec xvfb-run -a dbus-run-session -- node e2e/run-native.mjs "$@"

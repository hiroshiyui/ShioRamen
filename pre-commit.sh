#!/usr/bin/env bash
# pre-commit.sh — install the project's git pre-commit hook.
#
# Run once after cloning:
#   bash pre-commit.sh
#
# This sets core.hooksPath to .githooks/ so git picks up the committed
# hook that runs fmt → clippy → test before every commit.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
git -C "$ROOT" config core.hooksPath .githooks
echo "pre-commit hook installed (.githooks/pre-commit)"

#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="cc-workbench"
TARGET_BIN="${1:-$(pwd)/target/release/cc-workbench}"

CLAUDE_PATH="$(command -v claude || true)"
if [[ -z "${CLAUDE_PATH}" ]]; then
  echo "claude not found in PATH" >&2
  exit 1
fi

CLAUDE_DIR="$(dirname "${CLAUDE_PATH}")"
REAL_CLAUDE="${CLAUDE_DIR}/claude.real"

if [[ ! -x "${TARGET_BIN}" ]]; then
  echo "build binary not found: ${TARGET_BIN}" >&2
  exit 1
fi

if [[ ! -e "${REAL_CLAUDE}" ]]; then
  mv "${CLAUDE_PATH}" "${REAL_CLAUDE}"
fi

cp "${TARGET_BIN}" "${CLAUDE_DIR}/claude"
chmod +x "${CLAUDE_DIR}/claude"

echo "Installed cc-workbench wrapper at ${CLAUDE_DIR}/claude"

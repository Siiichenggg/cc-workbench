#!/usr/bin/env bash
set -euo pipefail

CLAUDE_PATH="$(command -v claude || true)"
if [[ -z "${CLAUDE_PATH}" ]]; then
  echo "claude not found in PATH" >&2
  exit 1
fi

CLAUDE_DIR="$(dirname "${CLAUDE_PATH}")"
REAL_CLAUDE="${CLAUDE_DIR}/claude.real"

if [[ -e "${REAL_CLAUDE}" ]]; then
  rm -f "${CLAUDE_PATH}"
  mv "${REAL_CLAUDE}" "${CLAUDE_PATH}"
  echo "Restored claude from claude.real"
else
  echo "claude.real not found; nothing to restore"
fi

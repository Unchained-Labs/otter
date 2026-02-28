#!/usr/bin/env bash
set -euo pipefail

OTTER_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OTTER_ENV_FILE="${OTTER_ROOT}/.env"
SOURCE_VIBE_HOME="/home/wardn/.vibe"
TARGET_VIBE_HOME="${HOME}/.vibe"

mkdir -p "${TARGET_VIBE_HOME}"

if [ -d "${SOURCE_VIBE_HOME}" ]; then
  cp -a "${SOURCE_VIBE_HOME}/." "${TARGET_VIBE_HOME}/"
fi

if [ -f "${OTTER_ENV_FILE}" ]; then
  # shellcheck source=/dev/null
  source "${OTTER_ENV_FILE}"
fi

if [ -n "${MISTRAL_API_KEY:-}" ]; then
  touch "${TARGET_VIBE_HOME}/.env"
  if rg -n "^MISTRAL_API_KEY=" "${TARGET_VIBE_HOME}/.env" >/dev/null 2>&1; then
    sed -i "s|^MISTRAL_API_KEY=.*$|MISTRAL_API_KEY=${MISTRAL_API_KEY}|" "${TARGET_VIBE_HOME}/.env"
  else
    printf "\nMISTRAL_API_KEY=%s\n" "${MISTRAL_API_KEY}" >> "${TARGET_VIBE_HOME}/.env"
  fi
else
  echo "MISTRAL_API_KEY is not set in ${OTTER_ENV_FILE}" >&2
  exit 1
fi

echo "Host vibe home prepared at ${TARGET_VIBE_HOME}"

#!/usr/bin/env bash
set -euo pipefail

# Installs mistral-vibe into the current user profile.
curl -LsSf https://mistral.ai/vibe/install.sh | bash

if [ -x "${HOME}/.local/bin/vibe" ]; then
  echo "mistral-vibe installed at ${HOME}/.local/bin/vibe"
else
  echo "mistral-vibe install finished, but vibe binary not found in ~/.local/bin" >&2
  exit 1
fi

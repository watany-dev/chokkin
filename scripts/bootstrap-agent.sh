#!/usr/bin/env bash
# Install ptuf (pre-tool-use filter) for agent guardrail hooks.
# Usage: bash scripts/bootstrap-agent.sh
set -euo pipefail

PTUF_VERSION="${PTUF_VERSION:-v0.3.0}"

echo "Installing ptuf ${PTUF_VERSION}..."
curl -LsSf "https://github.com/watany-dev/ptuf/releases/download/${PTUF_VERSION}/ptuf-installer.sh" | sh

export PATH="${HOME}/.cargo/bin:${PATH}"

if ptuf --version >/dev/null 2>&1; then
  echo "ptuf installed: $(ptuf --version)"
else
  echo "ERROR: ptuf installation failed." >&2
  exit 1
fi

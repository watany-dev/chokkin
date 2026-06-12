#!/usr/bin/env bash
set -euo pipefail

if [ -x "${HOME}/.cargo/bin/ptuf" ]; then
  PTUF="${HOME}/.cargo/bin/ptuf"
elif PTUF="$(command -v ptuf 2>/dev/null)"; then
  :
else
  echo "ptuf is not installed. Run 'bash scripts/bootstrap-agent.sh' to install it." >&2
  exit 1
fi

exec "${PTUF}" hook cursor

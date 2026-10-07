#!/usr/bin/env bash
# Install ptuf (pre-tool-use filter) for agent guardrail hooks.
# Usage: bash scripts/bootstrap-agent.sh
#
# The installer is downloaded to a temp file and only run if its SHA256 matches
# the pin below. It embeds the SHA256 of every release archive and checks it
# before unpacking, so pinning the installer pins the ptuf binary too.
# To bump: set PTUF_VERSION and PTUF_INSTALLER_SHA256 to the new release's
# ptuf-installer.sh (`curl -LsSf <url> | sha256sum`).
set -euo pipefail

PTUF_VERSION="${PTUF_VERSION:-v0.3.0}"
PTUF_INSTALLER_SHA256="${PTUF_INSTALLER_SHA256:-8f3144b0a588c24f1feaaa8c438a0d17aa165ec635e92caad30a85b7eac5316c}"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
installer="$tmp/ptuf-installer.sh"

echo "Installing ptuf ${PTUF_VERSION}..."
curl --proto '=https' --tlsv1.2 -LsSf -o "$installer" \
  "https://github.com/watany-dev/ptuf/releases/download/${PTUF_VERSION}/ptuf-installer.sh"

actual="$(sha256 "$installer")"
if [ "$actual" != "$PTUF_INSTALLER_SHA256" ]; then
  echo "ERROR: ptuf-installer.sh SHA256 mismatch." >&2
  echo "  expected: $PTUF_INSTALLER_SHA256" >&2
  echo "  actual:   $actual" >&2
  exit 1
fi

sh "$installer"

export PATH="${HOME}/.cargo/bin:${PATH}"

if ptuf --version >/dev/null 2>&1; then
  echo "ptuf installed: $(ptuf --version)"
else
  echo "ERROR: ptuf installation failed." >&2
  exit 1
fi

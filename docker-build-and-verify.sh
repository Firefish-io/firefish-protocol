#!/bin/bash
# Docker-based reproducible build and verification of the borrower WASM.
# See README.md for usage instructions.
#
# Usage: ./docker-build-and-verify.sh [path-to-local-borrower.wasm]
# Without an argument the build is verified against the binary published on
# the website; with one it is verified against the given local file.

set -euo pipefail

# macOS: prevent AppleDouble (._*) files from polluting the build context.
export COPYFILE_DISABLE=1

# The published binary is built on linux/amd64 (see .github/workflows/nix.yml).
# Any other platform can produce a different hash from the same source, so the
# container is forced to linux/amd64 (emulated where necessary).
PLATFORM=linux/amd64

WEBSITE_WASM_URL="https://app.firefish.io/borrower_wasm.wasm"

# Prefer Docker, fall back to Podman.
if command -v docker >/dev/null 2>&1; then
  ENGINE=docker
elif command -v podman >/dev/null 2>&1; then
  ENGINE=podman
else
  echo "Error: neither docker nor podman found. Please install one of them." >&2
  exit 1
fi

# Optional: verify against a locally downloaded binary instead of the website.
LOCAL_REMOTE_WASM="${1:-}"
if [ -n "$LOCAL_REMOTE_WASM" ] && [ ! -f "$LOCAL_REMOTE_WASM" ]; then
  echo "Error: file not found: $LOCAL_REMOTE_WASM" >&2
  exit 1
fi

# The build embeds the current git commit, so a tree with changes to tracked
# files cannot produce the published binary. Untracked files (like these docker
# helpers copied onto an older checkout) are fine and do not affect the build.
if command -v git >/dev/null 2>&1 && \
   [ -n "$(git status --porcelain 2>/dev/null | grep -v '^??' || true)" ]; then
  echo "WARNING: you have uncommitted changes to tracked files."
  echo "The build embeds the git commit, so the resulting binary cannot match"
  echo "the published one. Commit or stash your changes first, or check out the"
  echo "commit the published binary was built from."
  echo ""
fi

# Pre-build commit check (optional, requires Nix on the host).
if [ -z "$LOCAL_REMOTE_WASM" ] && command -v nix >/dev/null 2>&1 && command -v curl >/dev/null 2>&1; then
  echo "Checking if local commit matches published binary..."
  PUBLISHED_COMMIT=$(curl -sL "$WEBSITE_WASM_URL" | \
    nix run .#wasm-commit --extra-experimental-features 'nix-command flakes' -- --read /dev/stdin 2>/dev/null || echo "")

  if [ -n "$PUBLISHED_COMMIT" ]; then
    CURRENT_COMMIT=$(git rev-parse HEAD 2>/dev/null || echo "unknown")

    if [ "$PUBLISHED_COMMIT" != "$CURRENT_COMMIT" ]; then
      echo ""
      echo "WARNING: you are not on the same commit as the published binary!"
      echo ""
      echo "Current commit:   $CURRENT_COMMIT"
      echo "Published commit: $PUBLISHED_COMMIT"
      echo ""
      echo "To build the verifiable binary, run:"
      echo "  git checkout $PUBLISHED_COMMIT"
      echo ""
      REPLY=""
      read -p "Continue anyway? (y/N) " -n 1 -r || true
      echo
      if [[ ! "$REPLY" =~ ^[Yy]$ ]]; then
        exit 1
      fi
    else
      echo "Commit matches published binary ($CURRENT_COMMIT)"
    fi
  else
    echo "Could not determine the published commit (offline?) - continuing anyway."
  fi
  echo ""
fi

# Build and extract artifacts directly (no image is saved).
rm -rf ./docker-wasm-output
mkdir -p ./docker-wasm-output

echo "Building WASM inside $ENGINE ($PLATFORM)..."
"$ENGINE" build --platform "$PLATFORM" -f Dockerfile.nix \
  --output type=local,dest=./docker-wasm-output .

BUILT_WASM=./docker-wasm-output/firefish_borrower_wasm_bg.wasm
if [ ! -f "$BUILT_WASM" ]; then
  echo "Error: build did not produce $BUILT_WASM" >&2
  exit 1
fi

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Determine what to compare against.
if [ -n "$LOCAL_REMOTE_WASM" ]; then
  COMPARE_WASM="$LOCAL_REMOTE_WASM"
  COMPARE_LABEL="Local WASM:   "
  echo "Using local WASM: $COMPARE_WASM"
else
  command -v curl >/dev/null 2>&1 || {
    echo "Error: curl not found. Download the published binary manually" >&2
    echo "  $WEBSITE_WASM_URL" >&2
    echo "and re-run: ./docker-build-and-verify.sh path-to-downloaded.wasm" >&2
    exit 1
  }
  echo "Downloading published WASM from the website..."
  COMPARE_WASM=$(mktemp)
  trap 'rm -f "$COMPARE_WASM"' EXIT
  if ! curl -fsSL "$WEBSITE_WASM_URL" -o "$COMPARE_WASM"; then
    echo "Error: failed to download WASM from $WEBSITE_WASM_URL" >&2
    echo "Download it manually and re-run:" >&2
    echo "  ./docker-build-and-verify.sh path-to-downloaded.wasm" >&2
    exit 1
  fi
  COMPARE_LABEL="Website:      "
fi

echo ""
echo "Calculating SHA-256 hashes..."
BUILT_HASH=$(sha256 "$BUILT_WASM")
COMPARE_HASH=$(sha256 "$COMPARE_WASM")

echo ""
echo "Docker build:   $BUILT_HASH"
echo "$COMPARE_LABEL   $COMPARE_HASH"
echo ""

if [ "$BUILT_HASH" = "$COMPARE_HASH" ]; then
  echo "SUCCESS: Hashes match! The published binary is verified."
else
  echo "FAILURE: Hashes do not match!"
  echo ""
  echo "This could mean:"
  echo "  - You are not comparing the right commit (run 'git log -1' and check"
  echo "    the commit the published binary was built from)"
  echo "  - The build is not reproducible"
  echo "  - The binary has been tampered with"
  exit 1
fi

echo ""
echo "WASM artifacts extracted to:"
echo "  ./docker-wasm-output/"

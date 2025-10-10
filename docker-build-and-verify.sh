#!/bin/bash
# Docker-based build
# See README.md for usage instructions

set -e

# Check if we're on the correct commit before building (requires Nix)
if command -v nix &> /dev/null; then
  echo "Checking if local commit matches published binary..."
  PUBLISHED_COMMIT=$(curl -sL "https://app.firefish.io/borrower_wasm.wasm" | \
    nix run .#wasm-commit --extra-experimental-features 'nix-command flakes' -- --read /dev/stdin 2>/dev/null || echo "")

  if [ -n "$PUBLISHED_COMMIT" ]; then
    CURRENT_COMMIT=$(git rev-parse HEAD 2>/dev/null || echo "unknown")

    if [ "$PUBLISHED_COMMIT" != "$CURRENT_COMMIT" ]; then
      echo ""
      echo "WARNING: You are not on the same commit as the published binary!"
      echo ""
      echo "Current commit:   $CURRENT_COMMIT"
      echo "Published commit: $PUBLISHED_COMMIT"
      echo ""
      echo "To build the verifiable binary, run:"
      echo "  git checkout $PUBLISHED_COMMIT"
      echo ""
      read -p "Continue anyway? (y/N) " -n 1 -r
      echo
      if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
      fi
    else
      echo "✓ Commit matches published binary ($CURRENT_COMMIT)"
    fi
  fi
  echo ""
else
  echo "Note: Nix not found on host - skipping pre-build commit check"
  echo "Commit verification will happen inside Docker after build completes"
  echo ""
fi

# Build and extract artifacts directly (no image saved)
rm -rf ./docker-wasm-output
mkdir -p ./docker-wasm-output
#docker build -f Dockerfile.nix --output type=local,dest=./docker-wasm-output .
#podman build --platform linux/amd64 --security-opt seccomp=unconfined -f Dockerfile.nix .
podman build --platform linux/amd64 -f Dockerfile.nix .

echo ""
echo "=========================================="
echo "Docker Build Verification Result:"
echo "=========================================="
cat ./docker-wasm-output/verification.txt
echo ""
echo "WASM artifacts extracted to:"
echo "  ./docker-wasm-output/"

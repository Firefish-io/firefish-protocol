# Reproducible build of the borrower WASM inside a pinned container.
#
# The published binary is built for linux/amd64 (see .github/workflows/nix.yml),
# so the platform is enforced by docker-build-and-verify.sh (--platform linux/amd64).
# Building for any other platform can produce a byte-different binary even from
# the exact same source.
#
# See README.md for usage instructions.

FROM docker.io/nixos/nix:2.31.2 AS builder

WORKDIR /build

# Copy only what's needed for the Nix build.
# .git is required: the build embeds the current commit into the WASM.
COPY flake.nix flake.lock rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY .git .git
COPY src src
COPY borrower-wasm borrower-wasm
COPY contrib contrib
COPY cli cli

# Ensure git recognizes the directory (needed for the flake to resolve the
# current commit and for `git rev-parse` in verify-wasm).
RUN git config --global --add safe.directory /build || true

# Show which platform we are building for. This must print "x86_64" for the
# resulting hashes to match the published binary.
RUN uname -m && nix --version

# Build WASM in an isolated environment.
# filter-syscalls is disabled for compatibility with rootless podman.
RUN nix build .#borrower-wasm \
      --extra-experimental-features 'nix-command flakes' \
      --print-build-logs \
      --option filter-syscalls false

# Collect artifacts and checksums into /output.
RUN mkdir -p /output && \
    cp -rL result/lib/* /output/ && \
    sha256sum /output/*.wasm | tee /output/checksums.txt

# Final stage: only the output artifacts (for `docker build --output`).
FROM scratch
COPY --from=builder /output/ /

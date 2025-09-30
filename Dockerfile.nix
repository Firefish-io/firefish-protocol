FROM nixos/nix:2.31.2 AS builder

WORKDIR /build

# Copy only what's needed for the Nix build
COPY flake.nix flake.lock rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY .git .git
COPY src src
COPY borrower-wasm borrower-wasm
COPY contrib contrib
COPY cli cli

# Ensure git recognizes the directory (needed for git rev-parse in verify-wasm)
RUN git config --global --add safe.directory /build || true

# Build WASM in isolated environment
RUN nix build .#borrower-wasm --extra-experimental-features 'nix-command flakes' --print-build-logs

# Copy artifacts to output directory
RUN mkdir -p /output && \
    cp -rL result/lib/* /output/

# Generate checksums and show them
RUN sha256sum /output/*.wasm | tee /output/checksums.txt

# Verify against published binary and save result
RUN nix run .#verify-wasm --extra-experimental-features 'nix-command flakes' > /output/verification.txt 2>&1 || \
    echo "Verification failed or skipped (no network?)" > /output/verification.txt

# Final stage: only the output artifacts (for docker build -o)
FROM scratch
COPY --from=builder /output/ /
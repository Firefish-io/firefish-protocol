# Firefish core library

This library implements the main logic of the Firefish smart contract: creating offers, accepting offers, (pre)signing transactions, and other processing logic.
It also contains serialization code, an example CLI application and WASM bindings for the borrower.

The code here is deployed in production and can be verified.

**Note: external contributions will be allowed in the future.**
This code is only provided for review and auditing, not for development.
We have some changes prepared that will improve the development use case (while breaking the API of this one).
If you're interested in development in any way don't hesitate to contact us.

## Requirements

The library was tested with Rust 1.63 (available on Debian 12 via `apt`). It may be usable with lower versions but definitely not below 1.56.1 (MSRV of `rust-bitcoin`).
Due to internal use of secp256k1, a working C compiler is also required. Stock gcc on Debian 12 works fine.

## Testing

A simple test script is provided for creating the escrow transaction.
The script expects you to build the CLI using `cargo build` (change the path in it if you're doing a release build).
The script executes all CLI commands and passes information between them, then waits for a raw transaction.

It is possible to make it run automatically, invoking `bitcoin-cli -chain=regtest`, or do it manually.

To test it, you need to spin up a regtest node, generate some sats, and send them to the funding address provided in the output.
Enter the raw hex transaction (e.g., from Core via `getrawtransaction`) and end the input (double CTRL+D or return and then CTRL+D).
This will generate a transaction that you can broadcast using `bitcoin-cli -chain=regtest sendtransaction PUT_HEX_HERE 0`.
The script uses a deliberately high fee rate of 1000 sat/vb because regtest requires higher fees by default.

*Note that the script creates temporary files which it doesn't clean up!*
This is intentional to allow inspection.
The files aren't large, but avoid running the script repeatedly.

You're also free to change any of the hard-coded addresses in the script.
Interestingly, they don't require private keys for the *contract* to work.

If you want to explore the raw CLI commands and try them out manually (perhaps simulating a real setup with multiple machines), reading the script should be a good starting point.
More documentation will come eventually.

### Cancellation testing

To test canceling the transaction from prefund, run the script as you would normally.
However, do **not** provide the funding transaction.
Instead, kill the script using CTRL+C once you have the funding transaction.
The first line of output shows the borrower state file location:
`/tmp/tmp.SOME_RANDOM_STRING/borrower.state`

Run `./target/debug/examples/cli prefund cancel STATE_FILE_HERE FEE_RATE_SAT_VB`.
I recommend using 1000 as the fee rate on regtest.
Provide the funding transaction to this command.
It will generate the cancellation transaction.

Note that you must mine enough blocks after the funding transaction is confirmed before the cancellation transaction becomes valid.
If you used the script unchanged, the relative lock time is 42 blocks.
Running `bitcoin-cli generatetoaddress 42 SOME_ADDRESS` will enable inclusion of the cancellation transaction.
Use `sendrawtransaction` as usual.

### Finalization testing

To test repayment and default transactions, run the script normally and let it finish.
The first line of output shows the ted-p state file location:
`/tmp/tmp.SOME_RANDOM_STRING/ted-p.state`

Run `./target/debug/examples/cli escrow repayment STATE_FILE_HERE`.
Enter the base64-encoded signatures from TED-O (the longer string in the script output).
Broadcast the resulting transaction.
For default transactions, replace `repayment` with `default` in the command.

For liquidation, you must first run the `escrow liquidation` subcommand, supplying the TED-O state file.
This is the same as above, but replace `ted-p.state` with `ted-o.state`.
Then run `escrow liquidation` as you would run `repayment` or `default`, but enter the signature produced by the previous `escrow liquidation` step instead.

## Development

The library API is unstable and definitely going to change.
The plan is to have a more stable version that encapsulates the instability into a simple API - just exchanging messages, configuration and state transitions.
This is already mostly done in a different branch but not yet properly tested and not fully stable.

## Building with Nix

You can build the borrower with a Nix flake. This provides deterministic builds and ensures that anyone can verify
that our published binaries match the source code.

### Prerequisites
- A Linux x86_64 environment (see "Reproducibility across platforms" below)
- The Nix package manager

Some distributions provide the nix package manager in their own distribution packages. For example, on Debian you can use `apt install nix-bin`. This method is the most secure way of installing it. If you don't mind the security implications, the following command provides a simple method of installation on distributions that don't provide packaged binaries:

```bash
curl -L https://nixos.org/nix/install | sh
```

### Building

```console
nix build .#cli --extra-experimental-features 'nix-command flakes'
nix build .#borrower-wasm --extra-experimental-features 'nix-command flakes'
```

The binaries will be in `./result`. Nix aggressively hashes and caches everything, so repeat builds from the same commit
will be nearly instant. If you find your `/nix` folder is too large, run `nix-collect-garbage` to remove unused cache.

### Verifying Published Binaries

To verify that the published WASM binary matches the source code:

```console
nix run .#verify-wasm --extra-experimental-features 'nix-command flakes'
```

This will build the WASM module locally and compare its SHA-256 hash with the one published on the website.

If the hashes do not match and you are **not** building on Linux x86_64, this is expected — see
"Reproducibility across platforms" below and use the Docker-based method instead.

The website URL is configured in `flake.nix`. If you want, you can compare with a locally downloaded binary:

``` console
nix run .#verify-wasm -- path-to-local-borrower.wasm
```

### Development with Nix

You can use the Nix development shell which includes all necessary tools:

```bash
# Enter development shell
nix develop

# Or use direnv for automatic environment loading
echo "use flake" > .envrc
direnv allow
```

The development shell includes:

- Rust toolchain (from rust-toolchain.toml)
- cargo-watch for auto-recompilation
- cargo-expand for macro debugging
- wasm-pack and wasm-bindgen-cli
- All required system dependencies

### Misc

- All Nix builds are release builds by default
- The Rust version is pinned in `rust-toolchain.toml` for reproducibility
- If you're using VS Code or a good editor like Emacs, install the direnv extension for seamless integration

## Docker-based Build and Verification

For convenience we provide a Docker-based build that runs Nix inside a container.
This is the recommended way to verify the published binary on Windows and macOS,
or on any host that is not Linux x86_64.

### Reproducibility across platforms

The published binary is built on Linux x86_64 (see `.github/workflows/nix.yml`).
Nix builds are bit-reproducible *on that platform*: anyone building on Linux x86_64
from the same commit gets the exact same bytes. Builds on other platforms (macOS,
ARM, ...) run a toolchain built for the host and can produce a byte-different WASM
from the same source, so a "FAILURE: Hashes do not match!" from a native macOS build
means the build environment differs — not that the binary was tampered with.

The Docker script below therefore forces `--platform linux/amd64`, so the build uses
the same toolchain as CI and produces the same binary on every host (ARM hosts
emulate amd64).

### Building and verifying

Requirements: Docker (or Podman) with permission to run containers.
On Apple Silicon Macs, enabling "Use Rosetta for x86_64/amd64 emulation" in Docker
Desktop significantly speeds up the build.

```bash
# Note: before checking out an older commit, read "Verifying historical commits" below.
./docker-build-and-verify.sh
```

This builds the WASM module inside Docker (artifacts are extracted to
`docker-wasm-output/`) and then compares its SHA-256 hash with the one published on
the website. To compare against a locally downloaded binary instead:

```bash
./docker-build-and-verify.sh path-to-local-borrower.wasm
```

### Verifying historical commits

Verifiable builds require checking out the exact commit the published binary was
built from. Both `nix run .#verify-wasm` and `./docker-build-and-verify.sh` print
that commit if your checkout differs from it.

The Docker build files may not exist at that older commit. You can copy them over:
they are untracked files and do not affect the build.

```bash
cp docker-build-and-verify.sh Dockerfile.nix .dockerignore /tmp/ && \
git checkout COMMIT_FROM_THE_PREVIOUS_STEP && \
cp /tmp/docker-build-and-verify.sh /tmp/Dockerfile.nix /tmp/.dockerignore . && \
./docker-build-and-verify.sh
```

Do not modify any *tracked* files in this state: the build embeds the git commit,
so a modified tree produces a different binary (the script warns about this).

## Contributing

We welcome contributions from the community! Please read our [CONTRIBUTING](/CONTRIBUTING.md) file for guidelines on how to submit bug reports, feature requests, and pull requests.

## Security Policy

The Firefish team takes the security of our software and platform very seriously. If you discover a security vulnerability, please follow the guidelines in our [SECURITY](/SECURITY.md) file to report it responsibly.

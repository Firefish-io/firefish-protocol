# Firefish core library

This library implements the main logic of the Firefish smart contract: creating offers, accepting offers, (pre)signing transactions and other processing logic.
It also contains serialization code, an example CLI application and WASM bindings for the borrower.

**Warning: this is old code and will be replaced!**
This code is only provided for review and auditing, not for development.
We have some changes prepared that will improve the development use case (while breaking the API of this one).
If you're interested in development in any way don't hesitate to contact us.

The code here works and is deployed. The change we're preparing is not yet deployed, so it's not here to avoid confusion.

## Requirements

The library was tested with Rust 1.63 (installable on Debian 12 using `apt`), it may be usable with lower versions but definitely not below 1.56.1 (MSRV of `rust-bitcoin`)
Because of internal use of secp256k1 a working C compiler is also required. Stock gcc on Debian 12 works fine.

## Testing

There's simple test script provided for creating the escrow transaction.
The script expects that you built the cli using `cargo build` (change the path in it if you're doing release build).
The script launches all the CLI commands and passes the information around it then stops waiting for a raw transaction.

It is possible to make it run automatically, invoking `bitcoin-cli -chain=regtest` or do it manually.

To test it you need to spin up a regtest node, generate some sats and send them to the funding address provided in the output.
Enter the raw hex transaction (e.g. obtained from Core via `getrawtransaction`) and terminate the input (double CTRL+D or return and then CTRL+D).
You will get a transaction that you can broadcast using `bitcoin-cli -chain=regtest sendtransaction PUT_HEX_HERE 0`.
The script uses a fixed insane fee rate of 1000 sat/vb because regtest requires higher fees by default...

*Note that the script creates temporary files which it doesn't clean!*
This is intentional to allow inspection.
They aren't huge anyway but be careful about running it a lot.

Also you're free to change any of the hard-coded addresses in the script.
Interestingly they don't require private keys for the *contract* to work.

If you want to explore the raw CLI commands and try it out manually (maybe simulating real setup with more machines) reading the script should be good starting point.
More documentation will come eventually.

### Cancelation testing

To test cancelling the transaction from prefund run the script as you would normally.
However do **not** enter the funding transaction into it.
Instead kill the script using CTRL+C once you have the funding transaction.
Look at the first line in the output to find the borrower state file.
It will be `/tmp/tmp.SOME_RANDOM_STRING/borrower.state`
Run `./target/debug/examples/cli prefund cancel STATE_FILE_HERE FEE_RATE_SAT_VB`
I recommend using 1000 as the fee rate on regtest.
Enter the funding transaction inside this command.
It will give you the cancelation transaction.

Note that you will need to mine enough blocks after confirming the funding transaction to get cancelation to confirm.
If you used the script unchanged the relative lock time is 42 blocks.
So by running `bitcoin-cli generatetoaddress 42 SOME_ADDRESS` you will enable inclusion of cancelation transaction.
Use `sendrawtransaction` as usual.

### Finalization testing

To test repayment and default transactions run the script normally and let it finish.
Look at the first line in the output to find the ted-p state file.
It will be `/tmp/tmp.SOME_RANDOM_STRING/ted-p.state`
Run `./target/debug/examples/cli escrow repayment STATE_FILE_HERE`
Enter base64-encoded signatures from TED-O (in the output of the script, the longer string).
Broadcast the resulting transaction.
Same with default, just replace `repayment` with `default` in the command.

For liquidation you must run `escrow liquidation` subcommand first supplying the TED-O state file.
It's the same as above, just replace `ted-p.state` with `ted-o.state`.
Proceed running `escrow liquidation` as you would run `repayment` or `default` but enter the signature produced by `escrow liquidation` in the previous step instead.

## Development

The library API is unstable and definitely going to change.
The plan is to have a more stable version that encapsulates the instability into a simple API - just exchanging messages, configuration and state transitions.
This is already mostly done in a different branch but not yet properly tested and not fully stable.

## Building with Nix

You can build the borrower with a Nix flake. This provides deterministic builds and ensures that anyone can verify
that our published binaries match the source code.

### Prerequisites
- Linux environment
- The Nix package manager

Some distributions provide the nix package manager in their own distribution packages. For example, on Debian you can use `apt install nix-bin`. This method is the most secure way of installing it. If you don't mind the security implications the following command provides a simple method of installation on distributions that don't provide packaged binaries.

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
nix run .#verify-wasm
```

This will build the WASM module locally and compare its SHA-256 hash with the one published on the website.

The website URL is configured in `flake.nix`. If you want, you can compare with a locally downloaded binary:

``` console
nix run .#verify-wasm -- path-to-local-borrower.wasm
```

### Development with Nix

You can use the Nix development shell which includes all necessary tools:

```bash
# Enter development shell
nix develop

# Or use direnv for automatic environment loading into your environment
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

## Contributing

We welcome contributions from the community! Please read our [CONTRIBUTING](/CONTRIBUTING.md) file for guidelines on how to submit bug reports, feature requests, and pull requests.

## Security Policy

The Firefish team takes the security of our software and platform very seriously. If you discover a security vulnerability, please follow the guidelines in our [SECURITY](/SECURITY.md) file to report it responsibly.

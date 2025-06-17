# Read or update WASM metadata

This tool can read and update WASM metadata.
It is used in the build process to canonically add build revision to the WASM binary and during verification to find the git commit that produced a particular module.

## Usage

To read the commit ID run:

```
cargo run -- -r <you_wasm_file_here>
```

To update the commit ID run:

```
cargo run -- -u <you_wasm_file_here>
```

Which automatically retrieves the commit ID from git.
To override it, set the `GIT_COMMIT` environment variable to the desired value.

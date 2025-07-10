{
  description = "Firefish projects";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, crane, flake-utils, fenix, ... }:
    let
      # Configuration
      borrowerWasmUrl = "https://app.firefish.io/assets/borrower_wasm_bg.99aa2c22.wasm";
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };

        fenixPkgs = fenix.packages.${system};
        rustToolchain = fenixPkgs.fromToolchainFile {
          file = ./rust-toolchain.toml;
          sha256 = "sha256-Qxt8XAuaUR2OMdKbN4u8dBJOhSHxS+uS06Wl9+flVEk=";
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        commonArgs = {
          src = craneLib.cleanCargoSource (craneLib.path ./.);
          buildInputs = with pkgs; [
            secp256k1
          ];
          nativeBuildInputs = with pkgs; [
            pkg-config
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        wasm-commit = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "wasm-commit";
          cargoExtraArgs = "--package wasm-commit";
        });

        firefish-core = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "firefish-core";
          cargoExtraArgs = "--package firefish-core";
        });

        firefish-cli = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "firefish-cli";
          cargoExtraArgs = "--package firefish-cli";
        });

        wasm-bindgen-cli = craneLib.buildPackage {
          pname = "wasm-bindgen-cli";
          version = "0.2.100";

          src = pkgs.fetchCrate {
            pname = "wasm-bindgen-cli";
            version = "0.2.100";
            sha256 = "sha256-3RJzK7mkYFrs7C/WkhW9Rr4LdP5ofb2FdYGz1P7Uxog=";
          };
          nativeBuildInputs = [
            pkgs.openssl
            pkgs.pkg-config
          ];

          doCheck = false;
        };

        wasmArtifacts = craneLib.buildDepsOnly (commonArgs // {
          cargoExtraArgs = "--target wasm32-unknown-unknown";
        });

        # Build and process WASM modules
        buildPatchedWasm = { name, crateName, wasmName }:
          let
            gitCommit = self.rev or (builtins.substring 0 40 self.dirtyRev);

            wasm = craneLib.buildPackage (commonArgs // {
              inherit wasmArtifacts;
              pname = name;
              doCheck = false;
              cargoExtraArgs = "--package ${crateName} --target wasm32-unknown-unknown";
            });

            processed = pkgs.stdenv.mkDerivation {
              name = "${name}-processed";
              buildInputs = [ wasm-bindgen-cli ];
              dontUnpack = true;

              installPhase = ''
                mkdir -p $out/lib
                wasm-bindgen "${wasm}/lib/${wasmName}.wasm" \
                  --out-dir $out/lib \
                  --target web
              '';
            };
        in
          pkgs.stdenv.mkDerivation {
            name = "${name}-patched";
            src = ./.;
            env = {
              GIT_COMMIT = gitCommit;
            };
            buildPhase = ''
              cp ${processed}/lib/${wasmName}_bg.wasm ${wasmName}_bg.wasm
              chmod +w ${wasmName}_bg.wasm

              ${wasm-commit}/bin/wasm-commit --update \
                ${wasmName}_bg.wasm
            '';

            installPhase = ''
              mkdir -p $out/lib
              cp ${wasmName}_bg.wasm $out/lib/
              cp ${processed}/lib/${wasmName}.js $out/lib/
              cp ${processed}/lib/${wasmName}.d.ts $out/lib/
              cp ${processed}/lib/${wasmName}_bg.wasm.d.ts $out/lib/
            '';
          };


        borrower-wasm = buildPatchedWasm {
          name = "borrower-wasm";
          crateName = "firefish-borrower-wasm";
          wasmName = "firefish_borrower_wasm";
        };
      in
      {
        checks = {
          # TODO: Enable this if you want:
          #inherit firefish-core;
          #clippy = craneLib.cargoClippy (commonArgs // {
          #  inherit cargoArtifacts;
          #  cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          #});
          #fmt = craneLib.cargoFmt commonArgs;
        };

        packages = {
          core = firefish-core;
          cli = firefish-cli;
          borrower-wasm = borrower-wasm;
          default = firefish-core;
          wasm-commit = wasm-commit;
        };

        apps = {
          default = flake-utils.lib.mkApp {
            drv = firefish-cli;
            name = "firefish-cli";
          };

          verify-wasm = flake-utils.lib.mkApp {
            drv = pkgs.writeShellApplication {
              name = "verify-wasm";
              runtimeInputs = with pkgs; [ curl coreutils ];
              text = ''
                LOCAL_WASM="${borrower-wasm}/lib/firefish_borrower_wasm_bg.wasm"
                echo "Firefish WASM Binary Verification"
                echo "===================================="
                echo ""

                if [ $# -eq 0 ]; then
                  # Download from website
                  echo "Downloading WASM from website..."
                  TEMP_WASM=$(mktemp)
                  trap 'rm -f "$TEMP_WASM"' EXIT

                  if ! curl -sL "${borrowerWasmUrl}" -o "$TEMP_WASM"; then
                    echo "Error: Failed to download WASM from ${borrowerWasmUrl}"
                    exit 1
                  fi
                else
                  TEMP_WASM="$1"
                  echo "Using local WASM: $TEMP_WASM"
                fi

                # Verify we are on the correct GIT commit
                REMOTE_COMMIT=$(${wasm-commit}/bin/wasm-commit --read "$TEMP_WASM")
                LOCAL_COMMIT=$(git rev-parse HEAD 2>/dev/null || echo "unknown")

                if [ "$REMOTE_COMMIT" != "$LOCAL_COMMIT" ]; then
                  echo ""
                  echo "You are not on the same commit as the binary was built on"
                  echo "Remote: $REMOTE_COMMIT"
                  echo "Local:  $LOCAL_COMMIT"
                  echo "run 'git checkout $REMOTE_COMMIT' and try again"
                  exit 1
                fi

                # Calculate hashes
                echo ""
                echo "Calculating SHA-256 hashes..."
                LOCAL_HASH=$(sha256sum "$LOCAL_WASM" | cut -d' ' -f1)
                REMOTE_HASH=$(sha256sum "$TEMP_WASM" | cut -d' ' -f1)

                echo ""
                echo "Local build:  $LOCAL_HASH"
                echo "Website:      $REMOTE_HASH"
                echo ""

                # Compare
                if [ "$LOCAL_HASH" = "$REMOTE_HASH" ]; then
                  echo "SUCCESS: Hashes match! The website binary is verified."
                else
                  echo "FAILURE: Hashes do not match!"
                  echo ""
                  echo "This could mean:"
                  echo "  - The website binary was built from a different commit (verify which commit is deployed and which you are building from!)"
                  echo "  - The build is not reproducible"
                  echo "  - The binary has been tampered with"
                  exit 1
                fi
              '';
            };
          };
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ firefish-core ];

          packages = [
            pkgs.llvmPackages_20.bintools-unwrapped
            pkgs.cargo-watch
            pkgs.cargo-expand
            pkgs.wasm-pack
            pkgs.nodePackages.npm
            rustToolchain
            wasm-bindgen-cli
          ];

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      }
    );
}

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
      borrowerWasmUrl = "https://app.firefish.io/borrower_wasm.wasm";
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };

        fenixPkgs = fenix.packages.${system};
        rustToolchain = fenixPkgs.fromToolchainFile {
          file = ./rust-toolchain.toml;
          sha256 = "sha256-AJ6LX/Q/Er9kS15bn9iflkUwcgYqRQxiOIL2ToVAXaU=";
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Use Clang instead of default GCC as a compiler, otherwise the
        # secp2561k is not linked properly into the final wasm file.
        stdenv = pkgs: pkgs.clangStdenv;

        commonArgs = {
          inherit stdenv;
          strictDeps = true;

          src = craneLib.cleanCargoSource (craneLib.path ./.);
          nativeBuildInputs = with pkgs; [
            pkg-config

            # Do not use Nix-specific clang/linker wrappers with hardening
            # options.
            # - https://nixos.org/manual/nixpkgs/stable/#sec-hardening-in-nixpkgs
            pkgs.llvmPackages_21.clang-unwrapped
            pkgs.llvmPackages_21.bintools-unwrapped
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
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
        });

        # Build and process WASM modules
        buildPatchedWasm = { name, crateName, wasmName }:
          let
            gitCommit = self.rev or (builtins.substring 0 40 self.dirtyRev);

            wasm = craneLib.buildPackage (commonArgs // {
              inherit wasmArtifacts;
              pname = name;
              doCheck = false;
              cargoExtraArgs = "--package ${crateName}";
              CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
            });

            processed = pkgs.stdenv.mkDerivation {
              name = "${name}-processed";
              buildInputs = [ wasm-bindgen-cli ];
              dontUnpack = true;

              installPhase = ''
                mkdir -p $out/lib
                wasm-bindgen "${wasm}/lib/${wasmName}.wasm" \
                  --out-dir $out/lib \
                  --target bundler
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
              cp ${processed}/lib/${wasmName}*.js* $out/lib/
              cp ${processed}/lib/${wasmName}*.ts* $out/lib/
              cp ${wasmName}_bg.wasm $out/lib/
              VERSION=$(grep "^version" ${./.}/${name}/Cargo.toml | cut -d'"' -f2 || echo "0.1.0")

              cd $out/lib
              cat > package.json << EOF
              {
                "name": "${crateName}",
                "version": "$VERSION",
                "module": "${wasmName}.js",
                "types": "${wasmName}.d.ts",
                "sideEffects": false,
                "files": [$(ls *.wasm *.js *.d.ts 2>/dev/null | sed 's/^/"/;s/$/",/' | tr '\n' ' ' | sed 's/, $//')]
              }
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
                  REMOTE_LABEL="Website:      "
                else
                  TEMP_WASM="$1"
                  echo "Using local WASM: $TEMP_WASM"
                  REMOTE_LABEL="Local WASM:   "
                fi

                # Verify we are on the correct GIT commit
                REMOTE_COMMIT=$(${wasm-commit}/bin/wasm-commit --read "$TEMP_WASM")
                LOCAL_COMMIT=$(git rev-parse HEAD 2>/dev/null || echo "unknown")

                if [ "$REMOTE_COMMIT" != "$LOCAL_COMMIT" ]; then
                  echo ""
                  echo "You are not on the same commit as the binary was built on:"
                  echo ""
                  echo "Local build:     $LOCAL_COMMIT"
                  echo "$REMOTE_LABEL   $REMOTE_COMMIT"
                  echo ""
                  echo "run 'git checkout $REMOTE_COMMIT' and try again"
                  exit 1
                fi

                # Calculate hashes
                echo ""
                echo "Calculating SHA-256 hashes..."
                LOCAL_HASH=$(sha256sum "$LOCAL_WASM" | cut -d' ' -f1)
                REMOTE_HASH=$(sha256sum "$TEMP_WASM" | cut -d' ' -f1)

                echo ""
                echo "Local build:     $LOCAL_HASH"
                echo "$REMOTE_LABEL   $REMOTE_HASH"
                echo ""

                # Compare
                if [ "$LOCAL_HASH" = "$REMOTE_HASH" ]; then
                  echo "SUCCESS: Hashes match! The website binary is verified."
                else
                  echo "FAILURE: Hashes do not match!"
                  echo ""
                  echo "This could mean:"
                  echo "  - The build is not reproducible"
                  echo "  - The binary has been tampered with"
                  exit 1
                fi
              '';
            };
          };
        };

        devShells.default = pkgs.mkShell.override {
          stdenv = pkgs.clangStdenv;
        } rec {
          inputsFrom = [ firefish-core ];

          packages = [
            pkgs.llvmPackages_21.bintools-unwrapped
            pkgs.llvmPackages_21.clang-unwrapped
            pkgs.cargo-watch
            pkgs.cargo-expand
            pkgs.nodePackages.npm
            rustToolchain
            wasm-bindgen-cli
          ];

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      }
    );
}

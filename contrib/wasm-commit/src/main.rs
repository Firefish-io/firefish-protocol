fn main() {
    let mut args = std::env::args_os();
    args.next().unwrap_or_else(|| {
        eprintln!("Missing arguments");
        std::process::exit(1);
    });
    let first_arg = args.next().unwrap_or_else(|| {
        eprintln!("Missing operation (--read|-r|--update|-u)");
        std::process::exit(1);
    });

    if first_arg == "--read" || first_arg == "-r" {
        let path = args.next().unwrap_or_else(|| {
            eprintln!("Missing file path");
            std::process::exit(1);
        });

        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            eprintln!("Failed to read file {:?}: {}", path, error);
            std::process::exit(1);
        });
        let payload = wasm_metadata::Payload::from_binary(&bytes).unwrap_or_else(|error| {
            eprintln!("Failed to parse wasm module: {}", error);
            std::process::exit(1);
        });
        let revision = payload.metadata().revision.as_ref().unwrap_or_else(|| {
            eprintln!("Revision not found in the WASM module. This might be an old version that didn't have it or an unrelated module.");
            std::process::exit(1);
        });
        println!("{}", revision);
    } else if first_arg == "--update" || first_arg == "-u" {
        let path = args.next().unwrap_or_else(|| {
            eprintln!("Missing file path");
            std::process::exit(1);
        });

        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            eprintln!("Failed to read file {:?}: {}", path, error);
            std::process::exit(1);
        });

        let revision = std::env::var("GIT_COMMIT").unwrap_or_else(|error| {
            if let std::env::VarError::NotUnicode(invalid) = error {
                eprintln!("The GIT_COMMIT env var '{:?}' is not UTF-8", invalid);
                std::process::exit(1);
            }
            let output = std::process::Command::new("git")
                .arg("rev-parse")
                .arg("HEAD")
                .output()
                .unwrap_or_else(|error| {
                    eprintln!("Failed to execute `git rev-parse HEAD`: {}", error);
                    std::process::exit(1);
                });
            if !output.status.success() {
                eprintln!("`git rev-parse HEAD` failed with exit status {}", output.status);
                std::process::exit(1);
            }

            let mut output = String::from_utf8(output.stdout).unwrap_or_else(|error| {
                eprintln!("The output is not UTF-8 {}", error);
                std::process::exit(1);
            });

            if output.ends_with('\n') {
                output.pop();
            }

            output
        });

        let mut add = wasm_metadata::AddMetadata::default();
        add.revision = wasm_metadata::AddMetadataField::Set(wasm_metadata::Revision::new(revision));
        let new = add.to_wasm(&bytes).unwrap_or_else(|error| {
            eprintln!("Failed to update the WASM module: {}", error);
            std::process::exit(1);
        });
        std::fs::write(&path, &new).unwrap_or_else(|error| {
            eprintln!("Failed to write to the file {:?}: {}", path, error);
            std::process::exit(1);
        });
    } else {
        eprintln!("Unknown command {:?}. The valid commands are --read, -r, --update, and -u.", first_arg);
    }
}

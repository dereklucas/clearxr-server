use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

struct BuildTarget {
    label: &'static str,
    manifest_path: &'static str,
}

const BUILD_ORDER: &[BuildTarget] = &[
    BuildTarget {
        label: "clear-xr",
        manifest_path: "clearxr-space/Cargo.toml",
    },
    BuildTarget {
        label: "clear-xr-layer",
        manifest_path: "clearxr-layer/Cargo.toml",
    },
    BuildTarget {
        label: "clearxr-streamer",
        manifest_path: "clearxr-streamer/Cargo.toml",
    },
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        return Err(usage());
    };

    match command.as_str() {
        "build" => {
            let release = parse_build_args(args)?;
            build_all(release)
        }
        "help" | "--help" | "-h" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("Unknown xtask command '{other}'.\n\n{}", usage())),
    }
}

fn parse_build_args(args: impl Iterator<Item = String>) -> Result<bool, String> {
    let mut release = false;

    for arg in args {
        match arg.as_str() {
            "--release" => release = true,
            "--help" | "-h" => return Err(usage()),
            other => {
                return Err(format!(
                    "Unknown argument '{other}' for `xtask build`.\n\n{}",
                    usage()
                ))
            }
        }
    }

    Ok(release)
}

fn build_all(release: bool) -> Result<(), String> {
    let repo_root = repo_root()?;
    let profile_label = if release { "release" } else { "debug" };

    println!("Building Clear XR components in {profile_label} order:");
    for target in BUILD_ORDER {
        println!("  - {}", target.label);
    }

    for target in BUILD_ORDER {
        run_cargo_build(&repo_root, target, release)?;
    }

    println!("Build complete.");
    Ok(())
}

fn run_cargo_build(
    repo_root: &std::path::Path,
    target: &BuildTarget,
    release: bool,
) -> Result<(), String> {
    let cargo = cargo_command();
    let mut command = Command::new(&cargo);
    command
        .arg("build")
        .arg("--manifest-path")
        .arg(target.manifest_path)
        .arg("--locked")
        .current_dir(repo_root);

    if release {
        command.arg("--release");
    }

    println!(
        "\n==> cargo build --manifest-path {} --locked{}",
        target.manifest_path,
        if release { " --release" } else { "" }
    );
    let status = command
        .status()
        .map_err(|error| format!("failed to run {:?}: {error}", cargo))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "cargo build failed for '{}' with status {status}",
            target.label
        ))
    }
}

fn cargo_command() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

fn repo_root() -> Result<PathBuf, String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(|path| path.to_path_buf())
        .ok_or_else(|| "xtask could not determine the repository root".to_string())
}

fn usage() -> String {
    [
        "Usage:",
        "  cargo xtask build [--release]",
        "",
        "Commands:",
        "  build       Build clear-xr, clear-xr-layer, then clearxr-streamer.",
    ]
    .join("\n")
}

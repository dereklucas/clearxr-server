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

    copy_layer_to_streamer(&repo_root, profile_label)?;

    println!("Build complete.");
    Ok(())
}

/// Copy the layer DLL + manifest into the streamer's output directory so the
/// streamer's registration always points at a fresh build.
fn copy_layer_to_streamer(repo_root: &std::path::Path, profile: &str) -> Result<(), String> {
    let layer_out = repo_root.join("clearxr-layer").join("target").join(profile);
    let streamer_out = repo_root.join("clearxr-streamer").join("target").join(profile);

    let files = [
        ("clear_xr_layer.dll", "clear_xr_layer.dll"),
        ("clear-xr-layer.json", "clear-xr-layer.json"),
    ];

    for (src_name, dst_name) in &files {
        let src = layer_out.join(src_name);
        let dst = streamer_out.join(dst_name);

        if !src.exists() {
            return Err(format!(
                "Layer artifact missing: {}. Did clearxr-layer build succeed?",
                src.display()
            ));
        }

        std::fs::copy(&src, &dst).map_err(|e| {
            format!("Failed to copy {} → {}: {e}", src.display(), dst.display())
        })?;

        println!("  Copied {} → {}", src_name, dst.display());
    }

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

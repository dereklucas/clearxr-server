use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() {
    tauri_build::build();

    let profile = env::var("PROFILE").expect("missing PROFILE");
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"));
    let vendor_root = manifest_dir.join("..").join("vendor");
    let clearxr_space_root = manifest_dir.join("..").join("clearxr-space");
    let clearxr_layer_root = manifest_dir.join("..").join("clearxr-layer");
    println!("cargo:rerun-if-changed={}", vendor_root.display());
    println!(
        "cargo:rerun-if-changed={}",
        clearxr_layer_root.join("clear-xr-layer.json").display()
    );
    for candidate in clearxr_exe_candidates(&clearxr_space_root, &profile) {
        println!("cargo:rerun-if-changed={}", candidate.display());
    }
    for candidate in clearxr_layer_dll_candidates(&clearxr_layer_root, &profile) {
        println!("cargo:rerun-if-changed={}", candidate.display());
    }

    if let Err(error) = stage_vendor_runtime(
        &vendor_root,
        &clearxr_space_root,
        &clearxr_layer_root,
        &profile,
    ) {
        panic!(
            "failed to stage runtime dependencies from {}, {}, and {}: {error}",
            vendor_root.display(),
            clearxr_space_root.display(),
            clearxr_layer_root.display()
        );
    }
}

fn stage_vendor_runtime(
    vendor_root: &Path,
    clearxr_space_root: &Path,
    clearxr_layer_root: &Path,
    profile: &str,
) -> io::Result<()> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("missing OUT_DIR"));
    let target_dir = out_dir
        .ancestors()
        .find(|path| path.file_name().and_then(|name| name.to_str()) == Some(profile))
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not locate target profile directory",
            )
        })?;

    copy_vendor_item(&vendor_root.join("Server"), &target_dir.join("Server"))?;
    copy_vendor_item(
        &vendor_root.join("NvStreamManagerClient.dll"),
        &target_dir.join("NvStreamManagerClient.dll"),
    )?;
    copy_vendor_item(
        &vendor_root.join("NvStreamManagerClient.h"),
        &target_dir.join("NvStreamManagerClient.h"),
    )?;
    copy_vendor_item(
        &vendor_root.join("openxr_loader.dll"),
        &target_dir.join("openxr_loader.dll"),
    )?;

    if let Some(clearxr_exe) = find_clearxr_exe(clearxr_space_root, &profile) {
        copy_vendor_item(&clearxr_exe, &target_dir.join("clear-xr.exe"))?;
    } else {
        println!(
            "cargo:warning=clear-xr.exe was not found under {}. Build clearxr-space first if you want clearxr-streamer to stage it automatically.",
            clearxr_space_root.join("target").display()
        );
    }

    copy_vendor_item(
        &clearxr_layer_root.join("clear-xr-layer.json"),
        &target_dir.join("clear-xr-layer.json"),
    )?;
    if let Some(layer_dll) = find_clearxr_layer_dll(clearxr_layer_root, profile) {
        copy_vendor_item(&layer_dll, &target_dir.join("clear_xr_layer.dll"))?;
    } else {
        println!(
            "cargo:warning=clear_xr_layer.dll was not found under {}. Build clearxr-layer first if you want clearxr-streamer to stage it automatically.",
            clearxr_layer_root.join("target").display()
        );
    }

    Ok(())
}

fn find_clearxr_exe(clearxr_space_root: &Path, profile: &str) -> Option<PathBuf> {
    clearxr_exe_candidates(clearxr_space_root, profile)
        .into_iter()
        .find(|path| path.exists())
}

fn clearxr_exe_candidates(clearxr_space_root: &Path, profile: &str) -> Vec<PathBuf> {
    let target_root = clearxr_space_root.join("target");
    let mut candidates = vec![target_root.join(profile).join("clear-xr.exe")];

    if profile != "release" {
        candidates.push(target_root.join("release").join("clear-xr.exe"));
    }
    if profile != "debug" {
        candidates.push(target_root.join("debug").join("clear-xr.exe"));
    }

    candidates
}

fn find_clearxr_layer_dll(clearxr_layer_root: &Path, profile: &str) -> Option<PathBuf> {
    clearxr_layer_dll_candidates(clearxr_layer_root, profile)
        .into_iter()
        .find(|path| path.exists())
}

fn clearxr_layer_dll_candidates(clearxr_layer_root: &Path, profile: &str) -> Vec<PathBuf> {
    let target_root = clearxr_layer_root.join("target");
    let mut candidates = vec![target_root.join(profile).join("clear_xr_layer.dll")];

    if profile != "release" {
        candidates.push(target_root.join("release").join("clear_xr_layer.dll"));
    }
    if profile != "debug" {
        candidates.push(target_root.join("debug").join("clear_xr_layer.dll"));
    }

    candidates
}

fn copy_vendor_item(source: &Path, destination: &Path) -> io::Result<()> {
    if source.is_dir() {
        copy_dir_recursive(source, destination)
    } else {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        Ok(())
    }
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source_path, &destination_path)?;
        }
    }

    Ok(())
}

/// Build script:
///   1. Compiles GLSL shaders to SPIR-V via glslc (Vulkan SDK)
///   2. Copies openxr_loader.dll next to the output binary if not already present

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    compile_shader("shaders/scene.vert");
    compile_shader("shaders/scene.frag");
    copy_openxr_loader();
}

// ============================================================
// Shader compilation
// ============================================================

fn compile_shader(src: &str) {
    let dst = format!("{}.spv", src);
    println!("cargo:rerun-if-changed={}", src);

    let mut candidates = vec!["glslc".to_string()];

    if let Ok(sdk) = std::env::var("VULKAN_SDK") {
        candidates.push(format!(r"{}\Bin\glslc.exe", sdk));
    }

    if let Ok(entries) = std::fs::read_dir(r"C:\VulkanSDK") {
        let mut versions: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        versions.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        for entry in versions {
            candidates.push(format!(r"{}\Bin\glslc.exe", entry.path().display()));
        }
    }

    for glslc in &candidates {
        match Command::new(glslc)
            .args([src, "-o", &dst, "--target-env=vulkan1.1"])
            .status()
        {
            Ok(s) if s.success() => return,
            Ok(_) => panic!("glslc failed for '{}'. Check shader syntax.", src),
            Err(_) => continue,
        }
    }

    panic!(
        "Could not find glslc. Install the Vulkan SDK from https://vulkan.lunarg.com/ \
         and ensure its Bin directory is on your PATH, or set the VULKAN_SDK environment variable."
    );
}

// ============================================================
// OpenXR loader DLL auto-copy
// ============================================================

fn copy_openxr_loader() {
    println!("cargo:rerun-if-env-changed=OPENXR_LOADER_PATH");
    println!("cargo:rerun-if-changed=../vendor/openxr_loader.dll");

    // Figure out target directory (where the exe lands).
    // OUT_DIR is typically: target/<profile>/build/<crate>-<hash>/out
    // We walk ancestors to find the profile dir (debug / release).
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let target_dir = Path::new(&out_dir)
        .ancestors()
        .find(|p| {
            p.file_name()
                .map_or(false, |n| n == "debug" || n == "release")
        })
        .map(|p| p.to_path_buf())
        .expect("Could not determine target directory from OUT_DIR");

    let dest = target_dir.join("openxr_loader.dll");
    if dest.exists() {
        return; // Already present, nothing to do.
    }

    // Search for the DLL in local candidate locations.
    let candidates = find_openxr_loader_candidates();

    for src in &candidates {
        if src.exists() {
            match std::fs::copy(src, &dest) {
                Ok(_) => {
                    println!(
                        "cargo:warning=Copied openxr_loader.dll from {} → {}",
                        src.display(),
                        dest.display()
                    );
                    return;
                }
                Err(e) => {
                    println!(
                        "cargo:warning=Failed to copy openxr_loader.dll from {}: {}",
                        src.display(),
                        e
                    );
                }
            }
        }
    }

    println!(
        "cargo:warning=openxr_loader.dll not found. \
         The app will try to locate it at runtime, but may fail. \
         Set OPENXR_LOADER_PATH=<path to openxr_loader.dll> or copy it to {}",
        target_dir.display()
    );
}

/// Build a list of local candidate paths for openxr_loader.dll.
fn find_openxr_loader_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(path) = std::env::var("OPENXR_LOADER_PATH") {
        candidates.push(PathBuf::from(path));
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    if let Some(repo_root) = manifest_dir.parent() {
        candidates.push(repo_root.join("vendor").join("openxr_loader.dll"));
    }

    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join("openxr_loader.dll"));
    }

    candidates
}

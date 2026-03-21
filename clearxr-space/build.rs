/// Build script:
///   1. Compiles GLSL shaders to SPIR-V via glslc (Vulkan SDK)
///   2. On Windows, copies openxr_loader.dll next to the output binary if not already present

use std::process::Command;

fn main() {
    compile_shader("shaders/scene.vert");
    compile_shader("shaders/scene.frag");
    compile_shader("shaders/panel.vert");
    compile_shader("shaders/panel.frag");

    #[cfg(target_os = "windows")]
    copy_openxr_loader();
}

// ============================================================
// Shader compilation
// ============================================================

fn compile_shader(src: &str) {
    let dst = format!("{}.spv", src);
    println!("cargo:rerun-if-changed={}", src);

    // If the .spv is already newer than the source, skip compilation.
    // This allows building on machines without glslc if shaders haven't changed.
    if let (Ok(src_meta), Ok(dst_meta)) =
        (std::fs::metadata(src), std::fs::metadata(&dst))
    {
        if let (Ok(src_time), Ok(dst_time)) =
            (src_meta.modified(), dst_meta.modified())
        {
            if dst_time >= src_time {
                return;
            }
        }
    }

    let mut candidates = vec!["glslc".to_string()];

    // Windows: check VULKAN_SDK env and C:\VulkanSDK
    if let Ok(sdk) = std::env::var("VULKAN_SDK") {
        #[cfg(target_os = "windows")]
        candidates.push(format!(r"{}\Bin\glslc.exe", sdk));
        #[cfg(not(target_os = "windows"))]
        candidates.push(format!("{}/bin/glslc", sdk));
    }

    #[cfg(target_os = "windows")]
    if let Ok(entries) = std::fs::read_dir(r"C:\VulkanSDK") {
        let mut versions: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        versions.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        for entry in versions {
            candidates.push(format!(r"{}\Bin\glslc.exe", entry.path().display()));
        }
    }

    // macOS: check common Homebrew / LunarG SDK paths
    #[cfg(target_os = "macos")]
    {
        candidates.push("/usr/local/bin/glslc".to_string());
        candidates.push("/opt/homebrew/bin/glslc".to_string());
        // LunarG macOS SDK default install
        if let Ok(entries) = std::fs::read_dir("/usr/local/share/vulkan/sdks") {
            for entry in entries.flatten() {
                candidates.push(format!("{}/macOS/bin/glslc", entry.path().display()));
            }
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
// OpenXR loader DLL auto-copy (Windows only)
// ============================================================

#[cfg(target_os = "windows")]
fn copy_openxr_loader() {
    println!("cargo:rerun-if-env-changed=OPENXR_LOADER_PATH");
    println!("cargo:rerun-if-changed=../vendor/openxr_loader.dll");

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
        return;
    }

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

#[cfg(target_os = "windows")]
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

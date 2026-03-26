mod app;
mod audio;
mod capture;
mod config;
mod input;
mod launcher_panel;
mod panel;
mod shell;
mod ui;
#[cfg(all(feature = "xr", target_os = "windows"))]
mod mirror_window;
mod vk_backend;
#[cfg(all(feature = "xr", target_os = "windows"))]
mod xr_session;
mod renderer;
use anyhow::Result;
use log::info;

/// Log system diagnostics before any XR/Vulkan initialization.
fn log_system_info() {
    info!("ClearXR Shell starting");
    info!(
        "Platform: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    if let Ok(exe) = std::env::current_exe() {
        info!("Executable: {}", exe.display());
    }
    if let Ok(dir) = std::env::current_dir() {
        info!("Working directory: {}", dir.display());
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();

    log_system_info();

    info!("==============================================");
    info!("  Clear XR  –  CloudXR visual splash / test space");
    info!("  Press Ctrl+C to exit.");
    info!("==============================================");

    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let r = running.clone();
        ctrlc::set_handler(move || {
            info!("Ctrl+C received – requesting exit.");
            r.store(false, std::sync::atomic::Ordering::SeqCst);
        })
        .ok();
    }

    let args: Vec<String> = std::env::args().collect();
    let screen_capture = args.iter().any(|a| a == "--screen");

    #[cfg(all(feature = "xr", target_os = "windows"))]
    {
        xr_session::run(running, screen_capture)?;
        info!("Clear XR exited cleanly.");
        return Ok(());
    }

    #[cfg(not(all(feature = "xr", target_os = "windows")))]
    anyhow::bail!(
        "No runtime mode available. Build with --features xr on Windows."
    );
}

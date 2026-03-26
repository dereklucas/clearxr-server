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
#[cfg(feature = "desktop")]
mod desktop_session;

use anyhow::Result;
use log::info;

fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();

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
    let desktop_mode = args.iter().any(|a| a == "--desktop");
    let screen_capture = args.iter().any(|a| a == "--screen");

    if desktop_mode {
        #[cfg(feature = "desktop")]
        {
            if screen_capture {
                info!("Running in desktop mode with screen capture.");
            } else {
                info!("Running in desktop window mode.");
            }
            return desktop_session::run(running, screen_capture);
        }
        #[cfg(not(feature = "desktop"))]
        anyhow::bail!(
            "Desktop mode requested but the 'desktop' feature is not enabled.\n\
             Rebuild with: cargo build --features desktop"
        );
    }

    #[cfg(all(feature = "xr", target_os = "windows"))]
    {
        xr_session::run(running, screen_capture)?;
        info!("Clear XR exited cleanly.");
        return Ok(());
    }

    #[cfg(not(all(feature = "xr", target_os = "windows")))]
    anyhow::bail!(
        "No runtime mode available. Build with --features xr (Windows) or --features desktop."
    );
}

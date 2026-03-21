mod audio;
mod mirror_window;
mod vk_backend;
mod xr_session;
mod renderer;

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
 
    // // Start background audio loop.
    // let wav_path = Path::new("assets/clearxr.wav");
    // if wav_path.exists() {
    //     if let Err(e) = audio::start_looped(wav_path, running.clone()) {
    //         warn!("Audio playback failed to start: {}", e);
    //     }
    // } else {
    //     warn!("WAV file not found at {:?} – no audio.", wav_path);
    // } 

    xr_session::run(running)?;

    info!("Clear XR exited cleanly.");
    Ok(())
}

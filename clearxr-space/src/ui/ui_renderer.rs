/// Renders the launcher HTML to an RGBA pixel buffer using Ultralight.
///
/// Handles:
///   - Loading and rendering launcher.html
///   - Injecting game data as JSON into the JS context
///   - Mouse event forwarding (hover, click) from VR controller hits
///   - Hot-reloading when the HTML file changes on disk

use anyhow::Result;
use log::info;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use ul_next::{
    config::Config,
    event::{MouseButton, MouseEvent, MouseEventType},
    platform,
    renderer::Renderer as UlRenderer,
    view::ViewConfig,
    Library,
};

use crate::app::game_scanner::Game;

pub struct UiRenderer {
    lib: Arc<Library>,
    renderer: UlRenderer,
    view: ul_next::view::View,
    width: u32,
    height: u32,
    html_path: PathBuf,
    // Pixel buffer (RGBA, ready for Vulkan upload)
    pixels: Vec<u8>,
    dirty: bool,
    // File watcher
    _watcher: Option<notify::RecommendedWatcher>,
    file_changed_rx: mpsc::Receiver<()>,
}

impl UiRenderer {
    pub fn new(width: u32, height: u32, html_path: &Path) -> Result<Self> {
        let lib = Library::linked();

        // Platform setup — must happen before Renderer creation
        platform::enable_platform_fontloader(lib.clone());

        // Ultralight needs its resources/ dir (ICU data, certs) at the filesystem root.
        // The SDK is downloaded by ul-next-sys into the build output directory.
        let sdk_path = find_ultralight_sdk()?;
        info!("Ultralight SDK path: {}", sdk_path.display());

        platform::enable_platform_filesystem(lib.clone(), sdk_path.to_str().unwrap_or("."))
            .map_err(|e| anyhow::anyhow!("Ultralight filesystem setup failed: {:?}", e))?;

        let config = Config::start()
            .resource_path_prefix("resources/")
            .build(lib.clone())
            .ok_or_else(|| anyhow::anyhow!("Ultralight config creation failed"))?;

        let renderer = UlRenderer::create(config)
            .map_err(|e| anyhow::anyhow!("Ultralight renderer creation failed: {:?}", e))?;

        let view_config = ViewConfig::start()
            .is_accelerated(false) // CPU rendering → Surface with pixel data
            .is_transparent(true)  // transparent background for alpha blending
            .initial_device_scale(1.0)
            .build(lib.clone())
            .ok_or_else(|| anyhow::anyhow!("Ultralight view config creation failed"))?;

        let view = renderer
            .create_view(width, height, &view_config, None)
            .ok_or_else(|| anyhow::anyhow!("Ultralight view creation returned None"))?;

        // Console message callback
        view.set_add_console_message_callback(
            |_view, _source, _level, message, _line, _col, _source_id| {
                info!("[UI Console] {}", message);
            },
        );

        // Set up file watcher
        let (file_changed_tx, file_changed_rx) = mpsc::channel();
        let watcher = setup_file_watcher(html_path, file_changed_tx)?;

        let pixels = vec![0u8; (width * height * 4) as usize];

        let mut ui = Self {
            lib,
            renderer,
            view,
            width,
            height,
            html_path: html_path.to_path_buf(),
            pixels,
            dirty: true,
            _watcher: Some(watcher),
            file_changed_rx,
        };

        // Initial load
        ui.load_html()?;

        Ok(ui)
    }

    /// Load (or reload) the HTML file from disk.
    fn load_html(&mut self) -> Result<()> {
        let html = std::fs::read_to_string(&self.html_path)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", self.html_path.display(), e))?;

        self.view
            .load_html(&html)
            .map_err(|e| anyhow::anyhow!("Ultralight load_html failed: {:?}", e))?;

        // Pump until loaded
        let done = std::rc::Rc::new(AtomicBool::new(false));
        let done_c = done.clone();
        self.view
            .set_finish_loading_callback(move |_, _, is_main, _| {
                if is_main {
                    done_c.store(true, Ordering::SeqCst);
                }
            });

        let start = std::time::Instant::now();
        while !done.load(Ordering::SeqCst) {
            self.renderer.update();
            std::thread::sleep(std::time::Duration::from_millis(5));
            if start.elapsed().as_secs() > 5 {
                anyhow::bail!("Ultralight HTML load timed out");
            }
        }

        // Clear the callback
        self.view.set_finish_loading_callback(|_, _, _, _| {});

        self.dirty = true;
        info!("UI HTML loaded from {}", self.html_path.display());
        Ok(())
    }

    /// Inject game data into the page as `window.GAMES = [...]` and re-render.
    pub fn set_games(&mut self, games: &[Game]) -> Result<()> {
        let json = serde_json::to_string(games)?;
        let script = format!("window.GAMES = {}; renderGames();", json);
        match self.view.evaluate_script(&script) {
            Ok(Ok(_)) => {}
            Ok(Err(ex)) => log::warn!("JS exception setting games: {}", ex),
            Err(e) => log::warn!("JS eval error: {:?}", e),
        }
        self.dirty = true;
        Ok(())
    }

    /// Send a mouse move event (UV coordinates 0..1).
    pub fn mouse_move(&mut self, u: f32, v: f32) {
        let x = (u * self.width as f32) as i32;
        let y = (v * self.height as f32) as i32;
        if let Ok(evt) = MouseEvent::new(
            self.lib.clone(),
            MouseEventType::MouseMoved,
            x,
            y,
            MouseButton::None,
        ) {
            self.view.fire_mouse_event(evt);
            self.dirty = true;
        }
    }

    /// Evaluate a JavaScript expression and return the string result.
    ///
    /// Returns `None` if the script fails or returns an empty/undefined value.
    pub fn evaluate_js(&self, js: &str) -> Option<String> {
        match self.view.evaluate_script(js) {
            Ok(Ok(result)) => {
                let s = result.trim().to_string();
                if s.is_empty() || s == "undefined" || s == "null" {
                    None
                } else {
                    Some(s)
                }
            }
            Ok(Err(ex)) => {
                log::warn!("JS exception in evaluate_js: {}", ex);
                None
            }
            Err(e) => {
                log::warn!("JS eval error in evaluate_js: {:?}", e);
                None
            }
        }
    }

    /// Send a mouse click (down + up) at UV coordinates.
    pub fn mouse_click(&mut self, u: f32, v: f32) {
        let x = (u * self.width as f32) as i32;
        let y = (v * self.height as f32) as i32;
        if let Ok(down) = MouseEvent::new(
            self.lib.clone(),
            MouseEventType::MouseDown,
            x,
            y,
            MouseButton::Left,
        ) {
            self.view.fire_mouse_event(down);
        }
        if let Ok(up) = MouseEvent::new(
            self.lib.clone(),
            MouseEventType::MouseUp,
            x,
            y,
            MouseButton::Left,
        ) {
            self.view.fire_mouse_event(up);
        }
        self.dirty = true;
    }

    /// Check for file changes and re-render if needed.
    /// Returns the RGBA pixel buffer if anything changed, None otherwise.
    pub fn update(&mut self) -> Option<&[u8]> {
        // Check for hot-reload
        if self.file_changed_rx.try_recv().is_ok() {
            // Drain any additional change events
            while self.file_changed_rx.try_recv().is_ok() {}
            info!("UI file changed, reloading...");
            if let Err(e) = self.load_html() {
                log::error!("Hot-reload failed: {}", e);
            }
        }

        self.renderer.update();

        if !self.dirty && !self.view.needs_paint() {
            return None;
        }

        self.renderer.render();

        // Extract pixels from surface
        if let Some(mut surface) = self.view.surface() {
            let row_bytes = surface.row_bytes() as usize;
            let expected = (self.width * self.height * 4) as usize;

            {
                if let Some(pixels_guard) = surface.lock_pixels() {
                    let src = &*pixels_guard;
                    self.pixels.resize(expected, 0);

                    // Convert BGRA → RGBA, handling potential row padding
                    for y in 0..self.height as usize {
                        let src_row_start = y * row_bytes;
                        let dst_row_start = y * self.width as usize * 4;
                        for x in 0..self.width as usize {
                            let si = src_row_start + x * 4;
                            let di = dst_row_start + x * 4;
                            if si + 3 < src.len() {
                                self.pixels[di] = src[si + 2];     // R ← B
                                self.pixels[di + 1] = src[si + 1]; // G ← G
                                self.pixels[di + 2] = src[si];     // B ← R
                                self.pixels[di + 3] = src[si + 3]; // A ← A
                            }
                        }
                    }
                }
                // pixels_guard dropped here, safe to call clear_dirty_bounds
            }
            surface.clear_dirty_bounds();
        }

        self.dirty = false;
        Some(&self.pixels)
    }
}

// ---- Ultralight SDK locator ----

/// Find the Ultralight SDK directory (downloaded by ul-next-sys build script).
/// It lives under target/<profile>/build/ul-next-sys-<hash>/out/ul-sdk/
fn find_ultralight_sdk() -> Result<PathBuf> {
    // First check env var that ul-next-sys might set
    if let Ok(path) = std::env::var("UL_SDK_PATH") {
        let p = PathBuf::from(path);
        if p.join("resources").exists() {
            return Ok(p);
        }
    }

    // Walk up from the executable to find the build directory
    if let Ok(exe) = std::env::current_exe() {
        // exe is in target/debug/ or target/release/
        if let Some(target_dir) = exe.parent() {
            // Search target/<profile>/build/ul-next-sys-*/out/ul-sdk/
            let build_dir = target_dir.join("build");
            if let Ok(entries) = std::fs::read_dir(&build_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if name.to_string_lossy().starts_with("ul-next-sys-") {
                        let sdk = entry.path().join("out").join("ul-sdk");
                        if sdk.join("resources").exists() {
                            return Ok(sdk);
                        }
                    }
                }
            }
        }
    }

    anyhow::bail!(
        "Could not find Ultralight SDK resources directory. \
         Ensure ul-next-sys has been built (cargo build should download it)."
    )
}

// ---- File Watcher ----

fn setup_file_watcher(
    path: &Path,
    tx: mpsc::Sender<()>,
) -> Result<notify::RecommendedWatcher> {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    let watch_path = path.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |res: std::result::Result<Event, _>| {
        if let Ok(event) = res {
            match event.kind {
                EventKind::Modify(_) | EventKind::Create(_) => {
                    if event.paths.iter().any(|p| p == &watch_path) {
                        let _ = tx.send(());
                    }
                }
                _ => {}
            }
        }
    })?;

    // Watch the parent directory (notify can't always watch individual files reliably)
    let parent = path.parent().unwrap_or(Path::new("."));
    watcher.watch(parent, RecursiveMode::NonRecursive)?;

    info!("Watching {} for changes", path.display());
    Ok(watcher)
}

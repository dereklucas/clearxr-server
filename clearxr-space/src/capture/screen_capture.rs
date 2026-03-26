/// Screen / window capture + input injection.
///
/// macOS:   CoreGraphics CGDisplayCreateImage
/// Windows: DXGI Desktop Duplication (background thread)
/// Input injection (all platforms): enigo crate

use anyhow::Result;
use log::info;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A captured frame: raw BGRA pixels, tightly packed (4 bytes per pixel).
#[allow(dead_code)] // width/height are part of the public API for consumers
pub struct CaptureFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct ScreenCapture {
    receiver: std::sync::mpsc::Receiver<CaptureFrame>,
    keep_running: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Native display resolution (for coordinate mapping in input injection)
    screen_width: u32,
    screen_height: u32,
    /// Cache the latest frame for the main thread
    latest_frame: Option<CaptureFrame>,
}

impl ScreenCapture {
    pub fn new() -> Result<Self> {
        let (screen_width, screen_height) = get_screen_size();
        info!("ScreenCapture: native screen {}x{}", screen_width, screen_height);

        let keep_running = Arc::new(AtomicBool::new(true));
        let (sender, receiver) = std::sync::mpsc::sync_channel::<CaptureFrame>(1);

        let thread = {
            let keep_running = keep_running.clone();
            let sw = screen_width;
            let sh = screen_height;
            std::thread::Builder::new()
                .name("screen-capture".into())
                .spawn(move || {
                    capture_thread(keep_running, sender, sw, sh);
                })?
        };

        Ok(Self {
            receiver,
            keep_running,
            thread: Some(thread),
            screen_width,
            screen_height,
            latest_frame: None,
        })
    }

    /// Drain the channel and return a reference to the latest captured frame (if any).
    pub fn try_get_frame(&mut self) -> Option<&CaptureFrame> {
        while let Ok(frame) = self.receiver.try_recv() {
            self.latest_frame = Some(frame);
        }
        self.latest_frame.as_ref()
    }

    /// Native screen width in pixels.
    pub fn screen_width(&self) -> u32 { self.screen_width }

    /// Native screen height in pixels.
    pub fn screen_height(&self) -> u32 { self.screen_height }

    /// Move the real mouse cursor to the position corresponding to (u,v) on the panel.
    /// u,v are in [0,1] panel-space coordinates.
    pub fn inject_mouse_move(&self, u: f32, v: f32) {
        use enigo::{Enigo, Mouse, Settings, Coordinate};
        let x = (u * self.screen_width as f32) as i32;
        let y = (v * self.screen_height as f32) as i32;
        if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
            let _ = enigo.move_mouse(x, y, Coordinate::Abs);
        }
    }

    /// Click at the position corresponding to (u,v) on the panel.
    pub fn inject_mouse_click(&self, u: f32, v: f32) {
        use enigo::{Enigo, Mouse, Settings, Coordinate, Button, Direction};
        let x = (u * self.screen_width as f32) as i32;
        let y = (v * self.screen_height as f32) as i32;
        if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
            let _ = enigo.move_mouse(x, y, Coordinate::Abs);
            let _ = enigo.button(Button::Left, Direction::Click);
        }
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.keep_running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            handle.join().ok();
        }
    }
}

// ============================================================
// Background capture thread
// ============================================================

#[cfg(target_os = "windows")]
fn capture_thread(
    keep_running: Arc<AtomicBool>,
    sender: std::sync::mpsc::SyncSender<CaptureFrame>,
    _screen_width: u32,
    _screen_height: u32,
) {
    let mut dxgi = match DxgiCapture::new() {
        Ok(d) => {
            info!("DXGI Desktop Duplication initialized (capture thread).");
            d
        }
        Err(e) => {
            log::error!("DXGI init failed in capture thread: {}", e);
            return;
        }
    };

    let frame_interval = std::time::Duration::from_millis(33); // ~30fps
    let mut buffer = Vec::new(); // reusable buffer — stays allocated across frames

    while keep_running.load(Ordering::Relaxed) {
        let frame_start = std::time::Instant::now();

        match dxgi.capture_bgra_frame_into(&mut buffer) {
            Ok(Some((w, h))) => {
                // Clone the buffer for the channel (buffer stays allocated for next frame)
                let frame = CaptureFrame { data: buffer.clone(), width: w, height: h };
                // try_send: drop the frame if the channel is full (main thread hasn't consumed yet)
                let _ = sender.try_send(frame);
            }
            Ok(None) => {
                // No new frame available, sleep briefly and retry
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            Err(e) => {
                log::debug!("DXGI capture error: {}", e);
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
        }

        // Sleep to target ~30fps
        let elapsed = frame_start.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
    }

    info!("Capture thread exiting.");
}

#[cfg(target_os = "macos")]
fn capture_thread(
    keep_running: Arc<AtomicBool>,
    _sender: std::sync::mpsc::SyncSender<CaptureFrame>,
    _screen_width: u32,
    _screen_height: u32,
) {
    // TODO: macOS capture on background thread
    info!("macOS capture thread: not yet implemented, exiting.");
    while keep_running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn capture_thread(
    keep_running: Arc<AtomicBool>,
    _sender: std::sync::mpsc::SyncSender<CaptureFrame>,
    _screen_width: u32,
    _screen_height: u32,
) {
    while keep_running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

// ============================================================
// Platform: get screen size
// ============================================================

fn get_screen_size() -> (u32, u32) {
    #[cfg(target_os = "macos")]
    {
        type CGDirectDisplayID = u32;
        extern "C" {
            fn CGMainDisplayID() -> CGDirectDisplayID;
            fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;
            fn CGDisplayPixelsHigh(display: CGDirectDisplayID) -> usize;
        }
        unsafe {
            let d = CGMainDisplayID();
            (CGDisplayPixelsWide(d) as u32, CGDisplayPixelsHigh(d) as u32)
        }
    }
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN) as u32;
            let h = GetSystemMetrics(SM_CYSCREEN) as u32;
            (w, h)
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        (1920, 1080)
    }
}

// ============================================================
// Windows: DXGI Desktop Duplication (used inside capture thread)
// ============================================================

#[cfg(target_os = "windows")]
struct DxgiCapture {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    duplication: windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication,
    staging: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>,
    desc_width: u32,
    desc_height: u32,
    frame_acquired: bool,
    _frame_buffer: Vec<u8>,
}

#[cfg(target_os = "windows")]
impl DxgiCapture {
    fn new() -> Result<Self> {
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::*;
        use windows::Win32::Graphics::Dxgi::*;

        // Create D3D11 device
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None, // feature levels (default)
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.ok_or_else(|| anyhow::anyhow!("D3D11 device creation failed"))?;
        let context = context.ok_or_else(|| anyhow::anyhow!("D3D11 context creation failed"))?;

        // Get DXGI device -> adapter -> output
        use windows::core::Interface;
        let dxgi_device: IDXGIDevice = device.cast()?;
        let adapter = unsafe { dxgi_device.GetAdapter()? };
        let output = unsafe { adapter.EnumOutputs(0)? };
        let output1: IDXGIOutput1 = output.cast()?;

        // Duplicate the output
        let duplication = unsafe { output1.DuplicateOutput(&device)? };

        // Get output dimensions
        let out_desc = unsafe { output.GetDesc()? };
        let desc_width = (out_desc.DesktopCoordinates.right - out_desc.DesktopCoordinates.left) as u32;
        let desc_height = (out_desc.DesktopCoordinates.bottom - out_desc.DesktopCoordinates.top) as u32;

        info!("DXGI output: {}x{}", desc_width, desc_height);

        Ok(Self {
            device,
            context,
            duplication,
            staging: None,
            desc_width,
            desc_height,
            frame_acquired: false,
            _frame_buffer: Vec::new(),
        })
    }

    /// Acquire a DXGI frame, copy to staging, map, and write tightly-packed BGRA pixels
    /// into the caller-provided buffer. Returns Ok(Some((width, height))) on success,
    /// Ok(None) if no new frame is available.
    ///
    /// The buffer is resized as needed and reused across calls to avoid per-frame allocation.
    fn capture_bgra_frame_into(&mut self, buffer: &mut Vec<u8>) -> Result<Option<(u32, u32)>> {
        use windows::core::Interface;
        use windows::Win32::Graphics::Direct3D11::*;
        use windows::Win32::Graphics::Dxgi::Common::*;

        if self.frame_acquired {
            self.release_frame()?;
        }

        let mut frame_info = Default::default();
        let mut resource = None;

        // 0ms timeout = non-blocking
        let hr = unsafe {
            self.duplication
                .AcquireNextFrame(0, &mut frame_info, &mut resource)
        };

        match hr {
            Ok(()) => {}
            Err(e) if e.code().0 as u32 == 0x887A0027 => {
                // DXGI_ERROR_WAIT_TIMEOUT -- no new frame
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }

        self.frame_acquired = true;

        let resource = resource.ok_or_else(|| anyhow::anyhow!("No DXGI resource"))?;
        let texture: ID3D11Texture2D = resource.cast()?;

        // Get texture desc
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };

        // Create staging texture on first use (or if size changed)
        if self.staging.is_none()
            || self.desc_width != desc.Width
            || self.desc_height != desc.Height
        {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging_tex = None;
            unsafe { self.device.CreateTexture2D(&staging_desc, None, Some(&mut staging_tex))? };
            self.staging = staging_tex;
            self.desc_width = desc.Width;
            self.desc_height = desc.Height;
        }

        let staging = self.staging.as_ref().unwrap();

        // Copy desktop texture -> staging
        unsafe {
            self.context.CopyResource(staging, &texture);
        }

        // Map staging to CPU
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }

        let width = desc.Width;
        let height = desc.Height;
        let pitch = mapped.RowPitch as usize;
        let row_bytes = (width * 4) as usize;

        // Resize buffer once (stays allocated across frames)
        buffer.resize((width * height * 4) as usize, 0);

        // Copy rows: handle pitch (mapped data may have padding per row)
        unsafe {
            let src = mapped.pData as *const u8;
            for y in 0..height as usize {
                let src_row = src.add(y * pitch);
                let dst_off = y * row_bytes;
                std::ptr::copy_nonoverlapping(src_row, buffer.as_mut_ptr().add(dst_off), row_bytes);
            }
        }

        // Unmap + release
        unsafe {
            self.context.Unmap(staging, 0);
        }
        self.release_frame()?;

        // Draw mouse cursor into the buffer
        draw_cursor_into_buffer(buffer, width, height);

        Ok(Some((width, height)))
    }

    fn release_frame(&mut self) -> Result<()> {
        if self.frame_acquired {
            unsafe { self.duplication.ReleaseFrame()? };
            self.frame_acquired = false;
        }
        Ok(())
    }
}

/// Draw the system mouse cursor into a BGRA pixel buffer.
#[cfg(target_os = "windows")]
fn draw_cursor_into_buffer(buffer: &mut [u8], width: u32, height: u32) {
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    use windows::Win32::Foundation::POINT;

    let mut pt = POINT::default();
    let ok = unsafe { GetCursorPos(&mut pt) };
    if !ok.is_ok() {
        return;
    }

    let cx = pt.x;
    let cy = pt.y;

    // Draw a small white crosshair (5px radius) at cursor position
    let radius = 5i32;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            // Crosshair shape: horizontal or vertical line
            if dx.abs() > 1 && dy.abs() > 1 {
                continue;
            }
            let px = cx + dx;
            let py = cy + dy;
            if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                let idx = ((py as u32 * width + px as u32) * 4) as usize;
                if idx + 3 < buffer.len() {
                    // BGRA format: white with full alpha
                    buffer[idx] = 0xFF;     // B
                    buffer[idx + 1] = 0xFF; // G
                    buffer[idx + 2] = 0xFF; // R
                    buffer[idx + 3] = 0xFF; // A
                }
            }
        }
    }
    // Dark outline
    for dy in -(radius + 1)..=(radius + 1) {
        for dx in -(radius + 1)..=(radius + 1) {
            if dx.abs() > 2 && dy.abs() > 2 { continue; }
            if dx.abs() <= 1 && dy.abs() <= radius { continue; } // skip inner crosshair
            if dy.abs() <= 1 && dx.abs() <= radius { continue; }
            let px = cx + dx;
            let py = cy + dy;
            if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                let idx = ((py as u32 * width + px as u32) * 4) as usize;
                if idx + 3 < buffer.len() {
                    buffer[idx] = 0x00;
                    buffer[idx + 1] = 0x00;
                    buffer[idx + 2] = 0x00;
                    buffer[idx + 3] = 0xFF;
                }
            }
        }
    }
}

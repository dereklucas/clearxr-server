/// Screen / window capture + input injection.
///
/// macOS:   CoreGraphics CGDisplayCreateImage + CGEvent input injection
/// Windows: DXGI Desktop Duplication + SendInput

use anyhow::Result;
use log::info;

pub struct ScreenCapture {
    width: u32,
    height: u32,
    pixel_buffer: Vec<u8>, // RGBA
    /// Native display resolution (for coordinate mapping)
    screen_width: u32,
    screen_height: u32,
    frame_count: u64,
    #[cfg(target_os = "windows")]
    dxgi: Option<DxgiCapture>,
}

impl ScreenCapture {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        info!("ScreenCapture: {}x{}", width, height);

        let (screen_width, screen_height) = get_screen_size();
        info!("Native screen: {}x{}", screen_width, screen_height);

        #[cfg(target_os = "windows")]
        let dxgi = match DxgiCapture::new() {
            Ok(d) => {
                info!("DXGI Desktop Duplication initialized.");
                Some(d)
            }
            Err(e) => {
                log::warn!("DXGI init failed ({}), capture disabled.", e);
                None
            }
        };

        Ok(Self {
            width,
            height,
            pixel_buffer: vec![0u8; (width * height * 4) as usize],
            screen_width,
            screen_height,
            frame_count: 0,
            #[cfg(target_os = "windows")]
            dxgi,
        })
    }

    /// Capture the main display and return RGBA pixels scaled to our target size.
    /// Returns None if capture failed (non-fatal, just skip the frame).
    pub fn capture(&mut self) -> Option<&[u8]> {
        #[cfg(target_os = "macos")]
        return self.capture_macos();

        #[cfg(target_os = "windows")]
        return self.capture_windows();

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        return None;
    }

    /// Move the real mouse cursor to the position corresponding to (u,v) on the panel.
    /// u,v are in [0,1] panel-space coordinates.
    pub fn inject_mouse_move(&self, u: f32, v: f32) {
        let x = (u * self.screen_width as f32) as i32;
        let y = (v * self.screen_height as f32) as i32;
        platform_mouse_move(x, y);
    }

    /// Click at the position corresponding to (u,v) on the panel.
    pub fn inject_mouse_click(&self, u: f32, v: f32) {
        let x = (u * self.screen_width as f32) as i32;
        let y = (v * self.screen_height as f32) as i32;
        platform_mouse_click(x, y);
    }

    // ================================================================
    // macOS capture via CoreGraphics
    // ================================================================

    #[cfg(target_os = "macos")]
    fn capture_macos(&mut self) -> Option<&[u8]> {
        use std::ffi::c_void;

        type CGDirectDisplayID = u32;
        type CGImageRef = *const c_void;
        type CGDataProviderRef = *const c_void;
        type CFDataRef = *const c_void;
        type CFIndex = isize;

        extern "C" {
            fn CGMainDisplayID() -> CGDirectDisplayID;
            fn CGDisplayCreateImage(display: CGDirectDisplayID) -> CGImageRef;
            fn CGImageGetWidth(image: CGImageRef) -> usize;
            fn CGImageGetHeight(image: CGImageRef) -> usize;
            fn CGImageGetBytesPerRow(image: CGImageRef) -> usize;
            fn CGImageGetBitsPerPixel(image: CGImageRef) -> usize;
            fn CGImageGetDataProvider(image: CGImageRef) -> CGDataProviderRef;
            fn CGDataProviderCopyData(provider: CGDataProviderRef) -> CFDataRef;
            fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
            fn CFDataGetLength(data: CFDataRef) -> CFIndex;
            fn CGImageRelease(image: CGImageRef);
            fn CFRelease(cf: *const c_void);
        }

        unsafe {
            let display = CGMainDisplayID();
            let image = CGDisplayCreateImage(display);
            if image.is_null() {
                return None;
            }

            let src_w = CGImageGetWidth(image);
            let src_h = CGImageGetHeight(image);
            let row_bytes = CGImageGetBytesPerRow(image);
            let bpp = CGImageGetBitsPerPixel(image);

            let provider = CGImageGetDataProvider(image);
            if provider.is_null() {
                CGImageRelease(image);
                return None;
            }

            let data = CGDataProviderCopyData(provider);
            if data.is_null() {
                CGImageRelease(image);
                return None;
            }

            let ptr = CFDataGetBytePtr(data);
            let len = CFDataGetLength(data) as usize;
            let src_pixels = std::slice::from_raw_parts(ptr, len);

            let bytes_per_pixel = (bpp / 8) as usize;
            let dst_w = self.width as usize;
            let dst_h = self.height as usize;

            // Nearest-neighbor scale + BGRA→RGBA conversion + vertical flip
            for dy in 0..dst_h {
                let sy = (dst_h - 1 - dy) * src_h / dst_h;
                let src_row = sy * row_bytes;
                for dx in 0..dst_w {
                    let sx = dx * src_w / dst_w;
                    let src_off = src_row + sx * bytes_per_pixel;
                    let dst_off = (dy * dst_w + dx) * 4;

                    if src_off + 3 < len {
                        self.pixel_buffer[dst_off] = src_pixels[src_off + 2];     // R
                        self.pixel_buffer[dst_off + 1] = src_pixels[src_off + 1]; // G
                        self.pixel_buffer[dst_off + 2] = src_pixels[src_off];     // B
                        self.pixel_buffer[dst_off + 3] = 255;                     // A
                    }
                }
            }

            CFRelease(data);
            CGImageRelease(image);
        }

        self.frame_count += 1;
        Some(&self.pixel_buffer)
    }

    // ================================================================
    // Windows capture via DXGI Desktop Duplication
    // ================================================================

    #[cfg(target_os = "windows")]
    fn capture_windows(&mut self) -> Option<&[u8]> {
        let dxgi = self.dxgi.as_mut()?;

        let frame = match dxgi.acquire_frame() {
            Ok(Some(f)) => f,
            Ok(None) => return None, // timeout / no new frame
            Err(e) => {
                log::debug!("DXGI frame acquire: {}", e);
                return None;
            }
        };

        let dst_w = self.width as usize;
        let dst_h = self.height as usize;
        let src_w = frame.width as usize;
        let src_h = frame.height as usize;
        let row_pitch = frame.pitch as usize;

        // Nearest-neighbor scale + BGRA→RGBA + vertical flip
        for dy in 0..dst_h {
            let sy = (dst_h - 1 - dy) * src_h / dst_h;
            let src_row = sy * row_pitch;
            for dx in 0..dst_w {
                let sx = dx * src_w / dst_w;
                let src_off = src_row + sx * 4;
                let dst_off = (dy * dst_w + dx) * 4;

                if src_off + 3 < frame.data.len() {
                    self.pixel_buffer[dst_off] = frame.data[src_off + 2];     // R
                    self.pixel_buffer[dst_off + 1] = frame.data[src_off + 1]; // G
                    self.pixel_buffer[dst_off + 2] = frame.data[src_off];     // B
                    self.pixel_buffer[dst_off + 3] = 255;                     // A
                }
            }
        }

        if let Err(e) = dxgi.release_frame() {
            log::debug!("DXGI frame release: {}", e);
        }

        self.frame_count += 1;
        Some(&self.pixel_buffer)
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
// Platform: mouse input injection
// ============================================================

#[cfg(target_os = "macos")]
fn platform_mouse_move(x: i32, y: i32) {
    use std::ffi::c_void;
    type CGEventRef = *const c_void;
    type CGEventSourceRef = *const c_void;
    type CGFloat = f64;

    #[repr(C)]
    struct CGPoint {
        x: CGFloat,
        y: CGFloat,
    }

    extern "C" {
        fn CGEventCreateMouseEvent(
            source: CGEventSourceRef,
            mouse_type: u32,
            mouse_cursor_position: CGPoint,
            mouse_button: u32,
        ) -> CGEventRef;
        fn CGEventPost(tap: u32, event: CGEventRef);
        fn CFRelease(cf: *const c_void);
    }

    const K_CG_EVENT_MOUSE_MOVED: u32 = 5;
    const K_CG_HID_EVENT_TAP: u32 = 0;

    unsafe {
        let pt = CGPoint { x: x as CGFloat, y: y as CGFloat };
        let event = CGEventCreateMouseEvent(
            std::ptr::null(),
            K_CG_EVENT_MOUSE_MOVED,
            pt,
            0, // kCGMouseButtonLeft
        );
        if !event.is_null() {
            CGEventPost(K_CG_HID_EVENT_TAP, event);
            CFRelease(event);
        }
    }
}

#[cfg(target_os = "macos")]
fn platform_mouse_click(x: i32, y: i32) {
    use std::ffi::c_void;
    type CGEventRef = *const c_void;
    type CGEventSourceRef = *const c_void;
    type CGFloat = f64;

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct CGPoint {
        x: CGFloat,
        y: CGFloat,
    }

    extern "C" {
        fn CGEventCreateMouseEvent(
            source: CGEventSourceRef,
            mouse_type: u32,
            mouse_cursor_position: CGPoint,
            mouse_button: u32,
        ) -> CGEventRef;
        fn CGEventPost(tap: u32, event: CGEventRef);
        fn CFRelease(cf: *const c_void);
    }

    const K_CG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
    const K_CG_EVENT_LEFT_MOUSE_UP: u32 = 2;
    const K_CG_HID_EVENT_TAP: u32 = 0;

    unsafe {
        let pt = CGPoint { x: x as CGFloat, y: y as CGFloat };

        let down = CGEventCreateMouseEvent(
            std::ptr::null(), K_CG_EVENT_LEFT_MOUSE_DOWN, pt, 0,
        );
        if !down.is_null() {
            CGEventPost(K_CG_HID_EVENT_TAP, down);
            CFRelease(down);
        }

        let up = CGEventCreateMouseEvent(
            std::ptr::null(), K_CG_EVENT_LEFT_MOUSE_UP, pt, 0,
        );
        if !up.is_null() {
            CGEventPost(K_CG_HID_EVENT_TAP, up);
            CFRelease(up);
        }
    }
}

#[cfg(target_os = "windows")]
fn platform_mouse_move(x: i32, y: i32) {
    use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

#[cfg(target_os = "windows")]
fn platform_mouse_click(x: i32, y: i32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEINPUT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    unsafe {
        let screen_w = GetSystemMetrics(SM_CXSCREEN) as i32;
        let screen_h = GetSystemMetrics(SM_CYSCREEN) as i32;

        // Absolute coordinates are 0..65535
        let abs_x = (x * 65535 / screen_w) as i32;
        let abs_y = (y * 65535 / screen_h) as i32;

        let inputs = [
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: abs_x,
                        dy: abs_y,
                        dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE | MOUSEEVENTF_LEFTDOWN,
                        ..Default::default()
                    },
                },
            },
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: abs_x,
                        dy: abs_y,
                        dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE | MOUSEEVENTF_LEFTUP,
                        ..Default::default()
                    },
                },
            },
        ];

        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_mouse_move(_x: i32, _y: i32) {}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_mouse_click(_x: i32, _y: i32) {}

// ============================================================
// Windows: DXGI Desktop Duplication
// ============================================================

#[cfg(target_os = "windows")]
struct FrameData {
    data: Vec<u8>,
    width: u32,
    height: u32,
    pitch: u32,
}

#[cfg(target_os = "windows")]
struct DxgiCapture {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    duplication: windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication,
    staging: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>,
    desc_width: u32,
    desc_height: u32,
    frame_acquired: bool,
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

        // Get DXGI device → adapter → output
        let dxgi_device: IDXGIDevice = device.cast()?;
        let adapter = unsafe { dxgi_device.GetAdapter()? };
        let output = unsafe { adapter.EnumOutputs(0)? };
        let output1: IDXGIOutput1 = output.cast()?;

        // Duplicate the output
        let duplication = unsafe { output1.DuplicateOutput(&device)? };

        // Get output dimensions
        let mut out_desc = Default::default();
        unsafe { output.GetDesc(&mut out_desc)? };
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
        })
    }

    fn acquire_frame(&mut self) -> Result<Option<FrameData>> {
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
                // DXGI_ERROR_WAIT_TIMEOUT — no new frame
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
                BindFlags: D3D11_BIND_FLAG(0),
                CPUAccessFlags: D3D11_CPU_ACCESS_READ,
                MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
            };
            let staging = unsafe { self.device.CreateTexture2D(&staging_desc, None)? };
            self.staging = Some(staging);
            self.desc_width = desc.Width;
            self.desc_height = desc.Height;
        }

        let staging = self.staging.as_ref().unwrap();

        // Copy desktop texture → staging
        unsafe {
            self.context.CopyResource(staging, &texture);
        }

        // Map staging to CPU
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }

        let pitch = mapped.RowPitch;
        let data_size = (pitch * desc.Height) as usize;
        let data = unsafe {
            std::slice::from_raw_parts(mapped.pData as *const u8, data_size)
        }
        .to_vec();

        unsafe {
            self.context.Unmap(staging, 0);
        }

        Ok(Some(FrameData {
            data,
            width: desc.Width,
            height: desc.Height,
            pitch,
        }))
    }

    fn release_frame(&mut self) -> Result<()> {
        if self.frame_acquired {
            unsafe { self.duplication.ReleaseFrame()? };
            self.frame_acquired = false;
        }
        Ok(())
    }
}

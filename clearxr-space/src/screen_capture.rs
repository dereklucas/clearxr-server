/// Screen / window capture — grabs pixels from the desktop or a specific window.
///
/// macOS: CoreGraphics CGWindowListCreateImage / CGDisplayCreateImage
/// Windows: (TODO) DXGI Desktop Duplication or Windows.Graphics.Capture

use anyhow::Result;
use log::info;

pub struct ScreenCapture {
    width: u32,
    height: u32,
    pixel_buffer: Vec<u8>, // RGBA
    frame_count: u64,
}

impl ScreenCapture {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        info!("ScreenCapture: {}x{}", width, height);
        Ok(Self {
            width,
            height,
            pixel_buffer: vec![0u8; (width * height * 4) as usize],
            frame_count: 0,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Capture the main display and return RGBA pixels scaled to our target size.
    /// Returns None if capture failed (non-fatal, just skip the frame).
    pub fn capture(&mut self) -> Option<&[u8]> {
        #[cfg(target_os = "macos")]
        {
            self.capture_macos()
        }
        #[cfg(target_os = "windows")]
        {
            self.capture_windows()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            None
        }
    }

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

            // Nearest-neighbor scale + BGRA→RGBA conversion
            for dy in 0..dst_h {
                let sy = dy * src_h / dst_h;
                let src_row = sy * row_bytes;
                for dx in 0..dst_w {
                    let sx = dx * src_w / dst_w;
                    let src_off = src_row + sx * bytes_per_pixel;
                    let dst_off = (dy * dst_w + dx) * 4;

                    if src_off + 3 < len {
                        // CoreGraphics is BGRA (or BGRX) on macOS
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

    #[cfg(target_os = "windows")]
    fn capture_windows(&mut self) -> Option<&[u8]> {
        // TODO: DXGI Desktop Duplication or Windows.Graphics.Capture
        None
    }
}

//! VR screenshot capture.
//!
//! Captures the current VR viewport to a PNG file.
//! Uses the `image` crate for PNG encoding.

use std::path::PathBuf;
use std::time::SystemTime;
use log::info;

/// Result of a screenshot capture.
#[derive(Debug)]
#[allow(dead_code)] // Fields are part of the public API
pub struct ScreenshotResult {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

/// Get the default screenshot directory.
pub fn screenshot_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|d| d.picture_dir().map(|p| p.join("ClearXR")))
        .unwrap_or_else(|| PathBuf::from("screenshots"))
}

/// Generate a timestamped filename.
pub fn screenshot_filename() -> String {
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("clearxr_{}.png", timestamp)
}

/// Save RGBA pixel data as a PNG file.
/// Returns the path of the saved file.
pub fn save_screenshot(
    pixels: &[u8],
    width: u32,
    height: u32,
) -> Result<ScreenshotResult, String> {
    let expected_len = (width as usize) * (height as usize) * 4;
    if pixels.len() != expected_len {
        return Err(format!(
            "Invalid buffer length: expected {} got {} for {}x{} image",
            expected_len,
            pixels.len(),
            width,
            height,
        ));
    }

    let dir = screenshot_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create screenshot directory: {}", e))?;

    let path = dir.join(screenshot_filename());

    image::save_buffer(
        &path,
        pixels,
        width,
        height,
        image::ColorType::Rgba8,
    )
    .map_err(|e| format!("Failed to save screenshot: {}", e))?;

    info!("Screenshot saved: {}", path.display());

    Ok(ScreenshotResult { path, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_dir_exists_or_creatable() {
        let dir = screenshot_dir();
        // Should return a non-empty path
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn screenshot_filename_contains_timestamp() {
        let name = screenshot_filename();
        assert!(name.starts_with("clearxr_"));
        assert!(name.ends_with(".png"));
    }

    #[test]
    fn save_screenshot_small_image() {
        let width = 4u32;
        let height = 4u32;
        let pixels = vec![0xFFu8; (width * height * 4) as usize]; // white image

        let result = save_screenshot(&pixels, width, height);
        assert!(result.is_ok(), "Failed: {:?}", result.err());

        let info = result.unwrap();
        assert_eq!(info.width, width);
        assert_eq!(info.height, height);
        assert!(info.path.exists());

        // Clean up
        std::fs::remove_file(&info.path).ok();
    }

    #[test]
    fn save_screenshot_wrong_size_fails() {
        // 2x2 image but only 4 bytes (should be 16)
        let result = save_screenshot(&[0u8; 4], 2, 2);
        assert!(result.is_err());
    }
}

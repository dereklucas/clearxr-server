//! Shared memory for dashboard metadata.
//!
//! Contains panel pose, visibility, GPU LUID.
//! NO pixel data — pixels are shared via VK_KHR_external_memory_win32.

use shared_memory::{Shmem, ShmemConf, ShmemError};
use std::sync::atomic::{AtomicU32, Ordering};

/// Name of the shared memory region.
// v2: the region now carries pixel data after the header. Bumped name so a
// stale header-only region from an older build can never be reused by mistake.
pub const SHM_NAME: &str = "ClearXR_Dashboard_v2";

/// Named Vulkan handle for the shared image (must match layer's overlay.rs).
pub const IMAGE_HANDLE_NAME: &str = "ClearXR_DashboardImage";

/// Fixed header (64 bytes). Must match clearxr-layer/src/overlay.rs ShmHeader exactly.
#[repr(C)]
pub struct ShmHeader {
    pub frame_counter: AtomicU32,   // 0
    pub width: u32,                 // 4
    pub height: u32,                // 8
    pub flags: u32,                 // 12
    pub panel_pos: [f32; 3],        // 16
    pub panel_orient: [f32; 4],     // 28
    pub panel_size: [f32; 2],       // 44
    pub gpu_luid: [u8; 8],         // 52
    pub _reserved: [u8; 4],        // 60 -> total 64
}

pub const HEADER_SIZE: usize = 64;
const _: () = assert!(std::mem::size_of::<ShmHeader>() == HEADER_SIZE);

/// Offset within SHM where raw RGBA8_SRGB pixel data begins (immediately after header).
pub const PIXEL_DATA_OFFSET: usize = HEADER_SIZE;

/// Writer side (dashboard process). Creates shared memory for metadata.
pub struct ShmWriter {
    shmem: Shmem,
}

impl ShmWriter {
    pub fn create(width: u32, height: u32) -> Result<Self, ShmemError> {
        // Double-buffered pixel region: two full frames after the header. The
        // writer fills the slot for the next frame while the reader is still
        // copying the previously-published slot, so a ~10 MB reader copy can
        // never tear against the writer. Slot = frame_counter & 1.
        let total_size = HEADER_SIZE + 2 * (width * height * 4) as usize;
        let shmem = match ShmemConf::new()
            .size(total_size)
            .os_id(SHM_NAME)
            .create()
        {
            Ok(s) => s,
            Err(ShmemError::MappingIdExists) => {
                log::info!("[ClearXR Dashboard] SHM already exists, reusing.");
                ShmemConf::new().os_id(SHM_NAME).open()?
            }
            Err(e) => return Err(e),
        };

        unsafe {
            let header = &mut *(shmem.as_ptr() as *mut ShmHeader);
            header.frame_counter = AtomicU32::new(0);
            header.width = width;
            header.height = height;
            header.flags = 1;
            header.panel_pos = [0.0, 1.2, -2.0];
            header.panel_orient = [0.0, 0.0, 0.0, 1.0];
            header.panel_size = [1.6, 1.0];
            header.gpu_luid = [0; 8];
        }

        log::info!("[ClearXR Dashboard] SHM created: {}x{}, header + double-buffered pixels ({} bytes)", width, height, total_size);
        Ok(Self { shmem })
    }

    pub fn bump_frame_counter(&self) {
        unsafe {
            let header = &*(self.shmem.as_ptr() as *const ShmHeader);
            header.frame_counter.fetch_add(1, Ordering::Release);
        }
    }

    pub fn set_gpu_luid(&self, luid: [u8; 8]) {
        unsafe {
            let header = &mut *(self.shmem.as_ptr() as *mut ShmHeader);
            header.gpu_luid = luid;
        }
    }

    pub fn set_panel_pose(&self, pos: [f32; 3], orient: [f32; 4], size: [f32; 2]) {
        unsafe {
            let header = &mut *(self.shmem.as_ptr() as *mut ShmHeader);
            header.panel_pos = pos;
            header.panel_orient = orient;
            header.panel_size = size;
        }
    }

    pub fn set_visible(&self, visible: bool) {
        unsafe {
            let header = &mut *(self.shmem.as_ptr() as *mut ShmHeader);
            if visible { header.flags |= 1; } else { header.flags &= !1; }
        }
    }

    /// Write raw RGBA8_SRGB pixel data into the SHM pixel buffer.
    /// `data` must be exactly `width * height * 4` bytes.
    ///
    /// Double-buffered: the data is written to the slot for the *next* frame
    /// (`(frame_counter + 1) & 1`), which is the slot the reader is NOT reading.
    /// Call `bump_frame_counter()` immediately after to publish it. The slot
    /// math here must match the reader in clearxr-layer/src/overlay_d3d11.rs.
    pub fn write_pixels(&self, data: &[u8]) {
        // Per-slot capacity: half the pixel region. Never trust the mapping
        // size — a reused region from a run with different dimensions would
        // otherwise be overflowed.
        let slot_cap = self.shmem.len().saturating_sub(PIXEL_DATA_OFFSET) / 2;
        let n = data.len().min(slot_cap);
        if n < data.len() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                log::error!(
                    "[ClearXR Dashboard] SHM slot too small for a frame ({} < {} bytes); \
                     writing truncated pixels. Close stale ClearXR processes and restart.",
                    slot_cap,
                    data.len()
                );
            }
        }
        unsafe {
            let header = &*(self.shmem.as_ptr() as *const ShmHeader);
            let next_frame = header.frame_counter.load(Ordering::Relaxed).wrapping_add(1);
            let slot = (next_frame & 1) as usize;
            let base = self.shmem.as_ptr().add(PIXEL_DATA_OFFSET + slot * slot_cap);
            std::ptr::copy_nonoverlapping(data.as_ptr(), base, n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shm_header_size() {
        assert_eq!(std::mem::size_of::<ShmHeader>(), 64);
    }

    #[test]
    fn test_shm_header_field_offsets() {
        assert_eq!(memoffset_of!(ShmHeader, frame_counter), 0);
        assert_eq!(memoffset_of!(ShmHeader, width), 4);
        assert_eq!(memoffset_of!(ShmHeader, height), 8);
        assert_eq!(memoffset_of!(ShmHeader, flags), 12);
        assert_eq!(memoffset_of!(ShmHeader, panel_pos), 16);
        assert_eq!(memoffset_of!(ShmHeader, panel_orient), 28);
        assert_eq!(memoffset_of!(ShmHeader, panel_size), 44);
        assert_eq!(memoffset_of!(ShmHeader, gpu_luid), 52);
    }
}

macro_rules! memoffset_of {
    ($type:ty, $field:ident) => {{
        let uninit = std::mem::MaybeUninit::<$type>::uninit();
        let base = uninit.as_ptr() as usize;
        let field = unsafe { std::ptr::addr_of!((*uninit.as_ptr()).$field) } as usize;
        field - base
    }};
}
use memoffset_of;

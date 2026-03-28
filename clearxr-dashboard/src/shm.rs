//! Shared memory for dashboard metadata.
//!
//! Contains panel pose, visibility, GPU LUID, and timeline semaphore counter.
//! NO pixel data — pixels are shared via VK_KHR_external_memory_win32.

use shared_memory::{Shmem, ShmemConf, ShmemError};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Name of the shared memory region.
pub const SHM_NAME: &str = "ClearXR_Dashboard_Meta";

/// Named Vulkan handles for cross-process GPU resource sharing.
pub const IMAGE_HANDLE_NAME: &str = "ClearXR_DashboardImage";
pub const SEMAPHORE_HANDLE_NAME: &str = "ClearXR_DashboardSemaphore";

/// Fixed header (80 bytes). Metadata only — no pixel data.
#[repr(C)]
pub struct ShmHeader {
    /// Incremented after each GPU render submit. Layer checks for new frames.
    pub frame_counter: AtomicU32,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Bit 0 = overlay visible.
    pub flags: u32,
    /// Panel center position in world space [x, y, z].
    pub panel_pos: [f32; 3],
    /// Panel orientation quaternion [x, y, z, w].
    pub panel_orient: [f32; 4],
    /// Panel physical size in meters [width, height].
    pub panel_size: [f32; 2],
    /// Timeline semaphore value the dashboard signaled after its last render.
    pub semaphore_counter: AtomicU64,
    /// GPU LUID (from VkPhysicalDeviceIDProperties). Dashboard matches against this.
    pub gpu_luid: [u8; 8],
    /// Reserved for future use.
    pub _reserved: [u8; 4],
}

pub const HEADER_SIZE: usize = 80;
const _: () = assert!(std::mem::size_of::<ShmHeader>() == HEADER_SIZE);

/// Writer side (dashboard process). Creates shared memory for metadata.
pub struct ShmWriter {
    shmem: Shmem,
}

impl ShmWriter {
    /// Create or open the shared memory region (metadata only, no pixels).
    pub fn create(width: u32, height: u32) -> Result<Self, ShmemError> {
        let shmem = match ShmemConf::new()
            .size(HEADER_SIZE)
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

        // Initialize header
        unsafe {
            let header = &mut *(shmem.as_ptr() as *mut ShmHeader);
            header.frame_counter = AtomicU32::new(0);
            header.width = width;
            header.height = height;
            header.flags = 1; // visible by default
            header.panel_pos = [0.0, 1.2, -2.0];
            header.panel_orient = [0.0, 0.0, 0.0, 1.0];
            header.panel_size = [1.6, 1.0];
            header.semaphore_counter = AtomicU64::new(0);
            header.gpu_luid = [0; 8];
        }

        log::info!("[ClearXR Dashboard] SHM created: {}x{}, metadata only ({} bytes)", width, height, HEADER_SIZE);
        Ok(Self { shmem })
    }

    /// Increment the frame counter (release ordering).
    pub fn bump_frame_counter(&self) {
        unsafe {
            let header = &*(self.shmem.as_ptr() as *const ShmHeader);
            header.frame_counter.fetch_add(1, Ordering::Release);
        }
    }

    /// Write the timeline semaphore counter value.
    pub fn set_semaphore_counter(&self, value: u64) {
        unsafe {
            let header = &*(self.shmem.as_ptr() as *const ShmHeader);
            header.semaphore_counter.store(value, Ordering::Release);
        }
    }

    /// Write the GPU LUID so the layer can verify device matching.
    pub fn set_gpu_luid(&self, luid: [u8; 8]) {
        unsafe {
            let header = &mut *(self.shmem.as_ptr() as *mut ShmHeader);
            header.gpu_luid = luid;
        }
    }

    /// Update the panel pose in the header.
    pub fn set_panel_pose(&self, pos: [f32; 3], orient: [f32; 4], size: [f32; 2]) {
        unsafe {
            let header = &mut *(self.shmem.as_ptr() as *mut ShmHeader);
            header.panel_pos = pos;
            header.panel_orient = orient;
            header.panel_size = size;
        }
    }

    /// Set the visibility flag.
    pub fn set_visible(&self, visible: bool) {
        unsafe {
            let header = &mut *(self.shmem.as_ptr() as *mut ShmHeader);
            if visible { header.flags |= 1; } else { header.flags &= !1; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shm_header_size() {
        assert_eq!(std::mem::size_of::<ShmHeader>(), 80);
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
        assert_eq!(memoffset_of!(ShmHeader, semaphore_counter), 56); // 4 bytes padding after panel_size for u64 alignment
        assert_eq!(memoffset_of!(ShmHeader, gpu_luid), 64);
    }
}

/// Compile-time offset-of macro (no external dependency).
#[cfg(test)]
macro_rules! memoffset_of {
    ($type:ty, $field:ident) => {{
        let uninit = std::mem::MaybeUninit::<$type>::uninit();
        let base = uninit.as_ptr() as usize;
        let field = unsafe { std::ptr::addr_of!((*uninit.as_ptr()).$field) } as usize;
        field - base
    }};
}
#[cfg(test)]
use memoffset_of;

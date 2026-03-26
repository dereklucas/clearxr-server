//! Shared memory writer for the dashboard framebuffer.
//!
//! Creates a named shared memory region containing a header + RGBA pixel buffer.
//! The layer reads this to display the dashboard overlay.

use shared_memory::{Shmem, ShmemConf, ShmemError};
use std::sync::atomic::{AtomicU32, Ordering};

/// Name of the shared memory region.
pub const SHM_NAME: &str = "ClearXR_Dashboard_Frame";

/// Fixed header at the start of shared memory (64 bytes).
#[repr(C)]
pub struct ShmHeader {
    /// Incremented after each frame write. Layer checks this for new content.
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
    /// Reserved for future use.
    pub _reserved: [u8; 12],
}

const HEADER_SIZE: usize = 64;
const _: () = assert!(std::mem::size_of::<ShmHeader>() == HEADER_SIZE);

/// Writer side (dashboard process). Creates the shared memory and writes frames.
pub struct ShmWriter {
    shmem: Shmem,
    width: u32,
    height: u32,
}

impl ShmWriter {
    /// Raw pointer to the start of the shared memory region.
    pub fn ptr(&self) -> *const u8 {
        self.shmem.as_ptr()
    }

    /// Create or open the shared memory region for the given frame size.
    /// If a previous run left SHM behind, we reuse it.
    pub fn create(width: u32, height: u32) -> Result<Self, ShmemError> {
        let pixel_size = (width * height * 4) as usize;
        let total_size = HEADER_SIZE + pixel_size;

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

        // Initialize header
        let ptr = shmem.as_ptr();
        unsafe {
            let header = &mut *(ptr as *mut ShmHeader);
            header.frame_counter = AtomicU32::new(0);
            header.width = width;
            header.height = height;
            header.flags = 1; // visible by default
            header.panel_pos = [0.0, 1.2, -2.0]; // 2m in front, chest height
            header.panel_orient = [0.0, 0.0, 0.0, 1.0]; // identity (facing -Z)
            header.panel_size = [1.6, 1.0]; // 1.6m x 1.0m
        }

        log::info!(
            "[ClearXR Dashboard] SHM created: {}x{}, {} bytes total",
            width, height, total_size
        );

        Ok(Self { shmem, width, height })
    }

    /// Write RGBA pixel data and increment the frame counter.
    pub fn write_frame(&self, pixels: &[u8]) {
        let expected = (self.width * self.height * 4) as usize;
        if pixels.len() != expected {
            log::warn!(
                "[ClearXR Dashboard] Frame size mismatch: got {}, expected {}",
                pixels.len(), expected
            );
            return;
        }

        unsafe {
            let ptr = self.shmem.as_ptr();
            let pixel_dst = ptr.add(HEADER_SIZE);
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), pixel_dst, pixels.len());

            // Increment frame counter AFTER writing pixels (release ordering).
            let header = &*(ptr as *const ShmHeader);
            header.frame_counter.fetch_add(1, Ordering::Release);
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
            if visible {
                header.flags |= 1;
            } else {
                header.flags &= !1;
            }
        }
    }
}

/// Reader side (layer process). Opens existing shared memory and reads frames.
pub struct ShmReader {
    shmem: Shmem,
}

impl ShmReader {
    /// Open an existing shared memory region by name.
    pub fn open() -> Result<Self, ShmemError> {
        let shmem = ShmemConf::new()
            .os_id(SHM_NAME)
            .open()?;
        Ok(Self { shmem })
    }

    /// Get a reference to the header.
    pub fn header(&self) -> &ShmHeader {
        unsafe { &*(self.shmem.as_ptr() as *const ShmHeader) }
    }

    /// Read the current frame counter (acquire ordering).
    pub fn frame_counter(&self) -> u32 {
        self.header().frame_counter.load(Ordering::Acquire)
    }

    /// Copy pixel data into the provided buffer.
    pub fn read_pixels(&self, dst: &mut [u8]) {
        let header = self.header();
        let expected = (header.width * header.height * 4) as usize;
        let copy_len = dst.len().min(expected);
        unsafe {
            let src = self.shmem.as_ptr().add(HEADER_SIZE);
            std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), copy_len);
        }
    }

    /// Get the pixel data pointer directly (for zero-copy staging buffer writes).
    pub fn pixel_ptr(&self) -> *const u8 {
        unsafe { self.shmem.as_ptr().add(HEADER_SIZE) }
    }

    pub fn width(&self) -> u32 {
        self.header().width
    }

    pub fn height(&self) -> u32 {
        self.header().height
    }

    pub fn visible(&self) -> bool {
        self.header().flags & 1 != 0
    }

    pub fn panel_pos(&self) -> [f32; 3] {
        self.header().panel_pos
    }

    pub fn panel_orient(&self) -> [f32; 4] {
        self.header().panel_orient
    }

    pub fn panel_size(&self) -> [f32; 2] {
        self.header().panel_size
    }
}

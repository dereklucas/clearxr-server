//! Named pipe server for receiving controller input from the layer.
//!
//! The layer writes raw `SpatialControllerPacket` bytes to the pipe.
//! The dashboard reads them to perform ray-quad intersection and egui input.

use std::io::Read;

/// Name of the named pipe.
pub const PIPE_NAME: &str = r"\\.\pipe\ClearXR_Controller_Input";

/// Controller hand data (matches the layer's opaque.rs exactly).
#[repr(C, packed)]
#[derive(Copy, Clone, Default)]
pub struct SpatialControllerHand {
    pub buttons: u16,
    pub _reserved: u16,
    pub thumbstick_x: f32,
    pub thumbstick_y: f32,
    pub trigger: f32,
    pub grip: f32,
    pub pos_x: f32,
    pub pos_y: f32,
    pub pos_z: f32,
    pub rot_x: f32,
    pub rot_y: f32,
    pub rot_z: f32,
    pub rot_w: f32,
}

/// Controller packet (matches the layer's opaque.rs exactly).
#[repr(C, packed)]
#[derive(Copy, Clone, Default)]
pub struct SpatialControllerPacket {
    pub magic: u16,
    pub version: u8,
    pub active_hands: u8,
    pub left: SpatialControllerHand,
    pub right: SpatialControllerHand,
}

pub const SC_BTN_MENU: u16 = 1 << 5;

const PACKET_SIZE: usize = std::mem::size_of::<SpatialControllerPacket>();

/// Server side (dashboard process): creates pipe and reads controller packets.
pub struct InputPipeServer {
    #[cfg(target_os = "windows")]
    pipe: Option<windows::Win32::Foundation::HANDLE>,
    buffer: [u8; PACKET_SIZE],
}

impl InputPipeServer {
    /// Create the named pipe server. Non-blocking.
    pub fn create() -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Storage::FileSystem::*;
            use windows::Win32::System::Pipes::*;
            use windows::core::PCSTR;

            let name = format!("{}\0", PIPE_NAME);
            let handle = unsafe {
                CreateNamedPipeA(
                    PCSTR(name.as_ptr()),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT,
                    1, // max instances
                    0, // out buffer
                    PACKET_SIZE as u32 * 4, // in buffer
                    0, // default timeout
                    None, // default security
                )
            };

            match handle {
                Ok(h) => {
                    log::info!("[ClearXR Dashboard] Named pipe created: {}", PIPE_NAME);
                    Ok(Self {
                        pipe: Some(h),
                        buffer: [0u8; PACKET_SIZE],
                    })
                }
                Err(e) => Err(format!("CreateNamedPipe failed: {}", e)),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            log::warn!("[ClearXR Dashboard] Named pipes not supported on this platform.");
            Ok(Self {
                buffer: [0u8; PACKET_SIZE],
            })
        }
    }

    /// Try to read the latest controller packet. Non-blocking.
    /// Returns None if no data available or client not connected.
    pub fn try_read(&mut self) -> Option<SpatialControllerPacket> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::System::Pipes::ConnectNamedPipe;
            use windows::Win32::System::IO::*;

            let handle = self.pipe?;

            // Try to accept a client connection (non-blocking due to PIPE_NOWAIT).
            unsafe { ConnectNamedPipe(handle, None).ok() };

            // Non-blocking read
            let mut bytes_read = 0u32;
            let ok = unsafe {
                windows::Win32::Storage::FileSystem::ReadFile(
                    handle,
                    Some(&mut self.buffer),
                    Some(&mut bytes_read),
                    None,
                )
            };

            if ok.is_ok() && bytes_read as usize == PACKET_SIZE {
                let pkt: SpatialControllerPacket =
                    unsafe { std::ptr::read(self.buffer.as_ptr() as *const SpatialControllerPacket) };
                if pkt.magic == 0x5343 {
                    return Some(pkt);
                }
            }
            None
        }

        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }
}

impl Drop for InputPipeServer {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            if let Some(handle) = self.pipe.take() {
                use windows::Win32::Foundation::CloseHandle;
                unsafe { CloseHandle(handle).ok() };
            }
        }
        log::info!("[ClearXR Dashboard] Input pipe server closed.");
    }
}

/// Client side (layer process): connects to pipe and writes controller packets.
pub struct InputPipeClient {
    #[cfg(target_os = "windows")]
    handle: Option<windows::Win32::Foundation::HANDLE>,
}

impl InputPipeClient {
    /// Try to connect to the dashboard's named pipe.
    pub fn connect() -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Storage::FileSystem::*;
            use windows::core::PCSTR;

            let name = format!("{}\0", PIPE_NAME);
            let handle = unsafe {
                CreateFileA(
                    PCSTR(name.as_ptr()),
                    0x40000000, // GENERIC_WRITE
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            };

            match handle {
                Ok(h) => Ok(Self { handle: Some(h) }),
                Err(e) => Err(format!("Failed to connect to pipe: {}", e)),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            Err("Named pipes not supported on this platform".into())
        }
    }

    /// Write a controller packet to the pipe.
    pub fn write_packet(&self, pkt: &SpatialControllerPacket) -> bool {
        #[cfg(target_os = "windows")]
        {
            let Some(handle) = self.handle else { return false };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    pkt as *const SpatialControllerPacket as *const u8,
                    PACKET_SIZE,
                )
            };
            let mut written = 0u32;
            let ok = unsafe {
                windows::Win32::Storage::FileSystem::WriteFile(handle, Some(bytes), Some(&mut written), None)
            };
            ok.is_ok() && written as usize == PACKET_SIZE
        }

        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }
}

impl Drop for InputPipeClient {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            if let Some(handle) = self.handle.take() {
                use windows::Win32::Foundation::CloseHandle;
                unsafe { CloseHandle(handle).ok() };
            }
        }
    }
}

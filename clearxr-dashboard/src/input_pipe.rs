//! Named pipe for receiving pre-computed dashboard input from the layer.
//!
//! The layer computes ray-quad intersection against the dashboard panel,
//! then sends UV hit + button state through this pipe. The dashboard just
//! feeds it to egui — no spatial math needed here.

/// Name of the named pipe.
pub const PIPE_NAME: &str = r"\\.\pipe\ClearXR_Controller_Input";

/// Pre-computed input packet from the layer. The layer does all spatial math
/// (ray-quad intersection) and sends just the UI-relevant result.
///
/// 24 bytes, matches the layer's definition exactly.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct DashboardInputPacket {
    pub magic: u16,        // 0x4449 ("DI")
    pub flags: u8,         // bit 0: has_pointer
    pub _pad: u8,
    pub pointer_u: f32,    // UV coords (valid if has_pointer)
    pub pointer_v: f32,
    pub trigger: f32,      // 0.0-1.0 raw value
    pub grip: f32,         // 0.0-1.0 raw value
    pub thumbstick_y: f32, // -1.0 to 1.0 raw value
}

const PACKET_SIZE: usize = std::mem::size_of::<DashboardInputPacket>();

/// Server side (dashboard process): creates pipe and reads controller packets.
pub struct InputPipeServer {
    #[cfg(target_os = "windows")]
    pipe: Option<windows::Win32::Foundation::HANDLE>,
    #[cfg(target_os = "windows")]
    connected: bool,
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
                    PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_NOWAIT,
                    1, // max instances
                    0, // out buffer
                    PACKET_SIZE as u32 * 8, // in buffer (several frames of packets)
                    0, // default timeout
                    None, // default security
                )
            };

            match handle {
                Ok(h) => {
                    log::info!("[ClearXR Dashboard] Named pipe created: {}", PIPE_NAME);
                    Ok(Self {
                        pipe: Some(h),
                        connected: false,
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
    /// Drain all pending packets from the pipe and return the latest one.
    /// With PIPE_TYPE_MESSAGE, each ReadFile returns exactly one complete packet.
    pub fn try_read(&mut self) -> Option<DashboardInputPacket> {
        #[cfg(target_os = "windows")]
        {
            let handle = self.pipe?;

            // Only call ConnectNamedPipe when not yet connected (avoids kernel syscall per frame).
            if !self.connected {
                use windows::Win32::System::Pipes::ConnectNamedPipe;
                let result = unsafe { ConnectNamedPipe(handle, None) };
                // ERROR_PIPE_CONNECTED (0x217) means client already connected — that's fine.
                if result.is_ok() {
                    self.connected = true;
                } else if let Err(ref e) = result {
                    if e.code().0 as u32 == 0x80070217 {
                        self.connected = true;
                    }
                }
                if !self.connected {
                    return None;
                }
            }

            // Drain pending messages, keep only the latest. Cap at 16 to prevent
            // stall-burst if the dashboard was blocked and packets accumulated.
            let mut latest: Option<DashboardInputPacket> = None;
            for _ in 0..16 {
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
                    let pkt: DashboardInputPacket =
                        unsafe { std::ptr::read_unaligned(self.buffer.as_ptr() as *const DashboardInputPacket) };
                    if pkt.magic == 0x4449 {
                        latest = Some(pkt);
                    }
                } else {
                    // No more data, or read error (client disconnected)
                    if latest.is_none() {
                        // If we got nothing, client may have disconnected
                        if let Err(ref e) = ok {
                            let code = e.code().0 as u32;
                            // ERROR_BROKEN_PIPE or ERROR_NO_DATA → mark disconnected
                            if code == 0x8007006D || code == 0x800700E8 {
                                self.connected = false;
                            }
                        }
                    }
                    break;
                }
            }
            latest
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
    pub fn write_packet(&self, pkt: &DashboardInputPacket) -> bool {
        #[cfg(target_os = "windows")]
        {
            let Some(handle) = self.handle else { return false };
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    pkt as *const DashboardInputPacket as *const u8,
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

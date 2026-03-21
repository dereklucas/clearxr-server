/// Opaque data channel — reads SpatialControllerPacket from CloudXR.
/// Simplified port from clear-xr/src/opaque_channel.rs for use in the API layer.

use openxr_sys as xr;
use std::ffi::c_char;

macro_rules! opaque_log {
    (warn, $($arg:tt)*) => {{
        let message = format!($($arg)*);
        crate::debug_log(log::Level::Warn, &message);
    }};
    (info, $($arg:tt)*) => {{
        let message = format!($($arg)*);
        crate::debug_log(log::Level::Info, &message);
    }};
}

// ============================================================
// Spatial controller packet (100 bytes, packed, little-endian)
// ============================================================

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

#[repr(C, packed)]
#[derive(Copy, Clone, Default)]
pub struct SpatialControllerPacket {
    pub magic: u16,
    pub version: u8,
    pub active_hands: u8,
    pub left: SpatialControllerHand,
    pub right: SpatialControllerHand,
}

const PACKET_MAGIC: u16 = 0x5343;

// Button bitmask
pub const SC_BTN_A: u16            = 1 << 0;
pub const SC_BTN_B: u16            = 1 << 1;
pub const SC_BTN_THUMBSTICK: u16   = 1 << 4;
pub const SC_BTN_MENU: u16         = 1 << 5;
pub const SC_TOUCH_A: u16          = 1 << 6;
pub const SC_TOUCH_B: u16          = 1 << 7;
pub const SC_TOUCH_TRIGGER: u16    = 1 << 8;
pub const SC_TOUCH_THUMBSTICK: u16 = 1 << 10;


// ============================================================
// Haptic event packet (20 bytes, packed, little-endian)
// Sent PC → headset over the opaque data channel.
// ============================================================

#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct HapticEventPacket {
    pub magic: u16,       // 0x4856 ("HV")
    pub version: u8,      // 1
    pub hand: u8,         // 0 = left, 1 = right
    pub duration_ns: u64, // nanoseconds (0 = minimum)
    pub frequency: f32,   // Hz (0 = default)
    pub amplitude: f32,   // 0.0–1.0
}

const HAPTIC_MAGIC: u16 = 0x4856; // "HV"

// ============================================================
// FFI types for XR_NV_opaque_data_channel
// ============================================================

const XR_TYPE_CREATE_INFO: u64 = 1000500000;
const XR_TYPE_STATE: u64       = 1000500001;
const STATUS_CONNECTED: i32    = 1;
const STATUS_DISCONNECTED: i32 = 3;

#[repr(C)]
struct XrGuid { data1: u32, data2: u16, data3: u16, data4: [u8; 8] }

#[repr(C)]
struct CreateInfoNV {
    ty: u64,
    next: *const std::ffi::c_void,
    system_id: u64,
    uuid: XrGuid,
}

#[repr(C)]
struct StateNV {
    ty: u64,
    next: *mut std::ffi::c_void,
    state: i32,
}

type FnCreate   = unsafe extern "system" fn(xr::Instance, *const CreateInfoNV, *mut u64) -> xr::Result;
type FnDestroy  = unsafe extern "system" fn(u64) -> xr::Result;
type FnGetState = unsafe extern "system" fn(u64, *mut StateNV) -> xr::Result;
type FnShutdown = unsafe extern "system" fn(u64) -> xr::Result;
type FnSend     = unsafe extern "system" fn(u64, u32, *const u8) -> xr::Result;
type FnReceive  = unsafe extern "system" fn(u64, u32, *mut u32, *mut u8) -> xr::Result;

// ============================================================
// OpaqueChannel
// ============================================================

pub struct OpaqueChannel {
    fn_create: FnCreate,
    fn_destroy: FnDestroy,
    fn_get_state: FnGetState,
    fn_shutdown: FnShutdown,
    fn_send: FnSend,
    fn_receive: FnReceive,
    channel: u64,
    instance: xr::Instance,
    system_id: u64,
    connected: bool,
    recv_buf: [u8; 4096],
    pub latest: Option<SpatialControllerPacket>,
    reconnect_after: Option<std::time::Instant>,
}

impl OpaqueChannel {
    /// Load extension function pointers. Returns None if not available.
    pub unsafe fn load(
        get_proc: xr::pfn::GetInstanceProcAddr,
        instance: xr::Instance,
        system_id: u64,
    ) -> Option<Self> {
        let load = |name: &[u8]| -> Option<xr::pfn::VoidFunction> {
            let mut fp: Option<xr::pfn::VoidFunction> = None;
            (get_proc)(instance, name.as_ptr() as *const c_char, &mut fp);
            fp
        };

        Some(Self {
            fn_create:    std::mem::transmute(load(b"xrCreateOpaqueDataChannelNV\0")?),
            fn_destroy:   std::mem::transmute(load(b"xrDestroyOpaqueDataChannelNV\0")?),
            fn_get_state: std::mem::transmute(load(b"xrGetOpaqueDataChannelStateNV\0")?),
            fn_shutdown:  std::mem::transmute(load(b"xrShutdownOpaqueDataChannelNV\0")?),
            fn_send:      std::mem::transmute(load(b"xrSendOpaqueDataChannelNV\0")?),
            fn_receive:   std::mem::transmute(load(b"xrReceiveOpaqueDataChannelNV\0")?),
            channel: 0,
            instance,
            system_id,
            connected: false,
            recv_buf: [0u8; 4096],
            latest: None,
            reconnect_after: None,
        })
    }

    pub unsafe fn create_channel(&mut self) -> bool {
        let ci = CreateInfoNV {
            ty: XR_TYPE_CREATE_INFO,
            next: std::ptr::null(),
            system_id: self.system_id,
            uuid: XrGuid {
                data1: 0x12345678, data2: 0x1234, data3: 0x1234,
                data4: [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0],
            },
        };
        let mut handle: u64 = 0;
        let r = (self.fn_create)(self.instance, &ci, &mut handle);
        if r != xr::Result::SUCCESS {
            opaque_log!(warn, "[ClearXR Layer] Failed to create opaque channel: {:?}", r);
            return false;
        }
        self.channel = handle;
        opaque_log!(info, "[ClearXR Layer] Opaque data channel created.");
        true
    }

    /// Poll once per frame. Returns the latest valid packet if any.
    pub unsafe fn poll(&mut self) -> Option<SpatialControllerPacket> {
        if self.channel == 0 { return None; }

        let state = self.channel_state();
        if state == STATUS_CONNECTED {
            if !self.connected {
                opaque_log!(info, "[ClearXR Layer] Opaque data channel connected!");
            }
            self.connected = true;
            self.reconnect_after = None;
        } else if state == STATUS_DISCONNECTED {
            if self.connected {
                opaque_log!(info, "[ClearXR Layer] Opaque channel disconnected. Will reconnect.");
                self.connected = false;
                self.latest = None;
            }
            self.try_reconnect();
        }

        if !self.connected { return None; }

        // Drain pending data
        loop {
            let mut received: u32 = 0;
            let r = (self.fn_receive)(self.channel, self.recv_buf.len() as u32,
                                       &mut received, self.recv_buf.as_mut_ptr());
            if r != xr::Result::SUCCESS || received == 0 { break; }

            let pkt_size = std::mem::size_of::<SpatialControllerPacket>();
            if received as usize >= pkt_size {
                let pkt: SpatialControllerPacket =
                    std::ptr::read_unaligned(self.recv_buf.as_ptr() as *const _);
                if pkt.magic == PACKET_MAGIC && pkt.version == 1 {
                    self.latest = Some(pkt);
                }
            }
        }
        self.latest
    }

    
    /// Send a haptic event over the opaque channel to the headset.
    /// `hand`: 0 = left, 1 = right.
    pub fn send_haptic(&mut self, hand: u8, duration_ns: u64, frequency: f32, amplitude: f32) -> bool {
        if !self.connected || self.channel == 0 {
            return false;
        }

        let pkt = HapticEventPacket {
            magic: HAPTIC_MAGIC,
            version: 1,
            hand,
            duration_ns,
            frequency,
            amplitude,
        };

        let bytes = unsafe {
            std::slice::from_raw_parts(
                &pkt as *const HapticEventPacket as *const u8,
                std::mem::size_of::<HapticEventPacket>(),
            )
        };

        let result = unsafe {
            (self.fn_send)(self.channel, bytes.len() as u32, bytes.as_ptr())
        };

        if result != xr::Result::SUCCESS {
            opaque_log!(warn, "[ClearXR Layer] Haptic send failed: {:?}", result);
            return false;
        }
        opaque_log!(
            info,
            "[ClearXR Layer] Forwarded haptic packet: hand={} duration_ns={} frequency={} amplitude={}",
            hand,
            duration_ns,
            frequency,
            amplitude
        );
        true
    }


    unsafe fn try_reconnect(&mut self) {
        let now = std::time::Instant::now();
        if let Some(after) = self.reconnect_after {
            if now < after { return; }
        }
        self.reconnect_after = Some(now + std::time::Duration::from_secs(2));
        if self.channel != 0 {
            (self.fn_shutdown)(self.channel);
            (self.fn_destroy)(self.channel);
            self.channel = 0;
        }
        if self.create_channel() {
            opaque_log!(info, "[ClearXR Layer] Opaque channel recreated, waiting for connection...");
        }
    }

    unsafe fn channel_state(&self) -> i32 {
        let mut s = StateNV { ty: XR_TYPE_STATE, next: std::ptr::null_mut(), state: -1 };
        (self.fn_get_state)(self.channel, &mut s);
        s.state
    }
}


impl Drop for OpaqueChannel {
    fn drop(&mut self) {
        if self.channel != 0 {
            unsafe {
                (self.fn_shutdown)(self.channel);
                (self.fn_destroy)(self.channel);
            }
        }
    }
}

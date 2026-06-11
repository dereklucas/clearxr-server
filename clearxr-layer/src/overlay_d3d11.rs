//! D3D11 dashboard overlay — reads dashboard pixels from SHM (written by the
//! dashboard process after each render), uploads via UpdateSubresource to an
//! OpenXR D3D11 swapchain image each frame, and appends a quad layer.
//!
//! Mirrors the structure of overlay.rs but uses D3D11 instead of Vulkan.
//! No GPU texture sharing required — works regardless of Vulkan interop support.

use crate::d3d11_backend::D3D11Backend;
use crate::overlay::{
    connect_pipe, create_stage_space, cross, quat_rotate, ray_quad_hit, sub, length,
    DashboardInputPacket, HandStatePkt, ShmHeader, SHM_NAME,
    PIXEL_DATA_OFFSET,
};
use crate::opaque::SpatialControllerPacket;
use crate::NextDispatch;
use openxr_sys as xr;
use shared_memory::{Shmem, ShmemConf};
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::Ordering;

/// XrSwapchainImageD3D11KHR layout (from OpenXR spec).
#[repr(C)]
struct SwapchainImageD3D11KHR {
    ty: xr::StructureType,
    next: *mut c_void,
    texture: *mut c_void, // ID3D11Texture2D*
}

// DXGI_FORMAT_R8G8B8A8_UNORM_SRGB = 29
const DXGI_FORMAT_R8G8B8A8_UNORM_SRGB: i64 = 29;
const DXGI_FORMAT_B8G8R8A8_UNORM_SRGB: i64 = 91;
const DXGI_FORMAT_R8G8B8A8_UNORM: i64 = 28;
const DXGI_FORMAT_B8G8R8A8_UNORM: i64 = 87;

// Safety: DashboardOverlayD3D11 is only accessed from the thread that calls xrEndFrame.
unsafe impl Send for DashboardOverlayD3D11 {}

pub struct DashboardOverlayD3D11 {
    session: xr::Session,
    swapchain: xr::Swapchain,
    space: xr::Space,
    width: u32,
    height: u32,
    images: Vec<*mut c_void>, // ID3D11Texture2D* per swapchain image
    // Backface: 1x1 grey swapchain
    backface_swapchain: xr::Swapchain,
    backface_image: *mut c_void, // ID3D11Texture2D*
    backface_initialized: bool,
    d3d11: D3D11Backend,
    last_frame_counter: u32,
    has_rendered: bool,
    shm_layout_warned: bool,
    // SHM reader
    shmem: Option<Shmem>,
    // Pipe client for controller input
    #[cfg(target_os = "windows")]
    pipe: Option<windows_sys::Win32::Foundation::HANDLE>,
    // State
    visible: bool,
    menu_was_down: bool,
    pose: xr::Posef,
    size: xr::Extent2Df,
    // Grab/drag state
    grab_hand: Option<usize>,
    prev_grip: [bool; 2],
    grab_initial_yaw: f32,
    grab_initial_pitch: f32,
    grab_initial_distance: f32,
    grab_initial_orient: xr::Quaternionf,
    grab_controller_start_yaw: f32,
    grab_controller_start_pitch: f32,
    grab_controller_start_distance: f32,
    grab_base_width: f32,
    grab_base_height: f32,
}

impl DashboardOverlayD3D11 {
    pub unsafe fn new(
        next: &NextDispatch,
        session: xr::Session,
        device: *mut c_void, // ID3D11Device* from XrGraphicsBindingD3D11KHR
    ) -> Result<Self, String> {
        let d3d11 = D3D11Backend::from_device(device)?;

        // Try to open SHM
        let shmem = match ShmemConf::new().os_id(SHM_NAME).open() {
            Ok(s) => {
                layer_log!(info, "[ClearXR Layer D3D11] SHM opened: {}", SHM_NAME);
                Some(s)
            }
            Err(e) => {
                layer_log!(warn, "[ClearXR Layer D3D11] SHM not available yet: {e}");
                None
            }
        };

        // Read dimensions from SHM or use defaults
        let (width, height) = if let Some(ref s) = shmem {
            let header = &*(s.as_ptr() as *const ShmHeader);
            (header.width, header.height)
        } else {
            (2048, 1280)
        };

        let swapchain_format = pick_d3d11_swapchain_format(next, session)?;
        let swapchain = create_d3d11_swapchain(next, session, swapchain_format, width, height)?;
        let images = enumerate_d3d11_swapchain_images(next, swapchain)?;
        let space = create_stage_space(next, session)?;

        // Backface: 1x1 grey swapchain
        let backface_swapchain = create_d3d11_swapchain(next, session, swapchain_format, 1, 1)?;
        let backface_images = enumerate_d3d11_swapchain_images(next, backface_swapchain)?;
        let backface_image = backface_images[0];

        // Try to connect pipe
        #[cfg(target_os = "windows")]
        let pipe = connect_pipe();

        Ok(Self {
            session,
            swapchain,
            space,
            width,
            height,
            images,
            backface_swapchain,
            backface_image,
            backface_initialized: false,
            d3d11,
            last_frame_counter: 0,
            has_rendered: false,
            shm_layout_warned: false,
            shmem,
            #[cfg(target_os = "windows")]
            pipe,
            visible: true,
            menu_was_down: false,
            pose: xr::Posef {
                orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
                position: xr::Vector3f { x: 0.0, y: 1.5, z: -2.5 },
            },
            size: xr::Extent2Df { width: 1.6, height: 1.0 },
            grab_hand: None,
            prev_grip: [false; 2],
            grab_initial_yaw: 0.0,
            grab_initial_pitch: 0.0,
            grab_initial_distance: 0.0,
            grab_initial_orient: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
            grab_controller_start_yaw: 0.0,
            grab_controller_start_pitch: 0.0,
            grab_controller_start_distance: 0.0,
            grab_base_width: 1.6,
            grab_base_height: 1.0,
        })
    }

    pub fn is_for_session(&self, session: xr::Session) -> bool {
        self.session == session
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn update_menu_button(&mut self, menu_down: bool) -> bool {
        if !menu_down && self.menu_was_down {
            self.visible = !self.visible;
            if let Some(ref shmem) = self.shmem {
                unsafe {
                    let header = &mut *(shmem.as_ptr() as *mut ShmHeader);
                    if self.visible { header.flags |= 1; } else { header.flags &= !1; }
                }
            }
            self.menu_was_down = false;
            return true;
        }
        self.menu_was_down = menu_down;
        false
    }

    /// Compute ray-quad intersection and send input to the dashboard.
    /// This is identical logic to DashboardOverlay::send_controller_input.
    pub fn send_controller_input(&mut self, pkt: &SpatialControllerPacket) {
        const GRIP_THRESHOLD: f32 = 0.7;
        let head = [0.0f32, 1.6, 0.0];

        let panel_center = [self.pose.position.x, self.pose.position.y, self.pose.position.z];
        let q = [self.pose.orientation.x, self.pose.orientation.y, self.pose.orientation.z, self.pose.orientation.w];
        let panel_right = quat_rotate(&q, [1.0, 0.0, 0.0]);
        let panel_up = quat_rotate(&q, [0.0, 1.0, 0.0]);
        let panel_normal = cross(panel_right, panel_up);
        let half_w = self.size.width / 2.0;
        let half_h = self.size.height / 2.0;

        let mut best_hit: Option<(f32, f32, f32)> = None;
        let mut best_trigger = 0.0f32;
        let mut best_grip = 0.0f32;
        let mut best_thumbstick_y = 0.0f32;
        let mut best_hand_idx: usize = 0;

        let hands = [(0x01u8, 0usize, pkt.left), (0x02u8, 1usize, pkt.right)];
        for &(mask, hand_idx, hand) in &hands {
            if pkt.active_hands & mask == 0 { continue; }
            let aim_pos = [hand.pos_x, hand.pos_y, hand.pos_z];
            let aim_rot = [hand.rot_x, hand.rot_y, hand.rot_z, hand.rot_w];
            let aim_dir = quat_rotate(&aim_rot, [0.0, 0.0, -1.0]);

            if let Some((u, v, t)) = ray_quad_hit(
                aim_pos, aim_dir, panel_center, panel_normal, panel_right, panel_up, half_w, half_h,
            ) {
                if best_hit.map_or(true, |(_, _, prev_t)| t < prev_t) {
                    best_hit = Some((u, v, t));
                    best_trigger = hand.trigger;
                    best_grip = hand.grip;
                    best_thumbstick_y = hand.thumbstick_y;
                    best_hand_idx = hand_idx;
                }
            }
        }

        // ── Grab detection and orbital drag ──
        let grip_states = [pkt.left.grip >= GRIP_THRESHOLD, pkt.right.grip >= GRIP_THRESHOLD];

        if let Some(grab_idx) = self.grab_hand {
            let hand = if grab_idx == 0 { pkt.left } else { pkt.right };
            let still_holding = hand.grip > 0.3 || hand.trigger > 0.3;
            let active = if grab_idx == 0 { pkt.active_hands & 0x01 != 0 } else { pkt.active_hands & 0x02 != 0 };

            if still_holding && active {
                let aim_rot = [hand.rot_x, hand.rot_y, hand.rot_z, hand.rot_w];
                let aim_dir = quat_rotate(&aim_rot, [0.0, 0.0, -1.0]);
                let grip_yaw = aim_dir[0].atan2(-aim_dir[2]);
                let grip_pitch = aim_dir[1].asin();

                let dyaw = grip_yaw - self.grab_controller_start_yaw;
                let dpitch = grip_pitch - self.grab_controller_start_pitch;

                let grip_pos = [hand.pos_x, hand.pos_y, hand.pos_z];
                let grip_dist = length(sub(grip_pos, head)).max(0.1);
                let raw_ratio = grip_dist / self.grab_controller_start_distance.max(0.1);
                let amplified = 1.0 + (raw_ratio - 1.0) * 4.0;
                let new_dist = (self.grab_initial_distance * amplified).clamp(0.8, 10.0);

                let new_yaw = self.grab_initial_yaw + dyaw;
                let new_pitch = (self.grab_initial_pitch + dpitch).clamp(-1.2, 1.2);

                let new_center = [
                    head[0] + new_dist * new_pitch.cos() * new_yaw.sin(),
                    head[1] + new_dist * new_pitch.sin(),
                    head[2] - new_dist * new_pitch.cos() * new_yaw.cos(),
                ];

                self.pose.position.x = new_center[0];
                self.pose.position.y = new_center[1];
                self.pose.position.z = new_center[2];

                let half_dyaw = -dyaw / 2.0;
                let dq = xr::Quaternionf { x: 0.0, y: half_dyaw.sin(), z: 0.0, w: half_dyaw.cos() };
                let q0 = self.grab_initial_orient;
                self.pose.orientation = xr::Quaternionf {
                    x: dq.w * q0.x + dq.x * q0.w + dq.y * q0.z - dq.z * q0.y,
                    y: dq.w * q0.y - dq.x * q0.z + dq.y * q0.w + dq.z * q0.x,
                    z: dq.w * q0.z + dq.x * q0.y - dq.y * q0.x + dq.z * q0.w,
                    w: dq.w * q0.w - dq.x * q0.x - dq.y * q0.y - dq.z * q0.z,
                };

                let dist_scale = new_dist / self.grab_initial_distance.max(0.1);
                self.size.width = self.grab_base_width * dist_scale;
                self.size.height = self.grab_base_height * dist_scale;

                if let Some(ref shmem) = self.shmem {
                    unsafe {
                        let header = &mut *(shmem.as_ptr() as *mut ShmHeader);
                        header.panel_pos = [self.pose.position.x, self.pose.position.y, self.pose.position.z];
                        header.panel_orient = [self.pose.orientation.x, self.pose.orientation.y, self.pose.orientation.z, self.pose.orientation.w];
                        header.panel_size = [self.size.width, self.size.height];
                    }
                }
            } else {
                self.grab_hand = None;
            }
        } else if let Some((_u, v, _)) = best_hit {
            let grip_now = grip_states[best_hand_idx];
            let grip_prev = self.prev_grip[best_hand_idx];
            let in_grab_bar = v > 0.92;

            if grip_now && !grip_prev && in_grab_bar {
                self.grab_hand = Some(best_hand_idx);
                let to_panel = sub(panel_center, head);
                let dist = length(to_panel).max(0.5);
                self.grab_initial_distance = dist;
                self.grab_initial_yaw = to_panel[0].atan2(-to_panel[2]);
                self.grab_initial_pitch = (to_panel[1] / dist).asin();
                self.grab_initial_orient = self.pose.orientation;

                let hand = if best_hand_idx == 0 { pkt.left } else { pkt.right };
                let aim_rot = [hand.rot_x, hand.rot_y, hand.rot_z, hand.rot_w];
                let aim_dir = quat_rotate(&aim_rot, [0.0, 0.0, -1.0]);
                self.grab_controller_start_yaw = aim_dir[0].atan2(-aim_dir[2]);
                self.grab_controller_start_pitch = aim_dir[1].asin();
                let grip_pos = [hand.pos_x, hand.pos_y, hand.pos_z];
                self.grab_controller_start_distance = length(sub(grip_pos, head)).max(0.1);
                self.grab_base_width = self.size.width;
                self.grab_base_height = self.size.height;
            }
        }

        self.prev_grip = grip_states;

        let is_grabbing = self.grab_hand.is_some();
        let make_hand = |hand: crate::opaque::SpatialControllerHand, mask: u8| -> HandStatePkt {
            HandStatePkt {
                buttons: hand.buttons,
                active: if pkt.active_hands & mask != 0 { 1 } else { 0 },
                _pad: 0,
                trigger: hand.trigger,
                grip: hand.grip,
                thumbstick_x: hand.thumbstick_x,
                thumbstick_y: hand.thumbstick_y,
                pos_x: hand.pos_x,
                pos_y: hand.pos_y,
                pos_z: hand.pos_z,
            }
        };

        let input_pkt = DashboardInputPacket {
            magic: 0x4449,
            flags: if best_hit.is_some() && !is_grabbing { 0x01 } else { 0x00 },
            _pad: 0,
            pointer_u: best_hit.map_or(0.0, |(u, _, _)| u),
            pointer_v: best_hit.map_or(0.0, |(_, v, _)| v),
            trigger: if is_grabbing { 0.0 } else { best_trigger },
            grip: if is_grabbing { 0.0 } else { best_grip },
            thumbstick_y: best_thumbstick_y,
            left: make_hand(pkt.left, 0x01),
            right: make_hand(pkt.right, 0x02),
        };

        #[cfg(target_os = "windows")]
        {
            if self.pipe.is_none() {
                self.pipe = connect_pipe();
            }
            if let Some(handle) = self.pipe {
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        &input_pkt as *const DashboardInputPacket as *const u8,
                        std::mem::size_of::<DashboardInputPacket>(),
                    )
                };
                let mut written = 0u32;
                let ok = unsafe {
                    windows_sys::Win32::Storage::FileSystem::WriteFile(
                        handle,
                        bytes.as_ptr(),
                        bytes.len() as u32,
                        &mut written,
                        ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle); }
                    self.pipe = None;
                }
            }
        }
    }

    /// Initialize the backface swapchain with a solid grey pixel.
    /// For D3D11, we use UpdateSubresource to fill the 1x1 texture.
    unsafe fn init_backface(&mut self, next: &NextDispatch) -> Result<(), String> {
        let mut idx = 0;
        (next.acquire_swapchain_image)(
            self.backface_swapchain,
            &xr::SwapchainImageAcquireInfo { ty: xr::SwapchainImageAcquireInfo::TYPE, next: ptr::null() },
            &mut idx,
        );
        (next.wait_swapchain_image)(
            self.backface_swapchain,
            &xr::SwapchainImageWaitInfo { ty: xr::SwapchainImageWaitInfo::TYPE, next: ptr::null(), timeout: xr::Duration::INFINITE },
        );

        // Use UpdateSubresource to write a grey pixel (slot 48 in ID3D11DeviceContext vtable)
        // UpdateSubresource(pDstResource, DstSubresource, pDstBox, pSrcData, SrcRowPitch, SrcDepthPitch)
        // For SRGB: 0.15 linear ≈ 0.42 sRGB → byte value ~107
        // For simplicity, write RGBA bytes directly (the swapchain may be SRGB, which
        // means the hardware interprets bytes as sRGB). Write the same values as the
        // Vulkan path's clear color: (0.15, 0.15, 0.18, 0.9) in linear, which in sRGB
        // encoding becomes approx (107, 107, 115, 230) as bytes.
        let pixel: [u8; 4] = [107, 107, 115, 230];
        update_subresource(
            self.d3d11.context_ptr(),
            self.backface_image,
            &pixel as *const u8 as *const c_void,
            4, // row pitch (1 pixel * 4 bytes)
            4, // depth pitch
        );

        (next.release_swapchain_image)(
            self.backface_swapchain,
            &xr::SwapchainImageReleaseInfo { ty: xr::SwapchainImageReleaseInfo::TYPE, next: ptr::null() },
        );

        self.backface_initialized = true;
        layer_log!(info, "[ClearXR Layer D3D11] Backface initialized (dark grey 1x1).");
        Ok(())
    }

    /// Front face (dashboard content).
    pub fn quad_layer(&self) -> xr::CompositionLayerQuad {
        xr::CompositionLayerQuad {
            ty: xr::CompositionLayerQuad::TYPE,
            next: ptr::null(),
            layer_flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
            space: self.space,
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image: xr::SwapchainSubImage {
                swapchain: self.swapchain,
                image_rect: xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di {
                        width: self.width as i32,
                        height: self.height as i32,
                    },
                },
                image_array_index: 0,
            },
            pose: self.pose,
            size: self.size,
        }
    }

    /// Back face — grey card rotated 180deg around Y.
    pub fn backface_quad_layer(&self) -> xr::CompositionLayerQuad {
        // Rotate 180° around Y: q * (0,1,0,0) = (-q.z, q.w, q.x, -q.y)
        let q = self.pose.orientation;
        let back_orient = xr::Quaternionf {
            x: -q.z,
            y: q.w,
            z: q.x,
            w: -q.y,
        };

        xr::CompositionLayerQuad {
            ty: xr::CompositionLayerQuad::TYPE,
            next: ptr::null(),
            layer_flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
            space: self.space,
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image: xr::SwapchainSubImage {
                swapchain: self.backface_swapchain,
                image_rect: xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di { width: 1, height: 1 },
                },
                image_array_index: 0,
            },
            pose: xr::Posef {
                orientation: back_orient,
                position: self.pose.position,
            },
            size: self.size,
        }
    }

    /// Returns Ok(true) if the swapchain has valid content (safe to submit as a quad layer),
    /// Ok(false) if no content yet (do NOT submit quad layers), or Err on failure.
    pub unsafe fn render_frame(&mut self, next: &NextDispatch) -> Result<bool, String> {
        // Try to open SHM if not connected yet (needed for pose data AND pixel upload)
        if self.shmem.is_none() {
            if let Ok(s) = ShmemConf::new().os_id(SHM_NAME).open() {
                layer_log!(info, "[ClearXR Layer D3D11] SHM connected.");
                self.shmem = Some(s);
            } else {
                return Ok(self.has_rendered);
            }
        }

        // Extract everything we need from SHM up front to avoid borrow conflicts.
        let (pose_pos, pose_orient, panel_size, frame_counter, shmem_size, pixel_ptr_raw) = {
            let shmem = self.shmem.as_ref().unwrap();
            let header = &*(shmem.as_ptr() as *const ShmHeader);
            let fc = header.frame_counter.load(Ordering::Acquire);
            // Tell the dashboard a D3D11 consumer is live so it keeps running the
            // CPU readback (the Vulkan path imports the image directly and never
            // bumps this, letting the dashboard skip readback for Vulkan-only games).
            header.consumer_heartbeat.fetch_add(1, Ordering::Relaxed);
            let sz = shmem.len();
            let px = shmem.as_ptr().add(PIXEL_DATA_OFFSET);
            (
                [header.panel_pos[0], header.panel_pos[1], header.panel_pos[2]],
                [header.panel_orient[0], header.panel_orient[1], header.panel_orient[2], header.panel_orient[3]],
                [header.panel_size[0], header.panel_size[1]],
                fc, sz, px,
            )
        };

        self.pose.position.x = pose_pos[0];
        self.pose.position.y = pose_pos[1];
        self.pose.position.z = pose_pos[2];
        self.pose.orientation.x = pose_orient[0];
        self.pose.orientation.y = pose_orient[1];
        self.pose.orientation.z = pose_orient[2];
        self.pose.orientation.w = pose_orient[3];
        self.size.width = panel_size[0];
        self.size.height = panel_size[1];

        if !self.visible {
            return Ok(self.has_rendered);
        }

        // Initialize backface grey card (once)
        if !self.backface_initialized {
            if let Err(e) = self.init_backface(next) {
                layer_log!(warn, "[ClearXR Layer D3D11] Backface init failed: {e}");
            }
        }

        // Check frame counter — dashboard bumps this after writing new pixels to SHM.
        if frame_counter == self.last_frame_counter {
            return Ok(self.has_rendered);
        }
        // frame_counter==0 means dashboard hasn't written pixels yet.
        if frame_counter == 0 {
            return Ok(false);
        }

        // Acquire swapchain image
        let mut image_index = 0;
        let r = (next.acquire_swapchain_image)(
            self.swapchain,
            &xr::SwapchainImageAcquireInfo { ty: xr::SwapchainImageAcquireInfo::TYPE, next: ptr::null() },
            &mut image_index,
        );
        if r != xr::Result::SUCCESS {
            return Err(format!("AcquireSwapchainImage: {:?}", r));
        }
        let r = (next.wait_swapchain_image)(
            self.swapchain,
            &xr::SwapchainImageWaitInfo { ty: xr::SwapchainImageWaitInfo::TYPE, next: ptr::null(), timeout: xr::Duration::INFINITE },
        );
        if r != xr::Result::SUCCESS {
            return Err(format!("WaitSwapchainImage: {:?}", r));
        }

        let dst = self.images[image_index as usize];

        let pixel_bytes = (self.width * self.height * 4) as usize;
        // Double-buffered: the region holds two slots after the header. Require
        // both. Slot math must match the writer in clearxr-dashboard/src/shm.rs.
        if shmem_size < PIXEL_DATA_OFFSET + 2 * pixel_bytes {
            // Region is too small to hold pixels (stale mapping from an older
            // run/build). This is the classic "dashboard silently absent" cause,
            // so say it out loud exactly once.
            if !self.shm_layout_warned {
                self.shm_layout_warned = true;
                layer_log!(
                    warn,
                    "[ClearXR Layer D3D11] SHM region is {} bytes but {} are required; \
                     stale mapping from another ClearXR instance? Restart the streamer.",
                    shmem_size, PIXEL_DATA_OFFSET + 2 * pixel_bytes
                );
            }
            // Still release the acquired image — returning while holding it
            // leaks one acquire per frame.
            let r = (next.release_swapchain_image)(
                self.swapchain,
                &xr::SwapchainImageReleaseInfo { ty: xr::SwapchainImageReleaseInfo::TYPE, next: ptr::null() },
            );
            if r != xr::Result::SUCCESS {
                return Err(format!("ReleaseSwapchainImage: {:?}", r));
            }
            return Ok(self.has_rendered);
        }
        // Read the slot the writer just published. It writes the *other* slot
        // for the next frame, so this copy can't tear. slot_cap must match the
        // writer's (clearxr-dashboard/src/shm.rs write_pixels).
        let slot_cap = (shmem_size - PIXEL_DATA_OFFSET) / 2;
        let slot = (frame_counter & 1) as usize;
        let pixel_ptr = pixel_ptr_raw.add(slot * slot_cap) as *const c_void;
        let row_pitch = self.width * 4;
        update_subresource(self.d3d11.context_ptr(), dst, pixel_ptr, row_pitch, 0);

        self.last_frame_counter = frame_counter;

        // Release swapchain image
        let r = (next.release_swapchain_image)(
            self.swapchain,
            &xr::SwapchainImageReleaseInfo { ty: xr::SwapchainImageReleaseInfo::TYPE, next: ptr::null() },
        );
        if r != xr::Result::SUCCESS {
            return Err(format!("ReleaseSwapchainImage: {:?}", r));
        }

        self.has_rendered = true;
        Ok(true)
    }
}

impl Drop for DashboardOverlayD3D11 {
    fn drop(&mut self) {
        // D3D11Backend drop handles device/context Release.
        // OpenXR resources:
        unsafe {
            if let Some(next) = crate::NEXT.get() {
                if self.space != xr::Space::NULL {
                    (next.destroy_space)(self.space);
                }
                if self.swapchain != xr::Swapchain::NULL {
                    (next.destroy_swapchain)(self.swapchain);
                }
                if self.backface_swapchain != xr::Swapchain::NULL {
                    (next.destroy_swapchain)(self.backface_swapchain);
                }
            }

            #[cfg(target_os = "windows")]
            if let Some(h) = self.pipe {
                windows_sys::Win32::Foundation::CloseHandle(h);
            }
        }
        layer_log!(info, "[ClearXR Layer D3D11] DashboardOverlayD3D11 destroyed.");
    }
}

// ============================================================
// D3D11 swapchain helpers
// ============================================================

unsafe fn pick_d3d11_swapchain_format(
    next: &NextDispatch,
    session: xr::Session,
) -> Result<i64, String> {
    let mut count = 0;
    (next.enumerate_swapchain_formats)(session, 0, &mut count, ptr::null_mut());
    let mut formats = vec![0i64; count as usize];
    (next.enumerate_swapchain_formats)(session, count, &mut count, formats.as_mut_ptr());

    // Prefer SRGB formats to match dashboard's sRGB output
    let preferred = [
        DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_B8G8R8A8_UNORM,
    ];
    Ok(preferred.iter().copied()
        .find(|f| formats.contains(f))
        .unwrap_or(formats[0]))
}

unsafe fn create_d3d11_swapchain(
    next: &NextDispatch,
    session: xr::Session,
    format: i64,
    width: u32,
    height: u32,
) -> Result<xr::Swapchain, String> {
    let ci = xr::SwapchainCreateInfo {
        ty: xr::SwapchainCreateInfo::TYPE,
        next: ptr::null(),
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::TRANSFER_DST,
        format,
        sample_count: 1,
        width,
        height,
        face_count: 1,
        array_size: 1,
        mip_count: 1,
    };
    let mut swapchain = xr::Swapchain::NULL;
    let r = (next.create_swapchain)(session, &ci, &mut swapchain);
    if r != xr::Result::SUCCESS {
        return Err(format!("CreateSwapchain(D3D11): {:?}", r));
    }
    Ok(swapchain)
}

unsafe fn enumerate_d3d11_swapchain_images(
    next: &NextDispatch,
    swapchain: xr::Swapchain,
) -> Result<Vec<*mut c_void>, String> {
    let mut count = 0;
    (next.enumerate_swapchain_images)(swapchain, 0, &mut count, ptr::null_mut());
    // Allocate with correct type tag
    let mut images: Vec<SwapchainImageD3D11KHR> = (0..count)
        .map(|_| SwapchainImageD3D11KHR {
            ty: xr::StructureType::SWAPCHAIN_IMAGE_D3D11_KHR,
            next: ptr::null_mut(),
            texture: ptr::null_mut(),
        })
        .collect();
    let r = (next.enumerate_swapchain_images)(
        swapchain,
        count,
        &mut count,
        images.as_mut_ptr() as *mut xr::SwapchainImageBaseHeader,
    );
    if r != xr::Result::SUCCESS {
        return Err(format!("EnumerateSwapchainImages(D3D11): {:?}", r));
    }
    Ok(images.iter().map(|img| img.texture).collect())
}

/// ID3D11DeviceContext::UpdateSubresource — vtable slot 48
unsafe fn update_subresource(
    context: *mut c_void,
    dst_resource: *mut c_void,
    src_data: *const c_void,
    src_row_pitch: u32,
    src_depth_pitch: u32,
) {
    let vtable = *(context as *const *const *const c_void);
    let update_fn: unsafe extern "system" fn(
        *mut c_void,       // this
        *mut c_void,       // pDstResource
        u32,               // DstSubresource
        *const c_void,     // pDstBox (null = entire resource)
        *const c_void,     // pSrcData
        u32,               // SrcRowPitch
        u32,               // SrcDepthPitch
    ) = std::mem::transmute(*vtable.add(48));

    update_fn(context, dst_resource, 0, ptr::null(), src_data, src_row_pitch, src_depth_pitch);
}

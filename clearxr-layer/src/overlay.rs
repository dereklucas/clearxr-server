//! Thin dashboard overlay — imports shared Vulkan image from the dashboard
//! process, copies to a swapchain image, and appends a quad composition layer.
//! No egui, no rendering, no background threads.

use crate::{opaque::SpatialControllerPacket, vk_backend::VkBackend, NextDispatch};
use ash::{vk, vk::Handle};
use openxr_sys as xr;
use shared_memory::{Shmem, ShmemConf};
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

const SHM_NAME: &str = "ClearXR_Dashboard_Meta";
const PIPE_NAME: &str = r"\\.\pipe\ClearXR_Controller_Input";

// Named Win32 handles for cross-process sharing (UTF-16 with null terminator).
// Must match clearxr-dashboard/src/renderer.rs constants.
pub(crate) const IMAGE_HANDLE_NAME: &[u16] = &[
    b'C' as u16, b'l' as u16, b'e' as u16, b'a' as u16, b'r' as u16,
    b'X' as u16, b'R' as u16, b'_' as u16, b'D' as u16, b'a' as u16,
    b's' as u16, b'h' as u16, b'b' as u16, b'o' as u16, b'a' as u16,
    b'r' as u16, b'd' as u16, b'I' as u16, b'm' as u16, b'a' as u16,
    b'g' as u16, b'e' as u16, 0,
];

/// Minimal SHM header v2 (must match clearxr-dashboard/src/shm.rs).
/// Metadata only — no pixel data. Pixels shared via VK_KHR_external_memory_win32.
#[repr(C)]
pub(crate) struct ShmHeader {
    frame_counter: AtomicU32,     // 0
    width: u32,                   // 4
    height: u32,                  // 8
    flags: u32,                   // 12
    panel_pos: [f32; 3],          // 16
    panel_orient: [f32; 4],       // 28
    panel_size: [f32; 2],         // 44
    gpu_luid: [u8; 8],           // 52
    _reserved: [u8; 4],          // 60 -> total 64
}

// Safety: DashboardOverlay is only accessed from the thread that calls xrEndFrame.
// The raw pointers (pipe handle) are not shared.
unsafe impl Send for DashboardOverlay {}

pub struct DashboardOverlay {
    session: xr::Session,
    swapchain: xr::Swapchain,
    space: xr::Space,
    width: u32,
    height: u32,
    images: Vec<vk::Image>,
    image_layouts: Vec<vk::ImageLayout>,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    vk: VkBackend,
    // Shared Vulkan resources (imported from dashboard process)
    shared_image: vk::Image,
    shared_image_memory: vk::DeviceMemory,
    last_frame_counter: u32,
    shared_resources_imported: bool,
    // SHM reader
    shmem: Option<Shmem>,
    // Pipe client for controller input
    #[cfg(target_os = "windows")]
    pipe: Option<windows_sys::Win32::Foundation::HANDLE>,
    // State
    visible: bool,
    menu_was_down: bool,
    last_menu_toggle: std::time::Instant,
    pose: xr::Posef,
    size: xr::Extent2Df,
    // Grab/drag state
    grab_hand: Option<usize>,  // 0=left, 1=right; None=not grabbing
    prev_grip: [bool; 2],
    grab_initial_yaw: f32,
    grab_initial_pitch: f32,
    grab_initial_distance: f32,
    grab_initial_orient: xr::Quaternionf, // orientation at grab start
    grab_controller_start_yaw: f32,
    grab_controller_start_pitch: f32,
    grab_controller_start_distance: f32,
    grab_base_width: f32,
    grab_base_height: f32,
}

impl DashboardOverlay {
    pub unsafe fn new(
        next: &NextDispatch,
        session: xr::Session,
        binding: &xr::GraphicsBindingVulkanKHR,
    ) -> Result<Self, String> {
        let vk = VkBackend::from_graphics_binding(binding)
            .map_err(|e| format!("VkBackend failed: {e}"))?;

        // Try to open SHM (dashboard process may not have created it yet)
        let shmem = match ShmemConf::new().os_id(SHM_NAME).open() {
            Ok(s) => {
                log::info!("[ClearXR Layer] SHM opened: {}", SHM_NAME);
                Some(s)
            }
            Err(e) => {
                log::warn!("[ClearXR Layer] SHM not available yet: {e}");
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

        let format = pick_swapchain_format(next, session)?;
        let swapchain = create_swapchain(next, session, format, width, height)?;
        let images = enumerate_swapchain_images(next, swapchain)?
            .into_iter()
            .map(|img| vk::Image::from_raw(img.image as usize as u64))
            .collect::<Vec<_>>();
        let space = create_stage_space(next, session)?;

        // Command buffer + fence
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(vk.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command_buffer = vk.device().allocate_command_buffers(&alloc_info)
            .map_err(|e| format!("alloc cmd buf: {e}"))?[0];
        let fence = vk.device().create_fence(
            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
            None,
        ).map_err(|e| format!("create fence: {e}"))?;

        // Try to connect pipe
        #[cfg(target_os = "windows")]
        let pipe = connect_pipe();

        let image_count = images.len();
        Ok(Self {
            session,
            swapchain,
            space,
            width,
            height,
            images,
            image_layouts: vec![vk::ImageLayout::UNDEFINED; image_count],
            command_buffer,
            fence,
            vk,
            shared_image: vk::Image::null(),
            shared_image_memory: vk::DeviceMemory::null(),
            last_frame_counter: 0,
            shared_resources_imported: false,
            shmem,
            #[cfg(target_os = "windows")]
            pipe,
            visible: true,
            menu_was_down: false,
            last_menu_toggle: std::time::Instant::now(),
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

    /// Import the dashboard's shared Vulkan image via named Win32 handle.
    /// Called once when SHM is first connected.
    unsafe fn import_shared_resources(&mut self) -> Result<(), String> {
        let device = self.vk.device();

        // ── Import shared image ──
        // Create VkImage with ExternalMemoryImageCreateInfo (must match dashboard's creation params)
        let mut external_image_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
        let image_ci = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width: self.width, height: self.height, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC)
            .push_next(&mut external_image_info);
        self.shared_image = device.create_image(&image_ci, None)
            .map_err(|e| format!("create shared image: {e}"))?;

        // Query memory requirements for the imported image
        let mem_reqs = device.get_image_memory_requirements(self.shared_image);
        let mem_type = self.vk.find_memory_type(
            mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ).ok_or("No DEVICE_LOCAL memory type for shared image")?;

        // Import memory via named Win32 handle (null handle + name = name-based import)
        let mut import_win32_info = vk::ImportMemoryWin32HandleInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32)
            .handle(vk::HANDLE::default())
            .name(IMAGE_HANDLE_NAME.as_ptr());
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default()
            .image(self.shared_image);
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type)
            .push_next(&mut dedicated_info)
            .push_next(&mut import_win32_info);
        self.shared_image_memory = device.allocate_memory(&alloc_info, None)
            .map_err(|e| format!("import shared image memory: {e}"))?;
        device.bind_image_memory(self.shared_image, self.shared_image_memory, 0)
            .map_err(|e| format!("bind shared image memory: {e}"))?;

        log::info!(
            "[ClearXR Layer] Shared image imported: {}x{}, mem_type={}",
            self.width, self.height, mem_type
        );

        self.shared_resources_imported = true;
        Ok(())
    }

    pub fn is_for_session(&self, session: xr::Session) -> bool {
        self.session == session
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn update_menu_button(&mut self, menu_down: bool) -> bool {
        if menu_down && !self.menu_was_down {
            // Debounce: the opaque channel delivers menu as momentary pulses,
            // causing rapid re-triggers. Ignore toggles within 300ms.
            let now = std::time::Instant::now();
            if now.duration_since(self.last_menu_toggle).as_millis() < 300 {
                self.menu_was_down = menu_down;
                return false;
            }
            self.last_menu_toggle = now;
            self.menu_was_down = menu_down;
            self.visible = !self.visible;
            // Write visibility to SHM so dashboard can also read it
            if let Some(ref shmem) = self.shmem {
                unsafe {
                    let header = &mut *(shmem.as_ptr() as *mut ShmHeader);
                    if self.visible { header.flags |= 1; } else { header.flags &= !1; }
                }
            }
            return true;
        }
        self.menu_was_down = menu_down;
        false
    }

    /// Compute ray-quad intersection and send pre-computed UV + buttons to the dashboard.
    ///
    /// The layer does all spatial math here so the dashboard is a pure
    /// "mouse events in, pixels out" module with no 3D math.
    pub fn send_controller_input(&mut self, pkt: &SpatialControllerPacket) {
        static PIPE_LOG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let pipe_count = PIPE_LOG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        const GRIP_THRESHOLD: f32 = 0.7;
        const GRAB_MARGIN: f32 = 0.15;
        let head = [0.0f32, 1.6, 0.0]; // fixed head position (orbit center)

        // Compute ray-quad intersection against the dashboard panel
        let panel_center = [self.pose.position.x, self.pose.position.y, self.pose.position.z];
        let q = [self.pose.orientation.x, self.pose.orientation.y, self.pose.orientation.z, self.pose.orientation.w];
        let panel_right = quat_rotate(&q, [1.0, 0.0, 0.0]);
        let panel_up = quat_rotate(&q, [0.0, 1.0, 0.0]);
        let panel_normal = cross(panel_right, panel_up);
        let half_w = self.size.width / 2.0;
        let half_h = self.size.height / 2.0;

        // Find best hit across both hands, track per-hand data for grab
        let mut best_hit: Option<(f32, f32, f32)> = None;
        let mut best_trigger = 0.0f32;
        let mut best_grip = 0.0f32;
        let mut best_thumbstick_y = 0.0f32;
        let mut best_hand_idx: usize = 0;

        let hands = [(0x01u8, 0usize, pkt.left), (0x02u8, 1usize, pkt.right)];
        for &(mask, hand_idx, hand) in &hands {
            if pkt.active_hands & mask == 0 {
                continue;
            }
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
            // Currently grabbing — update or release
            let hand = if grab_idx == 0 { pkt.left } else { pkt.right };
            let still_holding = hand.grip > 0.3 || hand.trigger > 0.3;
            let active = if grab_idx == 0 { pkt.active_hands & 0x01 != 0 } else { pkt.active_hands & 0x02 != 0 };

            if still_holding && active {
                // Orbital drag: compute new panel position from controller aim direction
                let aim_rot = [hand.rot_x, hand.rot_y, hand.rot_z, hand.rot_w];
                let aim_dir = quat_rotate(&aim_rot, [0.0, 0.0, -1.0]);
                let grip_yaw = aim_dir[0].atan2(-aim_dir[2]);
                let grip_pitch = aim_dir[1].asin();

                let dyaw = grip_yaw - self.grab_controller_start_yaw;
                let dpitch = grip_pitch - self.grab_controller_start_pitch;

                // Distance scaling (8x sensitivity, clamped)
                let grip_pos = [hand.pos_x, hand.pos_y, hand.pos_z];
                let grip_dist = length(sub(grip_pos, head)).max(0.1);
                let raw_ratio = grip_dist / self.grab_controller_start_distance.max(0.1);
                let amplified = 1.0 + (raw_ratio - 1.0) * 8.0;
                let new_dist = (self.grab_initial_distance * amplified).clamp(0.8, 10.0);

                let new_yaw = self.grab_initial_yaw + dyaw;
                let new_pitch = self.grab_initial_pitch + dpitch;

                // Spherical → Cartesian
                let new_center = [
                    head[0] + new_dist * new_pitch.cos() * new_yaw.sin(),
                    head[1] + new_dist * new_pitch.sin(),
                    head[2] - new_dist * new_pitch.cos() * new_yaw.cos(),
                ];

                // Update panel position
                self.pose.position.x = new_center[0];
                self.pose.position.y = new_center[1];
                self.pose.position.z = new_center[2];

                // Apply yaw delta to the orientation saved at grab start.
                // This avoids computing orientation from scratch (which has
                // sign convention issues). The panel rotates by exactly the
                // same amount the controller yaw changed.
                let half_dyaw = dyaw / 2.0;
                let dq = xr::Quaternionf { x: 0.0, y: half_dyaw.sin(), z: 0.0, w: half_dyaw.cos() };
                let q0 = self.grab_initial_orient;
                // Quaternion multiply: dq * q0 (apply delta in world space)
                self.pose.orientation = xr::Quaternionf {
                    x: dq.w * q0.x + dq.x * q0.w + dq.y * q0.z - dq.z * q0.y,
                    y: dq.w * q0.y - dq.x * q0.z + dq.y * q0.w + dq.z * q0.x,
                    z: dq.w * q0.z + dq.x * q0.y - dq.y * q0.x + dq.z * q0.w,
                    w: dq.w * q0.w - dq.x * q0.x - dq.y * q0.y - dq.z * q0.z,
                };

                // Scale size proportionally with distance
                let dist_scale = new_dist / self.grab_initial_distance.max(0.1);
                self.size.width = self.grab_base_width * dist_scale;
                self.size.height = self.grab_base_height * dist_scale;

                // Write updated pose to SHM
                if let Some(ref shmem) = self.shmem {
                    unsafe {
                        let header = &mut *(shmem.as_ptr() as *mut ShmHeader);
                        header.panel_pos = [self.pose.position.x, self.pose.position.y, self.pose.position.z];
                        header.panel_orient = [self.pose.orientation.x, self.pose.orientation.y, self.pose.orientation.z, self.pose.orientation.w];
                        header.panel_size = [self.size.width, self.size.height];
                    }
                }
            } else {
                // Release grab
                self.grab_hand = None;
            }
        } else if let Some((u, v, _)) = best_hit {
            // Not grabbing — check for grab start (grab bar at bottom of panel only)
            let grip_now = grip_states[best_hand_idx];
            let grip_prev = self.prev_grip[best_hand_idx];
            let in_grab_bar = v > 0.92; // bottom ~8% of panel = visual grab bar

            if grip_now && !grip_prev && in_grab_bar {
                // Start grab — record initial state
                self.grab_hand = Some(best_hand_idx);
                let to_panel = sub(panel_center, head);
                let dist = length(to_panel).max(0.5);
                self.grab_initial_distance = dist;
                self.grab_initial_yaw = to_panel[0].atan2(-to_panel[2]);
                self.grab_initial_pitch = (to_panel[1] / dist).asin();
                self.grab_initial_orient = self.pose.orientation; // preserve current orientation

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

        // Build packet for dashboard — don't send pointer during grab (prevents phantom clicks)
        let is_grabbing = self.grab_hand.is_some();
        let input_pkt = DashboardInputPacket {
            magic: 0x4449,
            flags: if best_hit.is_some() && !is_grabbing { 0x01 } else { 0x00 },
            _pad: 0,
            pointer_u: best_hit.map_or(0.0, |(u, _, _)| u),
            pointer_v: best_hit.map_or(0.0, |(_, v, _)| v),
            trigger: if is_grabbing { 0.0 } else { best_trigger },
            grip: if is_grabbing { 0.0 } else { best_grip },
            thumbstick_y: best_thumbstick_y,
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
                if pipe_count < 5 || pipe_count % 360 == 0 {
                    layer_log!(info,
                        "[ClearXR Layer] Pipe write: ok={} written={}/{} uv={:?} grab={}",
                        ok, written, bytes.len(),
                        best_hit.map(|(u, v, _)| (u, v)),
                        is_grabbing,
                    );
                }
                if ok == 0 {
                    layer_log!(info, "[ClearXR Layer] Pipe write FAILED, reconnecting.");
                    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle); }
                    self.pipe = None;
                }
            }
        }
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

    /// Back face — rotated 180° around Y so it's visible from behind.
    /// Shows the same swapchain content (mirrored) so you can find the panel.
    pub fn backface_quad_layer(&self) -> xr::CompositionLayerQuad {
        // For yaw-only quaternion (0, sin(θ/2), 0, cos(θ/2)):
        // Adding π to θ: sin((θ+π)/2) = cos(θ/2), cos((θ+π)/2) = -sin(θ/2)
        let q = self.pose.orientation;
        let back_orient = xr::Quaternionf {
            x: 0.0,
            y: q.w,    // cos(θ/2) → sin((θ+π)/2)
            z: 0.0,
            w: -q.y,   // sin(θ/2) → -cos((θ+π)/2) → but cos = -sin, so w = -sin(θ/2)
        };

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
            pose: xr::Posef {
                orientation: back_orient,
                position: self.pose.position,
            },
            size: self.size,
        }
    }

    pub unsafe fn render_frame(&mut self, next: &NextDispatch) -> Result<(), String> {
        // Try to open SHM if not connected yet
        if self.shmem.is_none() {
            if let Ok(s) = ShmemConf::new().os_id(SHM_NAME).open() {
                log::info!("[ClearXR Layer] SHM connected.");
                self.shmem = Some(s);
            } else {
                return Ok(()); // Dashboard not running yet
            }
        }

        let shmem = self.shmem.as_ref().unwrap();
        let header = &*(shmem.as_ptr() as *const ShmHeader);

        // Read panel pose from SHM header (dashboard writes these).
        // NOTE: visibility is NOT read from SHM — the layer is sole authority
        // via update_menu_button(). Reading it back caused a fight where the
        // dashboard's flag overwrite defeated the layer's toggle.
        self.pose.position.x = header.panel_pos[0];
        self.pose.position.y = header.panel_pos[1];
        self.pose.position.z = header.panel_pos[2];
        self.pose.orientation.x = header.panel_orient[0];
        self.pose.orientation.y = header.panel_orient[1];
        self.pose.orientation.z = header.panel_orient[2];
        self.pose.orientation.w = header.panel_orient[3];
        self.size.width = header.panel_size[0];
        self.size.height = header.panel_size[1];

        if !self.visible {
            return Ok(());
        }

        // Import shared Vulkan resources on first connected frame
        if !self.shared_resources_imported {
            if let Err(e) = self.import_shared_resources() {
                log::error!("[ClearXR Layer] Failed to import shared resources: {e}");
                return Ok(()); // Don't render until import succeeds
            }
        }

        // Check frame counter for new frames (dashboard bumps this after its GPU
        // fence signals, so a new value means the shared image is stable).
        let frame_counter = header.frame_counter.load(Ordering::Acquire);
        if frame_counter == self.last_frame_counter {
            return Ok(()); // No new frame from dashboard
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

        let idx = image_index as usize;
        let swapchain_image = self.images[idx];
        let old_layout = self.image_layouts[idx];

        // Record command buffer: copy shared image → swapchain image
        let device = self.vk.device();
        device.wait_for_fences(&[self.fence], true, u64::MAX)
            .map_err(|e| format!("wait fence: {e}"))?;
        device.reset_fences(&[self.fence])
            .map_err(|e| format!("reset fence: {e}"))?;

        let cmd = self.command_buffer;
        device.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))
            .map_err(|e| format!("begin cmd: {e}"))?;

        // Barrier: shared image ownership acquire (GENERAL → TRANSFER_SRC)
        // The dashboard leaves the image in GENERAL layout after rendering.
        let shared_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .image(self.shared_image)
            .subresource_range(vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1).layer_count(1))
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ);

        // Barrier: swapchain image → TRANSFER_DST
        let swapchain_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(old_layout)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .image(swapchain_image)
            .subresource_range(vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1).layer_count(1))
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);

        device.cmd_pipeline_barrier(cmd,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(), &[], &[],
            &[shared_barrier, swapchain_barrier]);

        // Copy shared image → swapchain image
        let region = vk::ImageCopy::default()
            .src_subresource(vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1))
            .dst_subresource(vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1))
            .extent(vk::Extent3D { width: self.width, height: self.height, depth: 1 });
        device.cmd_copy_image(cmd,
            self.shared_image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            swapchain_image, vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region]);

        // Barrier: swapchain image → COLOR_ATTACHMENT_OPTIMAL (compositor expects this)
        let final_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .image(swapchain_image)
            .subresource_range(vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1).layer_count(1))
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_READ);
        device.cmd_pipeline_barrier(cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::DependencyFlags::empty(), &[], &[], &[final_barrier]);

        device.end_command_buffer(cmd)
            .map_err(|e| format!("end cmd: {e}"))?;

        // Submit — no semaphore wait needed; the dashboard waits for its own GPU
        // fence before bumping frame_counter, so the shared image is already stable.
        let submit = vk::SubmitInfo::default()
            .command_buffers(std::slice::from_ref(&cmd));
        device.queue_submit(self.vk.queue(), &[submit], self.fence)
            .map_err(|e| format!("queue submit: {e}"))?;

        self.last_frame_counter = frame_counter;
        self.image_layouts[idx] = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;

        // Release swapchain image
        let r = (next.release_swapchain_image)(
            self.swapchain,
            &xr::SwapchainImageReleaseInfo { ty: xr::SwapchainImageReleaseInfo::TYPE, next: ptr::null() },
        );
        if r != xr::Result::SUCCESS {
            return Err(format!("ReleaseSwapchainImage: {:?}", r));
        }

        Ok(())
    }

}

impl Drop for DashboardOverlay {
    fn drop(&mut self) {
        unsafe {
            let device = self.vk.device();
            device.device_wait_idle().ok();

            // Shared Vulkan resources
            if self.shared_image != vk::Image::null() {
                device.destroy_image(self.shared_image, None);
            }
            if self.shared_image_memory != vk::DeviceMemory::null() {
                device.free_memory(self.shared_image_memory, None);
            }

            // Vulkan resources
            device.destroy_fence(self.fence, None);
            self.vk.destroy_command_pool();

            // OpenXR resources (use NEXT static — no need for &NextDispatch param)
            if let Some(next) = crate::NEXT.get() {
                if self.space != xr::Space::NULL {
                    (next.destroy_space)(self.space);
                }
                if self.swapchain != xr::Swapchain::NULL {
                    (next.destroy_swapchain)(self.swapchain);
                }
            }

            // Pipe handle
            #[cfg(target_os = "windows")]
            if let Some(h) = self.pipe {
                windows_sys::Win32::Foundation::CloseHandle(h);
            }
        }
        log::info!("[ClearXR Layer] DashboardOverlay destroyed.");
    }
}

// ============================================================
// Helpers
// ============================================================

unsafe fn pick_swapchain_format(next: &NextDispatch, session: xr::Session) -> Result<vk::Format, String> {
    let mut count = 0;
    (next.enumerate_swapchain_formats)(session, 0, &mut count, ptr::null_mut());
    let mut formats = vec![0i64; count as usize];
    (next.enumerate_swapchain_formats)(session, count, &mut count, formats.as_mut_ptr());

    // Prefer SRGB so the XR compositor doesn't double-encode sRGB content.
    // The dashboard renders with srgb_framebuffer:true — output bytes are sRGB-encoded.
    let preferred = [
        vk::Format::R8G8B8A8_SRGB,
        vk::Format::B8G8R8A8_SRGB,
        vk::Format::R8G8B8A8_UNORM,
        vk::Format::B8G8R8A8_UNORM,
    ];
    Ok(preferred.iter().copied()
        .find(|f| formats.iter().any(|c| *c == f.as_raw() as i64))
        .unwrap_or(vk::Format::from_raw(formats[0] as i32)))
}

unsafe fn create_swapchain(
    next: &NextDispatch, session: xr::Session, format: vk::Format, width: u32, height: u32,
) -> Result<xr::Swapchain, String> {
    let ci = xr::SwapchainCreateInfo {
        ty: xr::SwapchainCreateInfo::TYPE, next: ptr::null(),
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::TRANSFER_DST,
        format: format.as_raw() as i64,
        sample_count: 1, width, height, face_count: 1, array_size: 1, mip_count: 1,
    };
    let mut swapchain = xr::Swapchain::NULL;
    let r = (next.create_swapchain)(session, &ci, &mut swapchain);
    if r != xr::Result::SUCCESS {
        return Err(format!("CreateSwapchain: {:?}", r));
    }
    Ok(swapchain)
}

unsafe fn enumerate_swapchain_images(
    next: &NextDispatch, swapchain: xr::Swapchain,
) -> Result<Vec<xr::SwapchainImageVulkanKHR>, String> {
    let mut count = 0;
    (next.enumerate_swapchain_images)(swapchain, 0, &mut count, ptr::null_mut());
    let mut images: Vec<MaybeUninit<xr::SwapchainImageVulkanKHR>> = (0..count)
        .map(|_| xr::SwapchainImageVulkanKHR::out(ptr::null_mut()))
        .collect();
    (next.enumerate_swapchain_images)(swapchain, count, &mut count,
        images.as_mut_ptr() as *mut xr::SwapchainImageBaseHeader);
    Ok(images.into_iter().map(|i| i.assume_init()).collect())
}

unsafe fn create_stage_space(next: &NextDispatch, session: xr::Session) -> Result<xr::Space, String> {
    // Use STAGE space so the overlay quad is in the same coordinate system
    // as controller aim poses from xrLocateSpace (which apps locate relative to STAGE).
    let ci = xr::ReferenceSpaceCreateInfo {
        ty: xr::ReferenceSpaceCreateInfo::TYPE, next: ptr::null(),
        reference_space_type: xr::ReferenceSpaceType::STAGE,
        pose_in_reference_space: xr::Posef {
            orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
            position: xr::Vector3f { x: 0.0, y: 0.0, z: 0.0 },
        },
    };
    let mut space = xr::Space::NULL;
    let r = (next.create_reference_space)(session, &ci, &mut space);
    if r != xr::Result::SUCCESS {
        // Fall back to LOCAL if STAGE isn't supported
        if r == xr::Result::ERROR_REFERENCE_SPACE_UNSUPPORTED {
            log::warn!("[ClearXR Layer] STAGE space not supported, falling back to LOCAL.");
            let ci_local = xr::ReferenceSpaceCreateInfo {
                ty: xr::ReferenceSpaceCreateInfo::TYPE, next: ptr::null(),
                reference_space_type: xr::ReferenceSpaceType::LOCAL,
                pose_in_reference_space: xr::Posef {
                    orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
                    position: xr::Vector3f { x: 0.0, y: 0.0, z: 0.0 },
                },
            };
            let r2 = (next.create_reference_space)(session, &ci_local, &mut space);
            if r2 != xr::Result::SUCCESS {
                return Err(format!("CreateReferenceSpace(LOCAL fallback): {:?}", r2));
            }
            return Ok(space);
        }
        return Err(format!("CreateReferenceSpace(STAGE): {:?}", r));
    }
    Ok(space)
}

// ============================================================
// DashboardInputPacket — simplified packet sent to the dashboard
// ============================================================

/// Pre-computed input packet. Must match clearxr-dashboard/src/input_pipe.rs.
#[repr(C)]
#[derive(Copy, Clone)]
struct DashboardInputPacket {
    magic: u16,        // 0x4449 ("DI")
    flags: u8,         // bit 0: has_pointer
    _pad: u8,
    pointer_u: f32,
    pointer_v: f32,
    trigger: f32,
    grip: f32,
    thumbstick_y: f32,
}

// ============================================================
// Vector math for ray-quad intersection
// ============================================================

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn quat_rotate(q: &[f32; 4], v: [f32; 3]) -> [f32; 3] {
    let qv = [q[0], q[1], q[2]];
    let w = q[3];
    let t = scale(cross(qv, v), 2.0);
    add(add(v, scale(t, w)), cross(qv, t))
}

fn length(a: [f32; 3]) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = length(a).max(1e-10);
    [a[0] / l, a[1] / l, a[2] / l]
}

fn ray_quad_hit(
    ray_origin: [f32; 3], ray_dir: [f32; 3],
    center: [f32; 3], normal: [f32; 3], right: [f32; 3], up: [f32; 3],
    half_w: f32, half_h: f32,
) -> Option<(f32, f32, f32)> {
    let denom = dot(ray_dir, normal);
    if denom.abs() < 1e-6 { return None; }
    let t = dot(sub(center, ray_origin), normal) / denom;
    if t < 0.0 { return None; }
    let hit = add(ray_origin, scale(ray_dir, t));
    let local = sub(hit, center);
    let u = dot(local, right) / (half_w * 2.0) + 0.5;
    let v = 0.5 - dot(local, up) / (half_h * 2.0);
    if u >= 0.0 && u <= 1.0 && v >= 0.0 && v <= 1.0 {
        Some((u, v, t))
    } else {
        None
    }
}

// ============================================================
// Pipe connection
// ============================================================

#[cfg(target_os = "windows")]
fn connect_pipe() -> Option<windows_sys::Win32::Foundation::HANDLE> {
    let name: Vec<u8> = format!("{}\0", PIPE_NAME).into_bytes();
    let handle = unsafe {
        windows_sys::Win32::Storage::FileSystem::CreateFileA(
            name.as_ptr(),
            0x40000000, // GENERIC_WRITE
            0, // no sharing
            ptr::null(),
            3, // OPEN_EXISTING
            0,
            ptr::null_mut(), // no template
        )
    };
    if handle == -1isize as windows_sys::Win32::Foundation::HANDLE {
        None
    } else {
        log::info!("[ClearXR Layer] Pipe connected: {}", PIPE_NAME);
        Some(handle)
    }
}

//! Minimal OpenXR session that keeps the dashboard overlay visible when no game is running.
//!
//! The layer DLL auto-loads as an implicit API layer and injects the dashboard quad.
//! This session just runs the frame loop — it doesn't render anything itself.
//!
//! Lifecycle:
//!   start() → frame loop runs on background thread
//!   yield_session() → destroys session (so a game can create one)
//!   reclaim_session() → re-creates session (game exited/crashed)
//!   stop() → shuts down everything

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use ash::vk;
use openxr as xr;

/// Session states.
const STATE_IDLE: u8 = 0;      // No session, waiting for reclaim
const STATE_RUNNING: u8 = 1;   // Session active, submitting frames
const STATE_YIELDING: u8 = 2;  // Told to yield, will destroy on next iteration

pub struct FallbackSession {
    keep_running: Arc<AtomicBool>,
    session_state: Arc<AtomicU8>,
    thread: Option<JoinHandle<()>>,
}

impl FallbackSession {
    /// Start the fallback session. Creates an OpenXR session immediately.
    pub fn start() -> Result<Self, String> {
        let keep_running = Arc::new(AtomicBool::new(true));
        let session_state = Arc::new(AtomicU8::new(STATE_RUNNING));

        let kr = keep_running.clone();
        let ss = session_state.clone();

        let thread = std::thread::Builder::new()
            .name("fallback-xr".into())
            .spawn(move || {
                if let Err(e) = session_loop(kr, ss) {
                    log::error!("[ClearXR Fallback] Session loop error: {}", e);
                }
            })
            .map_err(|e| format!("Failed to spawn fallback thread: {e}"))?;

        log::info!("[ClearXR Fallback] Started.");
        Ok(Self {
            keep_running,
            session_state,
            thread: Some(thread),
        })
    }

    /// Tell the session to destroy itself so a game can create its own.
    pub fn yield_session(&self) {
        log::info!("[ClearXR Fallback] Yielding session for game launch.");
        self.session_state.store(STATE_YIELDING, Ordering::Release);
    }

    /// Re-create the session (game exited or crashed).
    pub fn reclaim_session(&self) {
        log::info!("[ClearXR Fallback] Reclaiming session.");
        self.session_state.store(STATE_RUNNING, Ordering::Release);
    }

    /// Is the session currently active?
    pub fn is_active(&self) -> bool {
        self.session_state.load(Ordering::Acquire) == STATE_RUNNING
    }

    /// Is the session currently idle (yielded)?
    pub fn is_idle(&self) -> bool {
        self.session_state.load(Ordering::Acquire) == STATE_IDLE
    }
}

impl Drop for FallbackSession {
    fn drop(&mut self) {
        self.keep_running.store(false, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The main session loop. Creates/destroys OpenXR sessions as directed.
fn session_loop(
    keep_running: Arc<AtomicBool>,
    session_state: Arc<AtomicU8>,
) -> Result<(), String> {
    // Load the OpenXR runtime
    let entry = unsafe { xr::Entry::load() }
        .map_err(|e| format!("Failed to load OpenXR runtime: {e}"))?;

    let app_info = xr::ApplicationInfo {
        application_name: "ClearXR Dashboard",
        application_version: 1,
        engine_name: "ClearXR",
        engine_version: 1,
        api_version: xr::Version::new(1, 0, 0),
    };

    // Check for Vulkan support
    let available_extensions = entry.enumerate_extensions()
        .map_err(|e| format!("enumerate extensions: {e}"))?;
    if !available_extensions.khr_vulkan_enable2 && !available_extensions.khr_vulkan_enable {
        return Err("Runtime does not support Vulkan".into());
    }

    let mut required_exts = xr::ExtensionSet::default();
    if available_extensions.khr_vulkan_enable2 {
        required_exts.khr_vulkan_enable2 = true;
    } else {
        required_exts.khr_vulkan_enable = true;
    }

    let instance = entry.create_instance(&app_info, &required_exts, &[])
        .map_err(|e| format!("xrCreateInstance: {e}"))?;

    let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)
        .map_err(|e| format!("xrGetSystem: {e}"))?;

    log::info!("[ClearXR Fallback] OpenXR instance created, system acquired.");

    while keep_running.load(Ordering::Acquire) {
        let state = session_state.load(Ordering::Acquire);

        match state {
            STATE_RUNNING => {
                // Create and run a session
                match run_session(&instance, system, &keep_running, &session_state) {
                    Ok(()) => log::info!("[ClearXR Fallback] Session ended cleanly."),
                    Err(e) => log::warn!("[ClearXR Fallback] Session error: {}", e),
                }
                // After session ends, go idle — but only if we weren't reclaimed
                // while cleaning up (compare_exchange avoids overwriting a concurrent reclaim)
                let _ = session_state.compare_exchange(
                    STATE_YIELDING, STATE_IDLE, Ordering::Release, Ordering::Relaxed,
                );
            }
            STATE_IDLE | STATE_YIELDING => {
                // Wait for reclaim signal
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            _ => break,
        }
    }

    log::info!("[ClearXR Fallback] Session loop exiting.");
    Ok(())
}

/// Create a Vulkan-backed OpenXR session and run the frame loop.
fn run_session(
    instance: &xr::Instance,
    system: xr::SystemId,
    keep_running: &AtomicBool,
    session_state: &AtomicU8,
) -> Result<(), String> {
    // Get Vulkan requirements
    let _reqs = instance.graphics_requirements::<xr::Vulkan>(system)
        .map_err(|e| format!("graphics_requirements: {e}"))?;

    // ── Vulkan setup (same pattern as Space's VkBackend::new) ──

    // 1. Required instance extensions from the XR runtime
    let req_inst_exts = instance.vulkan_legacy_instance_extensions(system)
        .map_err(|e| format!("vulkan_legacy_instance_extensions: {e}"))?;
    log::info!("[ClearXR Fallback] Required VkInstance extensions: {}", req_inst_exts);

    let inst_ext_cstrings: Vec<std::ffi::CString> = req_inst_exts
        .split_ascii_whitespace()
        .map(|s| std::ffi::CString::new(s).unwrap())
        .collect();
    let inst_ext_ptrs: Vec<*const std::ffi::c_char> =
        inst_ext_cstrings.iter().map(|s| s.as_ptr()).collect();

    // 2. Create Vulkan instance
    let vk_entry = unsafe { ash::Entry::load() }
        .map_err(|e| format!("ash Entry: {e}"))?;

    let vk_app_info = vk::ApplicationInfo::default()
        .api_version(vk::make_api_version(0, 1, 1, 0));
    let vk_instance = unsafe {
        vk_entry.create_instance(
            &vk::InstanceCreateInfo::default()
                .application_info(&vk_app_info)
                .enabled_extension_names(&inst_ext_ptrs),
            None,
        )
    }.map_err(|e| format!("vkCreateInstance: {e}"))?;

    // 3. Physical device — mandated by XR runtime
    let phys_dev_raw = unsafe {
        instance.vulkan_graphics_device(
            system,
            std::mem::transmute(vk_instance.handle()),
        )
    }.map_err(|e| format!("vulkan_graphics_device: {e}"))?;
    let physical_device: vk::PhysicalDevice = unsafe { std::mem::transmute(phys_dev_raw) };

    // 4. Graphics queue family
    let queue_families = unsafe {
        vk_instance.get_physical_device_queue_family_properties(physical_device)
    };
    let queue_family = queue_families.iter().enumerate()
        .find(|(_, p)| p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .map(|(i, _)| i as u32)
        .ok_or("No graphics queue family found")?;

    // 5. Required device extensions from XR runtime
    let req_dev_exts = instance.vulkan_legacy_device_extensions(system)
        .map_err(|e| format!("vulkan_legacy_device_extensions: {e}"))?;
    log::info!("[ClearXR Fallback] Required VkDevice extensions: {}", req_dev_exts);

    let dev_ext_cstrings: Vec<std::ffi::CString> = req_dev_exts
        .split_ascii_whitespace()
        .map(|s| std::ffi::CString::new(s).unwrap())
        .collect();
    let dev_ext_ptrs: Vec<*const std::ffi::c_char> =
        dev_ext_cstrings.iter().map(|s| s.as_ptr()).collect();

    // 6. Create Vulkan device with runtime's required extensions
    let queue_priority = 1.0f32;
    let queue_ci = vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family)
        .queue_priorities(std::slice::from_ref(&queue_priority));
    let vk_device = unsafe {
        vk_instance.create_device(
            physical_device,
            &vk::DeviceCreateInfo::default()
                .queue_create_infos(std::slice::from_ref(&queue_ci))
                .enabled_extension_names(&dev_ext_ptrs),
            None,
        )
    }.map_err(|e| format!("vkCreateDevice: {e}"))?;

    // 7. Create OpenXR session
    let (session, mut frame_waiter, mut frame_stream) = unsafe {
        instance.create_session::<xr::Vulkan>(
            system,
            &xr::vulkan::SessionCreateInfo {
                instance: std::mem::transmute(vk_instance.handle()),
                physical_device: std::mem::transmute(physical_device),
                device: std::mem::transmute(vk_device.handle()),
                queue_family_index: queue_family,
                queue_index: 0,
            },
        )
    }.map_err(|e| format!("xrCreateSession: {e}"))?;

    log::info!("[ClearXR Fallback] Session created successfully.");

    // Create STAGE reference space
    let stage = session.create_reference_space(xr::ReferenceSpaceType::STAGE, xr::Posef::IDENTITY)
        .map_err(|e| format!("create reference space: {e}"))?;

    // Session state tracking
    let mut xr_session_running = false;
    let mut exit_requested = false;

    // Frame loop
    while keep_running.load(Ordering::Acquire) {
        // Check if we should yield — request graceful exit first
        if session_state.load(Ordering::Acquire) == STATE_YIELDING && !exit_requested {
            if xr_session_running {
                log::info!("[ClearXR Fallback] Yielding — requesting session exit.");
                session.request_exit().map_err(|e| format!("request_exit: {e}"))?;
                exit_requested = true;
                // Continue the event loop to process STOPPING → end → EXITING → break
            } else {
                log::info!("[ClearXR Fallback] Yielding — session not running, breaking.");
                break;
            }
        }

        // Poll OpenXR events
        let mut event_buffer = xr::EventDataBuffer::new();
        while let Some(event) = instance.poll_event(&mut event_buffer)
            .map_err(|e| format!("poll_event: {e}"))?
        {
            match event {
                xr::Event::SessionStateChanged(sce) => {
                    let new_state = sce.state();
                    log::info!("[ClearXR Fallback] Session state: {:?}", new_state);
                    match new_state {
                        xr::SessionState::READY => {
                            // Use the runtime's preferred view configuration
                            let view_configs = instance.enumerate_view_configurations(system)
                                .map_err(|e| format!("enumerate view configs: {e}"))?;
                            let view_config = view_configs.first()
                                .copied()
                                .unwrap_or(xr::ViewConfigurationType::PRIMARY_STEREO);
                            log::info!("[ClearXR Fallback] Using view config: {:?}", view_config);
                            session.begin(view_config)
                                .map_err(|e| format!("session begin: {e}"))?;
                            xr_session_running = true;
                        }
                        xr::SessionState::STOPPING => {
                            session.end().map_err(|e| format!("session end: {e}"))?;
                            xr_session_running = false;
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => {
                            break;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        if !xr_session_running {
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        // Wait for frame
        let frame_state = frame_waiter.wait()
            .map_err(|e| format!("wait_frame: {e}"))?;

        frame_stream.begin()
            .map_err(|e| format!("begin_frame: {e}"))?;

        // Submit empty frame — the layer adds the dashboard quad via hook_end_frame.
        // We don't submit any layers ourselves. The layer's xrEndFrame hook will
        // append the CompositionLayerQuad with the shared dashboard image.
        frame_stream.end(
            frame_state.predicted_display_time,
            xr::EnvironmentBlendMode::OPAQUE,
            &[], // empty — the layer adds the dashboard overlay
        ).map_err(|e| format!("end_frame: {e}"))?;
    }

    // Cleanup
    drop(stage);
    drop(session);
    drop(frame_stream);
    drop(frame_waiter);

    unsafe {
        vk_device.destroy_device(None);
        vk_instance.destroy_instance(None);
    }

    log::info!("[ClearXR Fallback] Session destroyed, Vulkan cleaned up.");
    Ok(())
}

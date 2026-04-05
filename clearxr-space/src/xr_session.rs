/// OpenXR instance, system, and session lifecycle management.
///
/// Supports two view configurations:
///   PRIMARY_QUAD_VARJO  – 4 views (2 peripheral + 2 foveal inset) if XR_VARJO_quad_views is available
///   PRIMARY_STEREO      – 2 views (standard left/right) as fallback

use anyhow::Result;
use glam;
use log::{debug, info, warn};
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc};

use openxr as xr;

use crate::mirror_window::MirrorWindow;
use crate::renderer::{HandData, Renderer};
use crate::vk_backend::VkBackend;

pub fn run(keep_running: Arc<AtomicBool>) -> Result<()> {
    // --------------------------------------------------------
    // 1. OpenXR instance
    // --------------------------------------------------------
    let xr_entry = load_openxr_entry()?;

    // Check which extensions are available before requesting them.
    let available = xr_entry.enumerate_extensions()?;

    let mut extensions = xr::ExtensionSet::default();
    extensions.khr_vulkan_enable = true;

    let has_quad_views = available.varjo_quad_views;
    if has_quad_views {
        extensions.varjo_quad_views = true;
        info!("XR_VARJO_quad_views extension available – will use foveated quad views.");
    } else {
        info!("XR_VARJO_quad_views not available – using standard stereo.");
    }

    let has_hand_tracking = available.ext_hand_tracking;
    if has_hand_tracking {
        extensions.ext_hand_tracking = true;
        info!("XR_EXT_hand_tracking extension available – enabling hand tracking.");
    } else {
        info!("XR_EXT_hand_tracking not available – hand tracking disabled.");
    }

    let has_depth = available.khr_composition_layer_depth;
    if has_depth {
        extensions.khr_composition_layer_depth = true;
        info!("XR_KHR_composition_layer_depth available – enabling depth submission.");
    } else {
        info!("XR_KHR_composition_layer_depth not available – depth disabled.");
    }

    let xr_instance = xr_entry.create_instance(
        &xr::ApplicationInfo {
            application_name: "Clear XR",
            application_version: 1,
            engine_name: "Clear XR Engine",
            engine_version: 1,
        },
        &extensions,
        &[],
    )?;

    let props = xr_instance.properties()?;
    info!(
        "OpenXR runtime: {} {}",
        props.runtime_name, props.runtime_version
    );

    // --------------------------------------------------------
    // 2. System
    // --------------------------------------------------------
    let system = xr_instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;

    let vk_reqs = xr_instance.graphics_requirements::<xr::Vulkan>(system)?;
    let min_ver = vk_reqs.min_api_version_supported;
    info!("Vulkan requirement: API >= {}.{}", min_ver.major(), min_ver.minor());


    // --------------------------------------------------------
    // 3. Choose view configuration: quad views (foveated) or stereo
    // --------------------------------------------------------
    let supported_configs = xr_instance.enumerate_view_configurations(system)?;

    let (view_config_type, view_config_label) =
        if has_quad_views && supported_configs.contains(&xr::ViewConfigurationType::PRIMARY_QUAD_VARJO) {
            (xr::ViewConfigurationType::PRIMARY_QUAD_VARJO, "PRIMARY_QUAD_VARJO (foveated)")
        } else {
            (xr::ViewConfigurationType::PRIMARY_STEREO, "PRIMARY_STEREO")
        };
    info!("View configuration: {}", view_config_label);

    let view_configs = xr_instance.enumerate_view_configuration_views(system, view_config_type)?;

    let num_views = view_configs.len();
    if num_views < 2 {
        anyhow::bail!("Expected at least 2 views, got {}", num_views);
    }

    for (i, vc) in view_configs.iter().enumerate() {
        let label = if num_views == 4 {
            match i { 0 => "peripheral L", 1 => "peripheral R", 2 => "foveal L", 3 => "foveal R", _ => "?" }
        } else {
            match i { 0 => "left", 1 => "right", _ => "?" }
        };
        info!(
            "  View {} ({}): {}×{} px",
            i, label,
            vc.recommended_image_rect_width,
            vc.recommended_image_rect_height,
        );
    }

    // --------------------------------------------------------
    // 3b. Action set + controller actions (before session creation)
    // --------------------------------------------------------
    let action_set = xr_instance.create_action_set("input", "Input", 0)?;
    let left_hand_path = xr_instance.string_to_path("/user/hand/left")?;
    let right_hand_path = xr_instance.string_to_path("/user/hand/right")?;

    let grip_action = action_set.create_action::<xr::Posef>(
        "grip", "Grip Pose", &[left_hand_path, right_hand_path],
    )?;
    let aim_action = action_set.create_action::<xr::Posef>(
        "aim", "Aim Pose", &[left_hand_path, right_hand_path],
    )?;

    // Oculus Touch input actions
    let trigger_action = action_set.create_action::<f32>(
        "trigger", "Trigger", &[left_hand_path, right_hand_path],
    )?;
    let squeeze_action = action_set.create_action::<f32>(
        "squeeze", "Squeeze", &[left_hand_path, right_hand_path],
    )?;
    let thumbstick_action = action_set.create_action::<xr::Vector2f>(
        "thumbstick", "Thumbstick", &[left_hand_path, right_hand_path],
    )?;
    let thumbstick_click_action = action_set.create_action::<bool>(
        "thumbstick_click", "Thumbstick Click", &[left_hand_path, right_hand_path],
    )?;
    let a_touch_action = action_set.create_action::<bool>(
        "a_touch", "A Touch", &[right_hand_path],
    )?;
    let b_touch_action = action_set.create_action::<bool>(
        "b_touch", "B Touch", &[right_hand_path],
    )?;
    let x_touch_action = action_set.create_action::<bool>(
        "x_touch", "X Touch", &[left_hand_path],
    )?;
    let y_touch_action = action_set.create_action::<bool>(
        "y_touch", "Y Touch", &[left_hand_path],
    )?;
    let a_click_action = action_set.create_action::<bool>(
        "a_click", "A Click", &[right_hand_path],
    )?;
    let b_click_action = action_set.create_action::<bool>(
        "b_click", "B Click", &[right_hand_path],
    )?;
    let x_click_action = action_set.create_action::<bool>(
        "x_click", "X Click", &[left_hand_path],
    )?;
    let y_click_action = action_set.create_action::<bool>(
        "y_click", "Y Click", &[left_hand_path],
    )?;
    let menu_click_action = action_set.create_action::<bool>(
        "menu_click", "Menu Click", &[left_hand_path, right_hand_path],
    )?;
    let haptic_action = action_set.create_action::<xr::Haptic>(
        "haptic", "Haptic Feedback", &[left_hand_path, right_hand_path],
    )?;
    let trigger_touch_action = action_set.create_action::<bool>(
        "trigger_touch", "Trigger Touch", &[left_hand_path, right_hand_path],
    )?;
    let squeeze_touch_action = action_set.create_action::<bool>(
        "squeeze_touch", "Squeeze Touch", &[left_hand_path, right_hand_path],
    )?;
    let thumbstick_touch_action = action_set.create_action::<bool>(
        "thumbstick_touch", "Thumbstick Touch", &[left_hand_path, right_hand_path],
    )?;

    // Suggest bindings for the simple controller profile (poses only)
    if let Err(e) = xr_instance.suggest_interaction_profile_bindings(
        xr_instance.string_to_path("/interaction_profiles/khr/simple_controller")?,
        &[
            xr::Binding::new(&grip_action, xr_instance.string_to_path("/user/hand/left/input/grip/pose")?),
            xr::Binding::new(&grip_action, xr_instance.string_to_path("/user/hand/right/input/grip/pose")?),
            xr::Binding::new(&aim_action, xr_instance.string_to_path("/user/hand/left/input/aim/pose")?),
            xr::Binding::new(&aim_action, xr_instance.string_to_path("/user/hand/right/input/aim/pose")?),
        ],
    ) {
        warn!("Failed to suggest simple_controller bindings: {:?}", e);
    }

    // Suggest bindings for Oculus Touch controller profile
    if let Err(e) = xr_instance.suggest_interaction_profile_bindings(
        xr_instance.string_to_path("/interaction_profiles/oculus/touch_controller")?,
        &[
            // Poses
            xr::Binding::new(&grip_action, xr_instance.string_to_path("/user/hand/left/input/grip/pose")?),
            xr::Binding::new(&grip_action, xr_instance.string_to_path("/user/hand/right/input/grip/pose")?),
            xr::Binding::new(&aim_action, xr_instance.string_to_path("/user/hand/left/input/aim/pose")?),
            xr::Binding::new(&aim_action, xr_instance.string_to_path("/user/hand/right/input/aim/pose")?),
            // Trigger
            xr::Binding::new(&trigger_action, xr_instance.string_to_path("/user/hand/left/input/trigger/value")?),
            xr::Binding::new(&trigger_action, xr_instance.string_to_path("/user/hand/right/input/trigger/value")?),
            // Squeeze
            xr::Binding::new(&squeeze_action, xr_instance.string_to_path("/user/hand/left/input/squeeze/value")?),
            xr::Binding::new(&squeeze_action, xr_instance.string_to_path("/user/hand/right/input/squeeze/value")?),
            // Thumbstick
            xr::Binding::new(&thumbstick_action, xr_instance.string_to_path("/user/hand/left/input/thumbstick")?),
            xr::Binding::new(&thumbstick_action, xr_instance.string_to_path("/user/hand/right/input/thumbstick")?),
            xr::Binding::new(&thumbstick_click_action, xr_instance.string_to_path("/user/hand/left/input/thumbstick/click")?),
            xr::Binding::new(&thumbstick_click_action, xr_instance.string_to_path("/user/hand/right/input/thumbstick/click")?),
            // Button touches (right: A/B, left: X/Y)
            xr::Binding::new(&a_touch_action, xr_instance.string_to_path("/user/hand/right/input/a/touch")?),
            xr::Binding::new(&b_touch_action, xr_instance.string_to_path("/user/hand/right/input/b/touch")?),
            xr::Binding::new(&x_touch_action, xr_instance.string_to_path("/user/hand/left/input/x/touch")?),
            xr::Binding::new(&y_touch_action, xr_instance.string_to_path("/user/hand/left/input/y/touch")?),
            // Button clicks (right: A/B, left: X/Y)
            xr::Binding::new(&a_click_action, xr_instance.string_to_path("/user/hand/right/input/a/click")?),
            xr::Binding::new(&b_click_action, xr_instance.string_to_path("/user/hand/right/input/b/click")?),
            xr::Binding::new(&x_click_action, xr_instance.string_to_path("/user/hand/left/input/x/click")?),
            xr::Binding::new(&y_click_action, xr_instance.string_to_path("/user/hand/left/input/y/click")?),
            // Menu
            xr::Binding::new(&menu_click_action, xr_instance.string_to_path("/user/hand/left/input/menu/click")?),
            xr::Binding::new(&menu_click_action, xr_instance.string_to_path("/user/hand/right/input/menu/click")?),
            
            // Haptic output
            xr::Binding::new(&haptic_action, xr_instance.string_to_path("/user/hand/left/output/haptic")?),
            xr::Binding::new(&haptic_action, xr_instance.string_to_path("/user/hand/right/output/haptic")?),
            // Grip/Trigger/thumbstick touch (squeeze/touch does NOT exist in the Oculus Touch profile —
            // squeeze touch data comes from the opaque data channel instead)

            xr::Binding::new(&squeeze_touch_action, xr_instance.string_to_path("/user/hand/left/input/squeeze/touch")?),
            xr::Binding::new(&squeeze_touch_action, xr_instance.string_to_path("/user/hand/right/input/squeeze/touch")?),
            
            xr::Binding::new(&trigger_touch_action, xr_instance.string_to_path("/user/hand/left/input/trigger/touch")?),
            xr::Binding::new(&trigger_touch_action, xr_instance.string_to_path("/user/hand/right/input/trigger/touch")?),
            xr::Binding::new(&thumbstick_touch_action, xr_instance.string_to_path("/user/hand/left/input/thumbstick/touch")?),
            xr::Binding::new(&thumbstick_touch_action, xr_instance.string_to_path("/user/hand/right/input/thumbstick/touch")?),
        ],
    ) {
        warn!("Failed to suggest oculus/touch_controller bindings: {:?}", e);
    }

    // --------------------------------------------------------
    // 4. Vulkan backend
    // --------------------------------------------------------
    let vk = VkBackend::new(&xr_instance, system)?;

    // --------------------------------------------------------
    // 5. OpenXR session
    // --------------------------------------------------------
    let (session, mut frame_waiter, mut frame_stream) = unsafe {
        xr_instance.create_session::<xr::Vulkan>(
            system,
            &xr::vulkan::SessionCreateInfo {
                instance:         vk.vk_instance_ptr(),
                physical_device:  vk.vk_physical_device_ptr(),
                device:           vk.vk_device_ptr(),
                queue_family_index: vk.queue_family_index(),
                queue_index: 0,
            },
        )
    }?;

    // --------------------------------------------------------
    // 6. Reference space
    // --------------------------------------------------------
    let stage = session
        .create_reference_space(xr::ReferenceSpaceType::STAGE, xr::Posef::IDENTITY)?;

    // --------------------------------------------------------
    // 6a. Attach action sets + create action spaces
    // --------------------------------------------------------
    session.attach_action_sets(&[&action_set])?;

    let grip_space_left = grip_action.create_space(
        session.clone(), left_hand_path, xr::Posef::IDENTITY,
    )?;
    let grip_space_right = grip_action.create_space(
        session.clone(), right_hand_path, xr::Posef::IDENTITY,
    )?;
    let aim_space_left = aim_action.create_space(
        session.clone(), left_hand_path, xr::Posef::IDENTITY,
    )?;
    let aim_space_right = aim_action.create_space(
        session.clone(), right_hand_path, xr::Posef::IDENTITY,
    )?;

    // --------------------------------------------------------
    // 6b. Hand trackers (if ext_hand_tracking is available)
    // --------------------------------------------------------
    let hand_tracker_left = if has_hand_tracking {
        match session.create_hand_tracker(xr::Hand::LEFT) {
            Ok(t) => {
                info!("Left hand tracker created.");
                Some(t)
            }
            Err(e) => {
                warn!("Failed to create left hand tracker: {:?}", e);
                None
            }
        }
    } else {
        None
    };
    let hand_tracker_right = if has_hand_tracking {
        match session.create_hand_tracker(xr::Hand::RIGHT) {
            Ok(t) => {
                info!("Right hand tracker created.");
                Some(t)
            }
            Err(e) => {
                warn!("Failed to create right hand tracker: {:?}", e);
                None
            }
        }
    } else {
        None
    };

    // --------------------------------------------------------
    // 6c. Opaque data channel (CloudXR controller workaround)
    // --------------------------------------------------------
    // if let Some(ref mut ch) = opaque_channel {
    //     if !ch.create_channel() {
    //         warn!("Could not create opaque channel!");
    //         opaque_channel = None;
    //     }
    // }

    // --------------------------------------------------------
    // 7. Renderer – one swapchain per view (2 or 4)
    // --------------------------------------------------------
    let mut renderer = Renderer::new(&vk, &session, &view_configs, has_depth)?;

    // --------------------------------------------------------
    // 7b. Desktop mirror window
    // --------------------------------------------------------
    let mut mirror = match MirrorWindow::new(
        vk.entry(),
        vk.instance_ref(),
        vk.device(),
        vk.physical_device(),
        vk.queue_family_index(),
        vk.command_pool,
    ) {
        Ok(m) => {
            info!("Desktop mirror window created.");
            Some(m)
        }
        Err(e) => {
            warn!("Mirror window unavailable: {}", e);
            None
        }
    };

    // --------------------------------------------------------
    // 8. Main loop
    // --------------------------------------------------------
    let mut session_running = false;
    let mut event_buf = xr::EventDataBuffer::new();
    let start = std::time::Instant::now();

    // Haptic edge detection: [left, right] previous-frame trigger/grip states
    let mut prev_trigger_pulled = [false; 2];
    let mut prev_grip_clicked = [false; 2];

    'main: loop {
        // Pump desktop mirror window events; close → exit
        if let Some(ref mut m) = mirror {
            if !m.pump_events() {
                info!("Mirror window closed, requesting exit.");
                keep_running.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }

        if !keep_running.load(std::sync::atomic::Ordering::Relaxed) {
            if session_running {
                session.request_exit()?;
            } else {
                break 'main;
            }
        }

        while let Some(event) = xr_instance.poll_event(&mut event_buf)? {
            use xr::Event::*;
            match event {
                SessionStateChanged(e) => {
                    let state = e.state();
                    info!("XR session state → {:?}", state);
                    match state {
                        xr::SessionState::READY => {
                            session.begin(view_config_type)?;
                            session_running = true;
                            info!("Rendering started ({} views).", num_views);
                        }
                        xr::SessionState::STOPPING => {
                            session_running = false;
                            session.end()?;
                        }
                        xr::SessionState::LOSS_PENDING => {
                            warn!(
                                "Session loss pending – another OpenXR app took the session. \
                                 Clear XR exiting so the new app can start."
                            );
                            break 'main;
                        }
                        xr::SessionState::EXITING => {
                            info!("Runtime requested exit.");
                            break 'main;
                        }
                        _ => {}
                    }
                }
                InstanceLossPending(_) => {
                    warn!("Instance loss pending – exiting.");
                    break 'main;
                }
                EventsLost(e) => {
                    warn!("Lost {} XR events (queue overflow).", e.lost_event_count());
                }
                _ => { debug!("Unhandled XR event."); }
            }
        }

        if !session_running {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        // ---- Frame ----
        let frame_state = frame_waiter.wait()?;
        frame_stream.begin()?;

        if !frame_state.should_render {
            frame_stream.end(
                frame_state.predicted_display_time,
                xr::EnvironmentBlendMode::OPAQUE,
                &[],
            )?;
            continue;
        }

        let (_, views) = session.locate_views(
            view_config_type,
            frame_state.predicted_display_time,
            &stage,
        )?;

        let elapsed = start.elapsed().as_secs_f32();

        // ---- Sync actions and detect controllers ----
        let mut hand_data = HandData::default();

        let controllers_active = if let Err(e) = session.sync_actions(&[xr::ActiveActionSet::new(&action_set)]) {
            debug!("sync_actions failed: {:?}", e);
            false
        } else {
            let left_active = grip_action.is_active(&session, left_hand_path).unwrap_or(false);
            let right_active = grip_action.is_active(&session, right_hand_path).unwrap_or(false);
            left_active || right_active
        };

        if controllers_active {
            // ---- Controllers active: populate controller data, disable hands ----
            let time = frame_state.predicted_display_time;
            let valid_flags = xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;

            // Left controller
            if grip_action.is_active(&session, left_hand_path).unwrap_or(false) {
                let grip_loc = grip_space_left.locate(&stage, time)?;
                let aim_loc = aim_space_left.locate(&stage, time)?;
                if grip_loc.location_flags.contains(valid_flags) {
                    hand_data.active[2] = 1.0; // left controller active
                    let gp = grip_loc.pose.position;
                    hand_data.ctrl_grip[0] = [gp.x, gp.y, gp.z, 0.02];
                    // Extract grip orientation vectors
                    let q = grip_loc.pose.orientation;
                    let rot = glam::Mat4::from_quat(glam::Quat::from_xyzw(q.x, q.y, q.z, q.w));
                    let grip_right = rot.col(0).truncate();
                    let grip_up = rot.col(1).truncate();
                    hand_data.ctrl_grip_right[0] = [grip_right.x, grip_right.y, grip_right.z, 0.0];
                    hand_data.ctrl_grip_up[0] = [grip_up.x, grip_up.y, grip_up.z, 0.0];
                }
                if aim_loc.location_flags.contains(valid_flags) {
                    let ap = aim_loc.pose.position;
                    hand_data.ctrl_aim_pos[0] = [ap.x, ap.y, ap.z, 0.0];
                    let q = aim_loc.pose.orientation;
                    let rot = glam::Mat4::from_quat(glam::Quat::from_xyzw(q.x, q.y, q.z, q.w));
                    let aim_fwd = -rot.col(2).truncate(); // -Z is forward in OpenXR
                    hand_data.ctrl_aim_dir[0] = [aim_fwd.x, aim_fwd.y, aim_fwd.z, 0.0];
                }
            }

            // Right controller
            if grip_action.is_active(&session, right_hand_path).unwrap_or(false) {
                let grip_loc = grip_space_right.locate(&stage, time)?;
                let aim_loc = aim_space_right.locate(&stage, time)?;
                if grip_loc.location_flags.contains(valid_flags) {
                    hand_data.active[3] = 1.0; // right controller active
                    let gp = grip_loc.pose.position;
                    hand_data.ctrl_grip[1] = [gp.x, gp.y, gp.z, 0.02];
                    // Extract grip orientation vectors
                    let q = grip_loc.pose.orientation;
                    let rot = glam::Mat4::from_quat(glam::Quat::from_xyzw(q.x, q.y, q.z, q.w));
                    let grip_right = rot.col(0).truncate();
                    let grip_up = rot.col(1).truncate();
                    hand_data.ctrl_grip_right[1] = [grip_right.x, grip_right.y, grip_right.z, 0.0];
                    hand_data.ctrl_grip_up[1] = [grip_up.x, grip_up.y, grip_up.z, 0.0];
                }
                if aim_loc.location_flags.contains(valid_flags) {
                    let ap = aim_loc.pose.position;
                    hand_data.ctrl_aim_pos[1] = [ap.x, ap.y, ap.z, 0.0];
                    let q = aim_loc.pose.orientation;
                    let rot = glam::Mat4::from_quat(glam::Quat::from_xyzw(q.x, q.y, q.z, q.w));
                    let aim_fwd = -rot.col(2).truncate();
                    hand_data.ctrl_aim_dir[1] = [aim_fwd.x, aim_fwd.y, aim_fwd.z, 0.0];
                }
            }

            // Read input states for left controller
            // Analog values from OpenXR (these work on CloudXR)
            let trigger_l = trigger_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(0.0);
            let squeeze_l = squeeze_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(0.0);
            let thumbstick_l = thumbstick_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or_default();

            let thumbstick_click_l = thumbstick_click_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(false);
            let x_touch = x_touch_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(false);
            let y_touch = y_touch_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(false);
            let menu_click = menu_click_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(false);
            let x_click = x_click_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(false);
            let y_click = y_click_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(false);
            let trigger_touch_l = trigger_touch_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(false);
            let squeeze_touch_l = squeeze_touch_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(false);
            let thumbstick_touch_l = thumbstick_touch_action.state(&session, left_hand_path).map(|s| s.current_state).unwrap_or(false);

            hand_data.ctrl_inputs[0] = [trigger_l, squeeze_l, thumbstick_l.x, thumbstick_l.y];
            hand_data.ctrl_buttons[0] = [
                if x_touch { 1.0 } else { 0.0 },
                if y_touch { 1.0 } else { 0.0 },
                if thumbstick_click_l { 1.0 } else { 0.0 },
                if menu_click { 1.0 } else { 0.0 },
            ];
            hand_data.ctrl_clicks[0] = [
                if x_click { 1.0 } else { 0.0 },
                if y_click { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ];
            hand_data.ctrl_touches[0] = [
                if trigger_touch_l { 1.0 } else { 0.0 },
                if squeeze_touch_l { 1.0 } else { 0.0 },
                if thumbstick_touch_l { 1.0 } else { 0.0 },
                0.0,
            ];

            // Read input states for right controller
            let trigger_r = trigger_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(0.0);
            let squeeze_r = squeeze_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(0.0);
            let thumbstick_r = thumbstick_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or_default();
            let thumbstick_click_r = thumbstick_click_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(false);
            let a_touch = a_touch_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(false);
            let b_touch = b_touch_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(false);
            let menu_click = menu_click_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(false);
            let a_click = a_click_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(false);
            let b_click = b_click_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(false);
            let trigger_touch_r = trigger_touch_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(false);
            let squeeze_touch_r = squeeze_touch_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(false);
            let thumbstick_touch_r = thumbstick_touch_action.state(&session, right_hand_path).map(|s| s.current_state).unwrap_or(false);

            hand_data.ctrl_inputs[1] = [trigger_r, squeeze_r, thumbstick_r.x, thumbstick_r.y];
            hand_data.ctrl_buttons[1] = [
                if a_touch { 1.0 } else { 0.0 },
                if b_touch { 1.0 } else { 0.0 },
                if thumbstick_click_r { 1.0 } else { 0.0 },
                if menu_click { 1.0 } else { 0.0 },
            ];
            hand_data.ctrl_clicks[1] = [
                if a_click { 1.0 } else { 0.0 },
                if b_click { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ];
            hand_data.ctrl_touches[1] = [
                if trigger_touch_r { 1.0 } else { 0.0 },
                if squeeze_touch_r { 1.0 } else { 0.0 },
                if thumbstick_touch_r { 1.0 } else { 0.0 },
                0.0,
            ];

            // Opaque channel is authoritative for touch/click events
            // (CloudXR bug: PSVR2 controllers report stale/missing data via OpenXR).
            // Write BOTH 0 and 1 so the opaque channel fully replaces OpenXR values.
            // if let Some(ref mut ch) = opaque_channel {
            //     if let Some(pkt) = ch.poll() {
            //         let bit = |b: u16, mask: u16| -> f32 { if b & mask != 0 { 1.0 } else { 0.0 } };
            //         if pkt.active_hands & 0x01 != 0 {
            //             let b = pkt.left.buttons;
            //             hand_data.ctrl_buttons[0][0] = bit(b, opaque_channel::SC_TOUCH_A);
            //             hand_data.ctrl_buttons[0][1] = bit(b, opaque_channel::SC_TOUCH_B);
            //             hand_data.ctrl_buttons[0][2] = bit(b, opaque_channel::SC_BTN_THUMBSTICK);
            //             hand_data.ctrl_buttons[0][3] = bit(b, opaque_channel::SC_BTN_MENU);
            //             hand_data.ctrl_touches[0][0] = bit(b, opaque_channel::SC_TOUCH_TRIGGER);
            //             hand_data.ctrl_touches[0][1] = bit(b, opaque_channel::SC_TOUCH_GRIP);
            //             hand_data.ctrl_touches[0][2] = bit(b, opaque_channel::SC_TOUCH_THUMBSTICK);
            //         }
            //         if pkt.active_hands & 0x02 != 0 {
            //             let b = pkt.right.buttons;
            //             hand_data.ctrl_buttons[1][0] = bit(b, opaque_channel::SC_TOUCH_A);
            //             hand_data.ctrl_buttons[1][1] = bit(b, opaque_channel::SC_TOUCH_B);
            //             hand_data.ctrl_buttons[1][2] = bit(b, opaque_channel::SC_BTN_THUMBSTICK);
            //             hand_data.ctrl_buttons[1][3] = bit(b, opaque_channel::SC_BTN_MENU);
                    
            //             hand_data.ctrl_touches[1][0] = bit(b, opaque_channel::SC_TOUCH_TRIGGER);
            //             hand_data.ctrl_touches[1][1] = bit(b, opaque_channel::SC_TOUCH_GRIP);
            //             hand_data.ctrl_touches[1][2] = bit(b, opaque_channel::SC_TOUCH_THUMBSTICK);
            //         }
            //     }
            // }

            // ---- Haptic feedback on trigger pull / grip click ----
            let triggers = [trigger_l, trigger_r];
            let squeezes = [squeeze_l, squeeze_r];
            let hand_paths = [left_hand_path, right_hand_path];
            for i in 0..2 {
                let trigger_pulled = triggers[i] > 0.8;
                let grip_clicked = squeezes[i] > 0.8;

                // Rising edge: trigger pull → short pulse
                if trigger_pulled && !prev_trigger_pulled[i] {
                    let dur_ns: u64 = 50_000_000; // 50ms
                    let freq: f32 = 200.0;
                    let amp: f32 = 0.5;
                    let vib = xr::HapticVibration::new()
                        .duration(xr::Duration::from_nanos(dur_ns as i64))
                        .frequency(freq)
                        .amplitude(amp);
                    let _ = haptic_action.apply_feedback(&session, hand_paths[i], &vib);

                }

                // Rising edge: grip click → longer pulse
                if grip_clicked && !prev_grip_clicked[i] {
                    let dur_ns: u64 = 150_000_000; // 150ms
                    let freq: f32 = 120.0;
                    let amp: f32 = 0.8;
                    let vib = xr::HapticVibration::new()
                        .duration(xr::Duration::from_nanos(dur_ns as i64))
                        .frequency(freq)
                        .amplitude(amp);
                    let _ = haptic_action.apply_feedback(&session, hand_paths[i], &vib);
  
                }

                prev_trigger_pulled[i] = trigger_pulled;
                prev_grip_clicked[i] = grip_clicked;
            }

            // hands stay inactive (active[0] and active[1] remain 0.0)
        } else {
            // ---- No controllers: fall back to hand tracking ----
            if let Some(ref tracker) = hand_tracker_left {
                match stage.locate_hand_joints(tracker, frame_state.predicted_display_time) {
                    Ok(Some(joints)) => {
                        hand_data.active[0] = 1.0;
                        for (i, joint) in joints.iter().enumerate() {
                            let p = joint.pose.position;
                            hand_data.joints[i] = [p.x, p.y, p.z, joint.radius];
                        }
                    }
                    Ok(None) => {} // hand not currently tracked
                    Err(e) => {
                        debug!("Left hand joint locate failed: {:?}", e);
                    }
                }
            }
            if let Some(ref tracker) = hand_tracker_right {
                match stage.locate_hand_joints(tracker, frame_state.predicted_display_time) {
                    Ok(Some(joints)) => {
                        hand_data.active[1] = 1.0;
                        for (i, joint) in joints.iter().enumerate() {
                            let p = joint.pose.position;
                            hand_data.joints[26 + i] = [p.x, p.y, p.z, joint.radius];
                        }
                    }
                    Ok(None) => {} // hand not currently tracked
                    Err(e) => {
                        debug!("Right hand joint locate failed: {:?}", e);
                    }
                }
            }
        }
        renderer.update_hand_data(&hand_data);

        // ---- Render all views (2 for stereo, 4 for quad) ----
        renderer.render_frame(&vk, &views, elapsed, mirror.as_mut())?;

        // ---- Build composition layer dynamically for N views ----
        // Build depth info structs (must live until xrEndFrame)
        let mut depth_infos: Vec<xr::sys::CompositionLayerDepthInfoKHR> = Vec::new();
        if renderer.has_depth {
            for sw in renderer.swapchains.iter() {
                if let Some(ref dh) = sw.depth_handle {
                    let rect = xr::sys::Rect2Di {
                        offset: xr::sys::Offset2Di { x: 0, y: 0 },
                        extent: xr::sys::Extent2Di {
                            width:  sw.resolution.width  as i32,
                            height: sw.resolution.height as i32,
                        },
                    };
                    depth_infos.push(xr::sys::CompositionLayerDepthInfoKHR {
                        ty: xr::sys::CompositionLayerDepthInfoKHR::TYPE,
                        next: std::ptr::null(),
                        sub_image: xr::sys::SwapchainSubImage {
                            swapchain: dh.as_raw(),
                            image_rect: rect,
                            image_array_index: 0,
                        },
                        min_depth: 0.0,
                        max_depth: 1.0,
                        near_z: 0.01,
                        far_z: 100.0,
                    });
                }
            }
        }

        let mut sub_images: Vec<xr::SwapchainSubImage<xr::Vulkan>> = Vec::with_capacity(num_views);
        for sw in renderer.swapchains.iter() {
            let rect = xr::Rect2Di {
                offset: xr::Offset2Di { x: 0, y: 0 },
                extent: xr::Extent2Di {
                    width:  sw.resolution.width  as i32,
                    height: sw.resolution.height as i32,
                },
            };
            sub_images.push(
                xr::SwapchainSubImage::new()
                    .swapchain(&sw.handle)
                    .image_array_index(0)
                    .image_rect(rect),
            );
        }

        let mut proj_views: Vec<_> = views.iter().zip(sub_images.into_iter()).map(|(view, sub)| {
            xr::CompositionLayerProjectionView::new()
                .pose(view.pose)
                .fov(view.fov)
                .sub_image(sub)
        }).collect();

        // Chain depth info into each projection view's `next` pointer.
        // CompositionLayerProjectionView is #[repr(transparent)] over sys::CompositionLayerProjectionView.
        if renderer.has_depth && depth_infos.len() == proj_views.len() {
            for (pv, di) in proj_views.iter_mut().zip(depth_infos.iter()) {
                let raw = pv as *mut xr::CompositionLayerProjectionView<xr::Vulkan>
                    as *mut xr::sys::CompositionLayerProjectionView;
                unsafe { (*raw).next = di as *const _ as *const std::ffi::c_void; }
            }
        }

        let proj_layer = xr::CompositionLayerProjection::new()
            .space(&stage)
            .views(&proj_views);

        frame_stream.end(
            frame_state.predicted_display_time,
            xr::EnvironmentBlendMode::OPAQUE,
            &[&proj_layer],
        )?;
    }

    unsafe { vk.device().device_wait_idle() }?;
    if let Some(ref mut m) = mirror {
        m.destroy(vk.device());
    }
    renderer.destroy(&vk);

    Ok(())
}

// ============================================================
// OpenXR loader discovery with fallback search
// ============================================================

fn load_openxr_entry() -> Result<xr::Entry> {
    if let Ok(path) = std::env::var("OPENXR_LOADER_PATH") {
        info!("OPENXR_LOADER_PATH set: {}", path);
        return unsafe { xr::Entry::load_from(Path::new(&path)) }
            .map_err(|e| anyhow::anyhow!("Failed to load OpenXR from {}: {}", path, e));
    }

    if let Ok(entry) = unsafe { xr::Entry::load() } {
        info!("OpenXR loader found via system DLL search.");
        return Ok(entry);
    }

    let mut searched = vec!["system DLL search path".to_string()];
    let candidates = gather_loader_candidates();
    for candidate in &candidates {
        searched.push(candidate.display().to_string());
        if candidate.exists() {
            info!("Trying OpenXR loader at: {}", candidate.display());
            match unsafe { xr::Entry::load_from(candidate) } {
                Ok(entry) => return Ok(entry),
                Err(e) => warn!("  ...failed: {}", e),
            }
        }
    }

    let searched_list = searched.iter().map(|s| format!("  - {}", s)).collect::<Vec<_>>().join("\n");
    Err(anyhow::anyhow!(
        "Could not find openxr_loader.dll.\n\nSearched:\n{}\n\n\
         Fix: copy openxr_loader.dll next to clear-xr.exe,\n\
         or set OPENXR_LOADER_PATH=<path to the DLL>.",
        searched_list
    ))
}

fn gather_loader_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("openxr_loader.dll"));
        }
    }
    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join("openxr_loader.dll"));
    }
    candidates
}

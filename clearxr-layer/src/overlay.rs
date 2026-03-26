use crate::{egui_gpu_renderer::EguiGpuRenderer, vk_backend::VkBackend, NextDispatch};
use ash::{vk, vk::Handle};
use egui::{Align, Color32, Frame, Layout, RichText, Vec2};
use openxr_sys as xr;
use std::mem::MaybeUninit;
use std::ptr;

const OVERLAY_WIDTH: u32 = 768;
const OVERLAY_HEIGHT: u32 = 384;
const OVERLAY_SIZE_METERS: (f32, f32) = (0.9, 0.45);
const OVERLAY_Z_METERS: f32 = -1.1;
const OVERLAY_Y_METERS: f32 = -0.06;

pub struct DashboardOverlay {
    session: xr::Session,
    swapchain: xr::Swapchain,
    space: xr::Space,
    width: u32,
    height: u32,
    pose: xr::Posef,
    size: xr::Extent2Df,
    visible: bool,
    menu_was_down: bool,
    vk: VkBackend,
    format: vk::Format,
    images: Vec<vk::Image>,
    renderer: EguiGpuRenderer,
}

impl DashboardOverlay {
    pub unsafe fn new(
        next: &NextDispatch,
        session: xr::Session,
        binding: &xr::GraphicsBindingVulkanKHR,
    ) -> Result<Self, String> {
        let vk = VkBackend::from_graphics_binding(binding)
            .map_err(|err| format!("failed to create Vulkan context for dashboard overlay: {err}"))?;
        let format = pick_swapchain_format(next, session)?;
        let swapchain = create_swapchain(next, session, format)?;
        let images = enumerate_swapchain_images(next, swapchain)?
            .into_iter()
            .map(|image| vk::Image::from_raw(image.image as usize as u64))
            .collect();
        let space = create_view_space(next, session)?;
        let renderer = EguiGpuRenderer::new(&vk, OVERLAY_WIDTH, OVERLAY_HEIGHT)
            .map_err(|err| format!("failed to create GPU egui renderer for dashboard overlay: {err}"))?;

        let mut overlay = Self {
            session,
            swapchain,
            space,
            width: OVERLAY_WIDTH,
            height: OVERLAY_HEIGHT,
            pose: xr::Posef {
                orientation: xr::Quaternionf {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    w: 1.0,
                },
                position: xr::Vector3f {
                    x: 0.0,
                    y: OVERLAY_Y_METERS,
                    z: OVERLAY_Z_METERS,
                },
            },
            size: xr::Extent2Df {
                width: OVERLAY_SIZE_METERS.0,
                height: OVERLAY_SIZE_METERS.1,
            },
            visible: true,
            menu_was_down: false,
            vk,
            format,
            images,
            renderer,
        };

        overlay.prime_swapchain_image(next)?;
        Ok(overlay)
    }

    pub fn is_for_session(&self, session: xr::Session) -> bool {
        self.session == session
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn update_menu_button(&mut self, menu_down: bool) -> bool {
        if menu_down && !self.menu_was_down {
            self.menu_was_down = menu_down;
            self.visible = !self.visible;
            return true;
        }
        self.menu_was_down = menu_down;
        false
    }

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

    pub unsafe fn destroy(&mut self, next: &NextDispatch) {
        if self.space != xr::Space::NULL {
            let _ = (next.destroy_space)(self.space);
            self.space = xr::Space::NULL;
        }
        if self.swapchain != xr::Swapchain::NULL {
            let _ = (next.destroy_swapchain)(self.swapchain);
            self.swapchain = xr::Swapchain::NULL;
        }
    }

    unsafe fn prime_swapchain_image(&mut self, next: &NextDispatch) -> Result<(), String> {
        let acquire_info = xr::SwapchainImageAcquireInfo {
            ty: xr::SwapchainImageAcquireInfo::TYPE,
            next: ptr::null(),
        };
        let wait_info = xr::SwapchainImageWaitInfo {
            ty: xr::SwapchainImageWaitInfo::TYPE,
            next: ptr::null(),
            timeout: xr::Duration::INFINITE,
        };
        let release_info = xr::SwapchainImageReleaseInfo {
            ty: xr::SwapchainImageReleaseInfo::TYPE,
            next: ptr::null(),
        };

        let mut image_index = 0;
        let result = (next.acquire_swapchain_image)(self.swapchain, &acquire_info, &mut image_index);
        if result != xr::Result::SUCCESS {
            return Err(format!(
                "xrAcquireSwapchainImage failed for dashboard overlay: {:?}",
                result
            ));
        }

        let result = (next.wait_swapchain_image)(self.swapchain, &wait_info);
        if result != xr::Result::SUCCESS {
            return Err(format!(
                "xrWaitSwapchainImage failed for dashboard overlay: {:?}",
                result
            ));
        }

        self.renderer.force_repaint();
        let image = self.images[image_index as usize];
        let updated = self.renderer.run(&self.vk, image, self.format, false, build_dashboard_ui);
        if !updated {
            return Err("dashboard egui renderer skipped the initial frame".to_string());
        }

        let result = (next.release_swapchain_image)(self.swapchain, &release_info);
        if result != xr::Result::SUCCESS {
            return Err(format!(
                "xrReleaseSwapchainImage failed for dashboard overlay: {:?}",
                result
            ));
        }

        Ok(())
    }
}

impl Drop for DashboardOverlay {
    fn drop(&mut self) {
        unsafe {
            self.renderer.destroy(self.vk.device());
            self.vk.destroy_command_pool();
        }
    }
}

unsafe fn pick_swapchain_format(next: &NextDispatch, session: xr::Session) -> Result<vk::Format, String> {
    let mut count = 0;
    let result = (next.enumerate_swapchain_formats)(session, 0, &mut count, ptr::null_mut());
    if result != xr::Result::SUCCESS || count == 0 {
        return Err(format!(
            "xrEnumerateSwapchainFormats(count) failed for dashboard overlay: {:?}",
            result
        ));
    }

    let mut formats = vec![0i64; count as usize];
    let result = (next.enumerate_swapchain_formats)(session, count, &mut count, formats.as_mut_ptr());
    if result != xr::Result::SUCCESS {
        return Err(format!(
            "xrEnumerateSwapchainFormats(list) failed for dashboard overlay: {:?}",
            result
        ));
    }

    let preferred = [
        vk::Format::R8G8B8A8_SRGB,
        vk::Format::B8G8R8A8_SRGB,
        vk::Format::R8G8B8A8_UNORM,
        vk::Format::B8G8R8A8_UNORM,
    ];
    let selected = preferred
        .iter()
        .copied()
        .find(|format| formats.iter().any(|candidate| *candidate == format.as_raw() as i64))
        .unwrap_or_else(|| vk::Format::from_raw(formats[0] as i32));

    Ok(selected)
}

unsafe fn create_swapchain(
    next: &NextDispatch,
    session: xr::Session,
    format: vk::Format,
) -> Result<xr::Swapchain, String> {
    let create_info = xr::SwapchainCreateInfo {
        ty: xr::SwapchainCreateInfo::TYPE,
        next: ptr::null(),
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::SAMPLED,
        format: format.as_raw() as i64,
        sample_count: 1,
        width: OVERLAY_WIDTH,
        height: OVERLAY_HEIGHT,
        face_count: 1,
        array_size: 1,
        mip_count: 1,
    };
    let mut swapchain = xr::Swapchain::NULL;
    let result = (next.create_swapchain)(session, &create_info, &mut swapchain);
    if result != xr::Result::SUCCESS {
        return Err(format!("xrCreateSwapchain failed for dashboard overlay: {:?}", result));
    }
    Ok(swapchain)
}

unsafe fn enumerate_swapchain_images(
    next: &NextDispatch,
    swapchain: xr::Swapchain,
) -> Result<Vec<xr::SwapchainImageVulkanKHR>, String> {
    let mut count = 0;
    let result = (next.enumerate_swapchain_images)(swapchain, 0, &mut count, ptr::null_mut());
    if result != xr::Result::SUCCESS || count == 0 {
        return Err(format!(
            "xrEnumerateSwapchainImages(count) failed for dashboard overlay: {:?}",
            result
        ));
    }

    let mut images: Vec<MaybeUninit<xr::SwapchainImageVulkanKHR>> = (0..count)
        .map(|_| xr::SwapchainImageVulkanKHR::out(ptr::null_mut()))
        .collect();
    let result = (next.enumerate_swapchain_images)(
        swapchain,
        count,
        &mut count,
        images.as_mut_ptr() as *mut xr::SwapchainImageBaseHeader,
    );
    if result != xr::Result::SUCCESS {
        return Err(format!(
            "xrEnumerateSwapchainImages(list) failed for dashboard overlay: {:?}",
            result
        ));
    }

    Ok(images.into_iter().map(|image| image.assume_init()).collect())
}

unsafe fn create_view_space(next: &NextDispatch, session: xr::Session) -> Result<xr::Space, String> {
    let create_info = xr::ReferenceSpaceCreateInfo {
        ty: xr::ReferenceSpaceCreateInfo::TYPE,
        next: ptr::null(),
        reference_space_type: xr::ReferenceSpaceType::VIEW,
        pose_in_reference_space: xr::Posef {
            orientation: xr::Quaternionf {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 1.0,
            },
            position: xr::Vector3f {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
        },
    };
    let mut space = xr::Space::NULL;
    let result = (next.create_reference_space)(session, &create_info, &mut space);
    if result != xr::Result::SUCCESS {
        return Err(format!(
            "xrCreateReferenceSpace(VIEW) failed for dashboard overlay: {:?}",
            result
        ));
    }
    Ok(space)
}

fn build_dashboard_ui(ctx: &egui::Context) {
    egui::CentralPanel::default()
        .frame(
            Frame::new()
                .fill(Color32::from_rgba_unmultiplied(9, 16, 28, 236))
                .inner_margin(24.0),
        )
        .show(ctx, |ui| {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.heading(
                    RichText::new("ClearXR Dashboard")
                        .size(34.0)
                        .color(Color32::from_rgb(242, 248, 255)),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("GPU-rendered from the OpenXR API layer")
                        .size(18.0)
                        .color(Color32::from_rgb(137, 207, 240)),
                );
                ui.add_space(18.0);

                Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(22, 34, 54, 220))
                    .corner_radius(14.0)
                    .inner_margin(18.0)
                    .show(ui, |ui| {
                        ui.set_min_size(Vec2::new(0.0, 150.0));
                        ui.label(
                            RichText::new("Dashboard overlay host model: confirmed")
                                .size(22.0)
                                .color(Color32::WHITE),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(
                                "This panel is rendered on the GPU into the layer-owned swapchain, then appended during xrEndFrame.",
                            )
                            .size(17.0)
                            .color(Color32::from_rgb(212, 224, 240)),
                        );
                        ui.add_space(12.0);
                        ui.horizontal_wrapped(|ui| {
                            pill(ui, "Visible by default");
                            pill(ui, "Menu toggles overlay");
                            pill(ui, "No second XR session");
                        });
                    });
            });
        });
}

fn pill(ui: &mut egui::Ui, text: &str) {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(77, 191, 255, 42))
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(95, 196, 255)))
        .corner_radius(999.0)
        .inner_margin(egui::Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(15.0)
                    .color(Color32::from_rgb(225, 244, 255)),
            );
        });
}

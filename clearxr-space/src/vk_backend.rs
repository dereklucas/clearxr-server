/// Vulkan instance, physical device, logical device, and queue.
///
/// Two construction paths:
///   - `new()` (feature = "xr"): XR_KHR_vulkan_enable path where the runtime picks the GPU.
///   - `new_standalone()` (feature = "desktop"): standalone Vulkan init for desktop window mode.

use anyhow::Result;
use ash::{vk, vk::Handle, Device, Instance};
use log::info;
#[cfg(feature = "xr")]
use openxr as xr;
use std::ffi::{c_char, c_void, CString};

pub struct VkBackend {
    entry: ash::Entry,
    instance: Instance,
    physical_device: vk::PhysicalDevice,
    device: Device,
    queue_family_index: u32,
    queue: vk::Queue,
    pub command_pool: vk::CommandPool,
}

impl VkBackend {
    // ================================================================
    // XR-driven construction (existing path)
    // ================================================================
    #[cfg(feature = "xr")]
    pub fn new(xr_instance: &xr::Instance, system: xr::SystemId) -> Result<Self> {
        // ---- 1. Required VkInstance extensions from XR runtime ----
        let req_inst_exts_str = xr_instance.vulkan_legacy_instance_extensions(system)?;
        info!("Required VkInstance extensions: {}", req_inst_exts_str);

        // Convert the space-separated list into null-terminated C strings,
        // then add surface extensions needed for the desktop mirror window.
        let mut inst_ext_names: Vec<String> = req_inst_exts_str
            .split_ascii_whitespace()
            .map(|s| s.to_string())
            .collect();
        for ext in ["VK_KHR_surface", "VK_KHR_win32_surface"] {
            if !inst_ext_names.iter().any(|e| e == ext) {
                inst_ext_names.push(ext.to_string());
            }
        }
        let inst_ext_cstrings: Vec<CString> = inst_ext_names
            .iter()
            .map(|s| CString::new(s.as_str()).unwrap())
            .collect();
        let inst_ext_ptrs: Vec<*const c_char> =
            inst_ext_cstrings.iter().map(|s| s.as_ptr()).collect();

        // ---- 2. Vulkan entry ----
        let entry = unsafe { ash::Entry::load()? };

        // ---- 3. VkInstance ----
        let app_info = vk::ApplicationInfo {
            p_application_name: c"Clear XR".as_ptr(),
            application_version: vk::make_api_version(0, 0, 1, 0),
            p_engine_name: c"Clear XR Engine".as_ptr(),
            engine_version: vk::make_api_version(0, 0, 1, 0),
            api_version: vk::API_VERSION_1_1,
            ..Default::default()
        };

        let inst_ci = vk::InstanceCreateInfo {
            p_application_info: &app_info,
            enabled_extension_count: inst_ext_ptrs.len() as u32,
            pp_enabled_extension_names: inst_ext_ptrs.as_ptr(),
            ..Default::default()
        };

        let vk_instance = unsafe { entry.create_instance(&inst_ci, None)? };

        // ---- 4. Physical device – mandated by XR runtime ----
        let phys_dev_raw = unsafe {
            xr_instance.vulkan_graphics_device(
                system,
                vk_instance.handle().as_raw() as usize as *const std::ffi::c_void,
            )?
        };
        let phys_dev = vk::PhysicalDevice::from_raw(phys_dev_raw as usize as u64);

        let props = unsafe { vk_instance.get_physical_device_properties(phys_dev) };
        info!(
            "Selected GPU: {}",
            unsafe { std::ffi::CStr::from_ptr(props.device_name.as_ptr()) }.to_string_lossy()
        );

        // ---- 5. Graphics queue family ----
        let queue_families =
            unsafe { vk_instance.get_physical_device_queue_family_properties(phys_dev) };
        let queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, p)| p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .map(|(i, _)| i as u32)
            .ok_or_else(|| anyhow::anyhow!("No graphics queue family found"))?;

        // ---- 6. Required VkDevice extensions from XR runtime ----
        let req_dev_exts_str = xr_instance.vulkan_legacy_device_extensions(system)?;
        info!("Required VkDevice extensions: {}", req_dev_exts_str);

        let mut dev_ext_names: Vec<String> = req_dev_exts_str
            .split_ascii_whitespace()
            .map(|s| s.to_string())
            .collect();
        if !dev_ext_names.iter().any(|e| e == "VK_KHR_swapchain") {
            dev_ext_names.push("VK_KHR_swapchain".to_string());
        }
        let dev_ext_cstrings: Vec<CString> = dev_ext_names
            .iter()
            .map(|s| CString::new(s.as_str()).unwrap())
            .collect();
        let dev_ext_ptrs: Vec<*const c_char> =
            dev_ext_cstrings.iter().map(|s| s.as_ptr()).collect();

        // ---- 7. VkDevice ----
        let queue_priority = 1.0_f32;
        let queue_ci = vk::DeviceQueueCreateInfo {
            queue_family_index,
            queue_count: 1,
            p_queue_priorities: &queue_priority,
            ..Default::default()
        };

        let dev_ci = vk::DeviceCreateInfo {
            queue_create_info_count: 1,
            p_queue_create_infos: &queue_ci,
            enabled_extension_count: dev_ext_ptrs.len() as u32,
            pp_enabled_extension_names: dev_ext_ptrs.as_ptr(),
            ..Default::default()
        };

        let device = unsafe { vk_instance.create_device(phys_dev, &dev_ci, None)? };
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        // ---- 8. Command pool ----
        let cp_ci = vk::CommandPoolCreateInfo {
            queue_family_index,
            flags: vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
            ..Default::default()
        };
        let command_pool = unsafe { device.create_command_pool(&cp_ci, None)? };

        Ok(Self {
            entry,
            instance: vk_instance,
            physical_device: phys_dev,
            device,
            queue_family_index,
            queue,
            command_pool,
        })
    }

    // ================================================================
    // Standalone construction for desktop window mode
    // ================================================================
    #[cfg(feature = "desktop")]
    pub fn new_standalone(
        required_instance_extensions: &[CString],
    ) -> Result<Self> {
        let entry = unsafe { ash::Entry::load()? };

        // Build instance extension list: caller's surface extensions + portability
        let mut inst_ext_names: Vec<CString> = required_instance_extensions.to_vec();

        // On macOS (MoltenVK), we need portability enumeration
        #[cfg(target_os = "macos")]
        {
            let portability_enum = CString::new("VK_KHR_portability_enumeration").unwrap();
            if !inst_ext_names.iter().any(|e| e == &portability_enum) {
                inst_ext_names.push(portability_enum);
            }
        }

        let inst_ext_ptrs: Vec<*const c_char> =
            inst_ext_names.iter().map(|s| s.as_ptr()).collect();

        let app_info = vk::ApplicationInfo {
            p_application_name: c"Clear XR Desktop".as_ptr(),
            application_version: vk::make_api_version(0, 0, 1, 0),
            p_engine_name: c"Clear XR Engine".as_ptr(),
            engine_version: vk::make_api_version(0, 0, 1, 0),
            api_version: vk::API_VERSION_1_1,
            ..Default::default()
        };

        let mut create_flags = vk::InstanceCreateFlags::empty();
        #[cfg(target_os = "macos")]
        {
            create_flags |= vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR;
        }

        let inst_ci = vk::InstanceCreateInfo {
            p_application_info: &app_info,
            enabled_extension_count: inst_ext_ptrs.len() as u32,
            pp_enabled_extension_names: inst_ext_ptrs.as_ptr(),
            flags: create_flags,
            ..Default::default()
        };

        let vk_instance = unsafe { entry.create_instance(&inst_ci, None)? };

        // Pick the first discrete GPU, or fall back to the first device
        let phys_devs = unsafe { vk_instance.enumerate_physical_devices()? };
        if phys_devs.is_empty() {
            anyhow::bail!("No Vulkan physical devices found");
        }

        let phys_dev = phys_devs
            .iter()
            .find(|&&pd| {
                let props = unsafe { vk_instance.get_physical_device_properties(pd) };
                props.device_type == vk::PhysicalDeviceType::DISCRETE_GPU
            })
            .copied()
            .unwrap_or(phys_devs[0]);

        let props = unsafe { vk_instance.get_physical_device_properties(phys_dev) };
        info!(
            "Selected GPU: {}",
            unsafe { std::ffi::CStr::from_ptr(props.device_name.as_ptr()) }.to_string_lossy()
        );

        // Graphics queue family
        let queue_families =
            unsafe { vk_instance.get_physical_device_queue_family_properties(phys_dev) };
        let queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, p)| p.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .map(|(i, _)| i as u32)
            .ok_or_else(|| anyhow::anyhow!("No graphics queue family found"))?;

        // Device extensions: swapchain + portability subset on macOS
        let mut dev_ext_names: Vec<CString> =
            vec![CString::new("VK_KHR_swapchain").unwrap()];

        #[cfg(target_os = "macos")]
        {
            dev_ext_names.push(CString::new("VK_KHR_portability_subset").unwrap());
        }

        let dev_ext_ptrs: Vec<*const c_char> =
            dev_ext_names.iter().map(|s| s.as_ptr()).collect();

        let queue_priority = 1.0_f32;
        let queue_ci = vk::DeviceQueueCreateInfo {
            queue_family_index,
            queue_count: 1,
            p_queue_priorities: &queue_priority,
            ..Default::default()
        };

        let dev_ci = vk::DeviceCreateInfo {
            queue_create_info_count: 1,
            p_queue_create_infos: &queue_ci,
            enabled_extension_count: dev_ext_ptrs.len() as u32,
            pp_enabled_extension_names: dev_ext_ptrs.as_ptr(),
            ..Default::default()
        };

        let device = unsafe { vk_instance.create_device(phys_dev, &dev_ci, None)? };
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        let cp_ci = vk::CommandPoolCreateInfo {
            queue_family_index,
            flags: vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
            ..Default::default()
        };
        let command_pool = unsafe { device.create_command_pool(&cp_ci, None)? };

        Ok(Self {
            entry,
            instance: vk_instance,
            physical_device: phys_dev,
            device,
            queue_family_index,
            queue,
            command_pool,
        })
    }

    // Raw *const c_void handles for OpenXR's SessionCreateInfo fields.
    pub fn vk_instance_ptr(&self) -> *const c_void {
        self.instance.handle().as_raw() as usize as *const c_void
    }
    pub fn vk_physical_device_ptr(&self) -> *const c_void {
        self.physical_device.as_raw() as usize as *const c_void
    }
    pub fn vk_device_ptr(&self) -> *const c_void {
        self.device.handle().as_raw() as usize as *const c_void
    }
    pub fn queue_family_index(&self) -> u32 {
        self.queue_family_index
    }
    pub fn device(&self) -> &Device {
        &self.device
    }
    pub fn queue(&self) -> vk::Queue {
        self.queue
    }
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }
    pub fn instance_ref(&self) -> &Instance {
        &self.instance
    }
    pub fn entry(&self) -> &ash::Entry {
        &self.entry
    }

    pub fn find_memory_type(
        &self,
        type_filter: u32,
        properties: vk::MemoryPropertyFlags,
    ) -> Option<u32> {
        let mem_props =
            unsafe { self.instance.get_physical_device_memory_properties(self.physical_device) };
        (0..mem_props.memory_type_count).find(|&i| {
            (type_filter & (1 << i)) != 0
                && mem_props.memory_types[i as usize]
                    .property_flags
                    .contains(properties)
        })
    }
}

impl Drop for VkBackend {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

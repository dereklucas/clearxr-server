use anyhow::Result;
use ash::{vk, vk::Handle, Device, Instance};
use openxr_sys as xr;

pub struct VkBackend {
    _entry: ash::Entry,
    instance: Instance,
    physical_device: vk::PhysicalDevice,
    device: Device,
    queue: vk::Queue,
    pub command_pool: vk::CommandPool,
}

impl VkBackend {
    pub unsafe fn from_graphics_binding(binding: &xr::GraphicsBindingVulkanKHR) -> Result<Self> {
        let entry = ash::Entry::load()?;
        let instance = ash::Instance::load(
            entry.static_fn(),
            vk::Instance::from_raw(binding.instance as usize as u64),
        );
        let physical_device =
            vk::PhysicalDevice::from_raw(binding.physical_device as usize as u64);
        let device = ash::Device::load(
            instance.fp_v1_0(),
            vk::Device::from_raw(binding.device as usize as u64),
        );
        let queue = device.get_device_queue(binding.queue_family_index, binding.queue_index);
        let command_pool = device.create_command_pool(
            &vk::CommandPoolCreateInfo {
                queue_family_index: binding.queue_family_index,
                flags: vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
                ..Default::default()
            },
            None,
        )?;

        Ok(Self {
            _entry: entry,
            instance,
            physical_device,
            device,
            queue,
            command_pool,
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn queue(&self) -> vk::Queue {
        self.queue
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

    pub unsafe fn destroy_command_pool(&self) {
        if self.command_pool != vk::CommandPool::null() {
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

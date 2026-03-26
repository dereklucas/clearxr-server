use anyhow::Result;
use ash::vk;

pub fn load_spv(device: &ash::Device, bytes: &[u8]) -> Result<vk::ShaderModule> {
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let ci = vk::ShaderModuleCreateInfo {
        code_size: words.len() * 4,
        p_code: words.as_ptr(),
        ..Default::default()
    };
    Ok(unsafe { device.create_shader_module(&ci, None)? })
}

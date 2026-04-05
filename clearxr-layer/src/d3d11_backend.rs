//! Minimal D3D11 backend for the dashboard overlay.
//!
//! Uses raw COM vtable calls — no dependency on the `windows` crate's
//! generated COM bindings.  Only the methods we actually call are defined.

use std::ffi::c_void;
use std::ptr;

/// Opaque COM interface pointers (matches openxr_sys::platform typedefs).
type ID3D11Device = c_void;
type ID3D11DeviceContext = c_void;
/// HRESULT as a raw i32.
type HRESULT = i32;

/// GUID layout for COM QueryInterface.
#[repr(C)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// IID_ID3D11Device1: {a04bfb29-08ef-43d6-a49c-a9bdbdcbe686}
const IID_ID3D11DEVICE1: GUID = GUID {
    data1: 0xa04bfb29,
    data2: 0x08ef,
    data3: 0x43d6,
    data4: [0xa4, 0x9c, 0xa9, 0xbd, 0xbd, 0xcb, 0xe6, 0x86],
};

/// IID_ID3D11Texture2D: {6f15aaf2-d208-4e89-9ab4-489535d34F9C}
const IID_ID3D11TEXTURE2D: GUID = GUID {
    data1: 0x6f15aaf2,
    data2: 0xd208,
    data3: 0x4e89,
    data4: [0x9a, 0xb4, 0x48, 0x95, 0x35, 0xd3, 0x4f, 0x9c],
};

// ── COM vtable offsets ──
// IUnknown: 0=QueryInterface, 1=AddRef, 2=Release
// ID3D11Device inherits IUnknown:
//   ...various methods...
//   slot 40 = GetImmediateContext (ID3D11Device vtable)
// ID3D11Device1 inherits ID3D11Device:
//   slot 43 = OpenSharedResource1 (ID3D11Device1 vtable)
// ID3D11DeviceContext inherits ID3D11DeviceChild (inherits ID3D11DeviceChild → IUnknown):
//   slot 47 = CopyResource (ID3D11DeviceContext vtable)

/// Typed wrapper around a raw `ID3D11Device*` from the game's graphics binding.
/// AddRef'd on construction, Release'd on drop.
pub struct D3D11Backend {
    device: *mut ID3D11Device,
    device1: *mut c_void, // ID3D11Device1* (QI'd from device)
    context: *mut ID3D11DeviceContext,
}

// Safety: The D3D11 device and context are thread-safe COM objects.
// They are only used from the xrEndFrame thread.
unsafe impl Send for D3D11Backend {}

/// Wrapper around an imported `ID3D11Texture2D*` shared texture.
pub struct SharedTexture {
    pub texture: *mut c_void, // ID3D11Texture2D*
}

impl Drop for SharedTexture {
    fn drop(&mut self) {
        if !self.texture.is_null() {
            unsafe { com_release(self.texture); }
        }
    }
}

impl D3D11Backend {
    /// Create from the game's D3D11 device pointer (from `XrGraphicsBindingD3D11KHR`).
    /// AddRefs the device so we can safely hold it across frames.
    pub unsafe fn from_device(device: *mut c_void) -> Result<Self, String> {
        if device.is_null() {
            return Err("D3D11 device pointer is null".into());
        }

        // AddRef the device so it stays alive while we hold it
        com_addref(device);

        // Get the immediate context
        let context = d3d11_get_immediate_context(device)?;

        // QI for ID3D11Device1 (needed for OpenSharedResource1)
        let device1 = com_query_interface(device, &IID_ID3D11DEVICE1)
            .map_err(|hr| format!("QueryInterface(ID3D11Device1) failed: HRESULT 0x{:08x}", hr))?;

        Ok(Self {
            device,
            device1,
            context,
        })
    }

    /// Import a shared texture via NT handle name (same named handle the dashboard exports).
    /// Uses `ID3D11Device1::OpenSharedResource1`.
    pub unsafe fn import_shared_texture(
        &self,
        handle_name: *const u16,
    ) -> Result<SharedTexture, String> {
        // OpenSharedResourceByName is ID3D11Device1 method at vtable slot 44
        // Actually, OpenSharedResource1 takes a HANDLE, not a name.
        // We need OpenSharedResourceByName for named resources.
        // ID3D11Device1 vtable:
        //   slot 43: GetImmediateContext1
        //   slot 44: CreateDeferredContext1
        //   slot 45: CreateBlendState1
        //   slot 46: CreateRasterizerState1
        //   slot 47: CreateDeviceContextState
        //   slot 48: OpenSharedResource1 (takes HANDLE)
        //   slot 49: OpenSharedResourceByName (takes LPCWSTR name, DWORD access, REFIID iid, void** out)

        // We use OpenSharedResourceByName (slot 49) with the named handle
        let vtable = *(self.device1 as *const *const *const c_void);
        let open_shared_by_name: unsafe extern "system" fn(
            *mut c_void,       // this
            *const u16,        // lpName (LPCWSTR)
            u32,               // dwDesiredAccess (DXGI_SHARED_RESOURCE_READ = 0x80000000)
            *const GUID,       // returnedInterface (IID)
            *mut *mut c_void,  // ppResource
        ) -> HRESULT = std::mem::transmute(*vtable.add(49));

        let mut texture: *mut c_void = ptr::null_mut();
        const DXGI_SHARED_RESOURCE_READ: u32 = 0x80000000;
        let hr = open_shared_by_name(
            self.device1,
            handle_name,
            DXGI_SHARED_RESOURCE_READ,
            &IID_ID3D11TEXTURE2D,
            &mut texture,
        );
        if hr < 0 || texture.is_null() {
            return Err(format!(
                "OpenSharedResourceByName failed: HRESULT 0x{:08x}",
                hr as u32
            ));
        }

        Ok(SharedTexture { texture })
    }

    /// Get the raw context pointer (for direct vtable calls in overlay_d3d11).
    pub fn context_ptr(&self) -> *mut c_void {
        self.context
    }

    /// Copy the shared texture onto the swapchain image.
    /// Both must be the same dimensions and compatible formats.
    pub unsafe fn copy_resource(
        &self,
        dst: *mut c_void, // ID3D11Texture2D* (swapchain image)
        src: *mut c_void, // ID3D11Texture2D* (shared texture)
    ) {
        // ID3D11DeviceContext::CopyResource is at vtable slot 47
        let vtable = *(self.context as *const *const *const c_void);
        let copy_resource: unsafe extern "system" fn(
            *mut c_void,       // this (context)
            *mut c_void,       // pDstResource
            *mut c_void,       // pSrcResource
        ) = std::mem::transmute(*vtable.add(47));

        copy_resource(self.context, dst, src);
    }
}

impl Drop for D3D11Backend {
    fn drop(&mut self) {
        unsafe {
            if !self.context.is_null() {
                com_release(self.context);
            }
            if !self.device1.is_null() {
                com_release(self.device1);
            }
            if !self.device.is_null() {
                com_release(self.device);
            }
        }
    }
}

// ── COM helpers ──

unsafe fn com_addref(obj: *mut c_void) -> u32 {
    let vtable = *(obj as *const *const *const c_void);
    let addref: unsafe extern "system" fn(*mut c_void) -> u32 =
        std::mem::transmute(*vtable.add(1));
    addref(obj)
}

unsafe fn com_release(obj: *mut c_void) -> u32 {
    let vtable = *(obj as *const *const *const c_void);
    let release: unsafe extern "system" fn(*mut c_void) -> u32 =
        std::mem::transmute(*vtable.add(2));
    release(obj)
}

unsafe fn com_query_interface(
    obj: *mut c_void,
    iid: &GUID,
) -> Result<*mut c_void, HRESULT> {
    let vtable = *(obj as *const *const *const c_void);
    let qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT =
        std::mem::transmute(*vtable.add(0));
    let mut out: *mut c_void = ptr::null_mut();
    let hr = qi(obj, iid, &mut out);
    if hr < 0 {
        Err(hr)
    } else {
        Ok(out)
    }
}

/// ID3D11Device::GetImmediateContext — vtable slot 40
unsafe fn d3d11_get_immediate_context(device: *mut c_void) -> Result<*mut ID3D11DeviceContext, String> {
    let vtable = *(device as *const *const *const c_void);
    let get_ctx: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) =
        std::mem::transmute(*vtable.add(40));
    let mut ctx: *mut c_void = ptr::null_mut();
    get_ctx(device, &mut ctx);
    if ctx.is_null() {
        return Err("GetImmediateContext returned null".into());
    }
    Ok(ctx)
}

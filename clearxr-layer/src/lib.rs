/// Clear XR API Layer — intercepts OpenXR action state queries and injects
/// controller touch/click events from the NV opaque data channel.
///
/// This allows any OpenXR app running on CloudXR to receive correct capacitive
/// touch and button click events from PS Sense controllers, working around
/// CloudXR bugs.

mod opaque;

use opaque::*;
use openxr_sys as xr;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr};
use std::sync::Mutex;

// ============================================================
// Layer-specific loader types (not in openxr-sys)
// ============================================================

const STRUCT_LOADER_INFO: u32 = 1;
const STRUCT_API_LAYER_REQUEST: u32 = 2;
const STRUCT_API_LAYER_CREATE_INFO: u32 = 4;
const STRUCT_API_LAYER_NEXT_INFO: u32 = 5;

const LOADER_INFO_VERSION: u32 = 1;
const API_LAYER_REQUEST_VERSION: u32 = 1;
const CURRENT_LOADER_LAYER_IFACE_VERSION: u32 = 1;

const MAX_LAYER_NAME: usize = 256;
const MAX_SETTINGS_PATH: usize = 512;

const LAYER_NAME: &str = "XR_APILAYER_CLEARXR_controller_fix";

#[cfg(windows)]
unsafe fn output_debug_string(message: &str) {
    unsafe extern "system" {
        fn OutputDebugStringA(lp_output_string: *const c_char);
    }

    let mut bytes: Vec<u8> = message
        .as_bytes()
        .iter()
        .copied()
        .filter(|b| *b != 0)
        .collect();
    bytes.push(b'\n');
    bytes.push(0);

    OutputDebugStringA(bytes.as_ptr() as *const c_char);
}

#[cfg(not(windows))]
unsafe fn output_debug_string(_message: &str) {}

pub(crate) fn debug_log(level: log::Level, message: &str) {
    log::log!(level, "{}", message);
    unsafe { output_debug_string(message); }
}

macro_rules! layer_log {
    (error, $($arg:tt)*) => {{
        let message = format!($($arg)*);
        crate::debug_log(log::Level::Error, &message);
    }};
    (warn, $($arg:tt)*) => {{
        let message = format!($($arg)*);
        crate::debug_log(log::Level::Warn, &message);
    }};
    (info, $($arg:tt)*) => {{
        let message = format!($($arg)*);
        crate::debug_log(log::Level::Info, &message);
    }};
}

#[repr(C)]
pub struct NegotiateLoaderInfo {
    struct_type: u32,
    struct_version: u32,
    struct_size: usize,
    min_interface_version: u32,
    max_interface_version: u32,
    min_api_version: xr::Version,
    max_api_version: xr::Version,
}

#[repr(C)]
pub struct NegotiateApiLayerRequest {
    struct_type: u32,
    struct_version: u32,
    struct_size: usize,
    layer_interface_version: u32,
    layer_api_version: xr::Version,
    get_instance_proc_addr: xr::pfn::GetInstanceProcAddr,
    create_api_layer_instance: CreateApiLayerInstanceFn,
}

type CreateApiLayerInstanceFn = unsafe extern "system" fn(
    *const xr::InstanceCreateInfo,
    *const ApiLayerCreateInfo,
    *mut xr::Instance,
) -> xr::Result;

#[repr(C)]
struct ApiLayerCreateInfo {
    struct_type: u32,
    struct_version: u32,
    struct_size: usize,
    loader_instance: *mut c_void,
    settings_file_location: [c_char; MAX_SETTINGS_PATH],
    next_info: *const ApiLayerNextInfo,
}

#[repr(C)]
struct ApiLayerNextInfo {
    struct_type: u32,
    struct_version: u32,
    struct_size: usize,
    layer_name: [c_char; MAX_LAYER_NAME],
    next_get_instance_proc_addr: xr::pfn::GetInstanceProcAddr,
    next_create_api_layer_instance: CreateApiLayerInstanceFn,
    next: *const ApiLayerNextInfo,
}

// ============================================================
// Dispatch table — "next" function pointers
// ============================================================

struct NextDispatch {
    get_instance_proc_addr: xr::pfn::GetInstanceProcAddr,
    destroy_instance: xr::pfn::DestroyInstance,
    get_system: xr::pfn::GetSystem,
    create_session: xr::pfn::CreateSession,
    destroy_session: xr::pfn::DestroySession,
    suggest_interaction_profile_bindings: xr::pfn::SuggestInteractionProfileBindings,
    sync_actions: xr::pfn::SyncActions,
    get_action_state_boolean: xr::pfn::GetActionStateBoolean,
    apply_haptic_feedback: xr::pfn::ApplyHapticFeedback,
    stop_haptic_feedback: xr::pfn::StopHapticFeedback,
    path_to_string: xr::pfn::PathToString,
    string_to_path: xr::pfn::StringToPath,
}

// ============================================================
// Binding map: which (action, hand) → opaque channel bit?
// ============================================================

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Hand { Left, Right }

/// For a given (action, hand), which bit in the SpatialControllerPacket should
/// override the boolean state?
#[derive(Clone, Copy)]
struct OverrideInfo {
    hand: Hand,
    bit_mask: u16,
}

/// Map component path suffix → bit mask for the opaque channel packet.
fn component_to_bit(component: &str) -> Option<u16> {
    match component {
        "/input/x/touch" | "/input/a/touch" => Some(SC_TOUCH_A),
        "/input/y/touch" | "/input/b/touch" => Some(SC_TOUCH_B),
        "/input/x/click" | "/input/a/click" => Some(SC_BTN_A),
        "/input/y/click" | "/input/b/click" => Some(SC_BTN_B),
        "/input/trigger/touch"              => Some(SC_TOUCH_TRIGGER),
        "/input/thumbstick/touch"           => Some(SC_TOUCH_THUMBSTICK),
        "/input/thumbstick/click"           => Some(SC_BTN_THUMBSTICK),
        "/input/menu/click"                 => Some(SC_BTN_MENU),
        _ => None,
    }
}

/// Parse "/user/hand/left/input/x/touch" → (Hand::Left, "/input/x/touch")
fn parse_binding_path(path: &str) -> Option<(Hand, &str)> {
    if let Some(rest) = path.strip_prefix("/user/hand/left") {
        Some((Hand::Left, rest))
    } else if let Some(rest) = path.strip_prefix("/user/hand/right") {
        Some((Hand::Right, rest))
    } else {
        None
    }
}

fn is_haptic_output_path(component: &str) -> bool {
    component == "/output/haptic"
}

// ============================================================
// Global layer state
// ============================================================

struct LayerState {
    instance: xr::Instance,
    next: NextDispatch,
    system_id: u64,
    opaque: Option<OpaqueChannel>,
    /// (action_handle_raw, hand) → bit_mask to override
    overrides: HashMap<(u64, Hand), u16>,
    haptic_actions: HashSet<(u64, Hand)>,
    /// Has the opaque channel extension been enabled?
    has_opaque_ext: bool,
}

static LAYER: Mutex<Option<LayerState>> = Mutex::new(None);

// Store the next layer's getInstanceProcAddr for use during instance creation
static NEXT_GPA: Mutex<Option<xr::pfn::GetInstanceProcAddr>> = Mutex::new(None);
static NEXT_CREATE: Mutex<Option<CreateApiLayerInstanceFn>> = Mutex::new(None);

// ============================================================
// DLL export: xrNegotiateLoaderApiLayerInterface
// ============================================================

#[no_mangle]
pub unsafe extern "system" fn xrNegotiateLoaderApiLayerInterface(
    loader_info: *const NegotiateLoaderInfo,
    _layer_name: *const c_char,
    request: *mut NegotiateApiLayerRequest,
) -> xr::Result {
    // Initialize logging — write to both stderr and a file for easy debugging.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .format_timestamp_millis()
    .try_init();

    // Also log to a file via OutputDebugString (visible in DebugView/VS Output)
    layer_log!(info, "[ClearXR Layer] xrNegotiateLoaderApiLayerInterface called.");

    if loader_info.is_null() || request.is_null() {
        layer_log!(error, "[ClearXR Layer] Negotiation received null pointers.");
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    let info = &*loader_info;
    if info.struct_type != STRUCT_LOADER_INFO
        || info.struct_version != LOADER_INFO_VERSION
        || info.struct_size != std::mem::size_of::<NegotiateLoaderInfo>()
    {
        layer_log!(
            error,
            "[ClearXR Layer] Loader info mismatch: type={} version={} size={} expected_size={}",
            info.struct_type,
            info.struct_version,
            info.struct_size,
            std::mem::size_of::<NegotiateLoaderInfo>()
        );
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    let req = &mut *request;
    if req.struct_type != STRUCT_API_LAYER_REQUEST
        || req.struct_version != API_LAYER_REQUEST_VERSION
        || req.struct_size != std::mem::size_of::<NegotiateApiLayerRequest>()
    {
        layer_log!(
            error,
            "[ClearXR Layer] API layer request mismatch: type={} version={} size={} expected_size={}",
            req.struct_type,
            req.struct_version,
            req.struct_size,
            std::mem::size_of::<NegotiateApiLayerRequest>()
        );
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    // Verify interface version is compatible
    if CURRENT_LOADER_LAYER_IFACE_VERSION < info.min_interface_version
        || CURRENT_LOADER_LAYER_IFACE_VERSION > info.max_interface_version
    {
        layer_log!(
            error,
            "[ClearXR Layer] Interface version {} outside loader range {}..={}.",
            CURRENT_LOADER_LAYER_IFACE_VERSION,
            info.min_interface_version,
            info.max_interface_version
        );
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    req.layer_interface_version = CURRENT_LOADER_LAYER_IFACE_VERSION;
    req.layer_api_version = xr::Version::new(1, 0, 0);
    req.get_instance_proc_addr = layer_get_instance_proc_addr;
    req.create_api_layer_instance = layer_create_api_layer_instance;

    layer_log!(info, "[ClearXR Layer] Negotiated successfully.");
    xr::Result::SUCCESS
}

// ============================================================
// xrCreateApiLayerInstance — called by the loader to create instance through this layer
// ============================================================

unsafe extern "system" fn layer_create_api_layer_instance(
    ci: *const xr::InstanceCreateInfo,
    layer_ci: *const ApiLayerCreateInfo,
    instance_out: *mut xr::Instance,
) -> xr::Result {
    layer_log!(info, "[ClearXR Layer] layer_create_api_layer_instance called.");

    if ci.is_null() || layer_ci.is_null() || instance_out.is_null() {
        layer_log!(error, "[ClearXR Layer] Null pointer passed to create instance (ci={} layer_ci={} out={})",
            !ci.is_null(), !layer_ci.is_null(), !instance_out.is_null());
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    let layer_info = &*layer_ci;
    layer_log!(info, "[ClearXR Layer] layer_ci struct_type={}, struct_version={}, struct_size={}",
        layer_info.struct_type, layer_info.struct_version, layer_info.struct_size);

    if layer_info.struct_type != STRUCT_API_LAYER_CREATE_INFO
        || layer_info.struct_version != LOADER_INFO_VERSION
        || layer_info.struct_size != std::mem::size_of::<ApiLayerCreateInfo>()
    {
        layer_log!(
            error,
            "[ClearXR Layer] Unexpected create info: type={} version={} size={} expected_type={} expected_version={} expected_size={}",
            layer_info.struct_type,
            layer_info.struct_version,
            layer_info.struct_size,
            STRUCT_API_LAYER_CREATE_INFO,
            LOADER_INFO_VERSION,
            std::mem::size_of::<ApiLayerCreateInfo>()
        );
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }

    // Log the app's requested extensions
    {
        let orig_ci = &*ci;
        let count = orig_ci.enabled_extension_count as usize;
        layer_log!(info, "[ClearXR Layer] App requests {} extensions:", count);
        for i in 0..count {
            let ext = CStr::from_ptr(*orig_ci.enabled_extension_names.add(i));
            layer_log!(info, "[ClearXR Layer]   - {:?}", ext);
        }
    }

    // Walk the next_info chain to find our layer's entry
    let mut next_info = layer_info.next_info;
    let mut chain_idx = 0;
    while !next_info.is_null() {
        let ni = &*next_info;
        let ni_name = CStr::from_ptr(ni.layer_name.as_ptr());
        layer_log!(info, "[ClearXR Layer] Chain[{}]: struct_type={}, name={:?}",
            chain_idx, ni.struct_type, ni_name);

        if ni.struct_type != STRUCT_API_LAYER_NEXT_INFO {
            layer_log!(warn, "[ClearXR Layer] Unexpected struct_type in chain, stopping walk.");
            break;
        }

        if ni_name.to_bytes() == LAYER_NAME.as_bytes() {
            layer_log!(info, "[ClearXR Layer] Found our entry in the chain.");
            let next_gpa = ni.next_get_instance_proc_addr;
            let next_create = ni.next_create_api_layer_instance;

            // Store for later use
            *NEXT_GPA.lock().unwrap() = Some(next_gpa);
            *NEXT_CREATE.lock().unwrap() = Some(next_create);

            // Check if opaque data channel extension is available
            layer_log!(info, "[ClearXR Layer] Checking for opaque data channel extension...");
            let has_nvx1 = check_extension_available(next_gpa, "XR_NVX1_opaque_data_channel");
            let has_nv = check_extension_available(next_gpa, "XR_NV_opaque_data_channel");
            let has_opaque = has_nvx1 || has_nv;
            layer_log!(info, "[ClearXR Layer] Extension check: NVX1={}, NV={}", has_nvx1, has_nv);

            // Build the next layer's ApiLayerCreateInfo (shared for both paths)
            let next_layer_ci = ApiLayerCreateInfo {
                struct_type: STRUCT_API_LAYER_CREATE_INFO,
                struct_version: layer_info.struct_version,
                struct_size: layer_info.struct_size,
                loader_instance: layer_info.loader_instance,
                settings_file_location: layer_info.settings_file_location,
                next_info: ni.next,
            };

            let result = if has_opaque {
                let ext_name: &[u8] = if has_nvx1 {
                    b"XR_NVX1_opaque_data_channel\0"
                } else {
                    b"XR_NV_opaque_data_channel\0"
                };

                let orig_ci = &*ci;
                let orig_count = orig_ci.enabled_extension_count as usize;
                let mut ext_ptrs: Vec<*const c_char> = Vec::with_capacity(orig_count + 1);
                for i in 0..orig_count {
                    ext_ptrs.push(*orig_ci.enabled_extension_names.add(i));
                }
                ext_ptrs.push(ext_name.as_ptr() as *const c_char);

                let mut modified_ci = *ci;
                modified_ci.enabled_extension_count = ext_ptrs.len() as u32;
                modified_ci.enabled_extension_names = ext_ptrs.as_ptr();

                layer_log!(info, "[ClearXR Layer] Calling next_create with {} extensions (added opaque channel).",
                    ext_ptrs.len());
                (next_create)(&modified_ci, &next_layer_ci, instance_out)
            } else {
                layer_log!(info, "[ClearXR Layer] No opaque channel ext, calling next_create with original {} extensions.",
                    (*ci).enabled_extension_count);
                (next_create)(ci, &next_layer_ci, instance_out)
            };

            if result != xr::Result::SUCCESS {
                layer_log!(warn, "[ClearXR Layer] Next layer create instance failed: {:?}", result);
                return result;
            }

            let instance = *instance_out;

            // Build dispatch table
            let dispatch = build_dispatch(next_gpa, instance);

            // Store layer state
            *LAYER.lock().unwrap() = Some(LayerState {
                instance,
                next: dispatch,
                system_id: 0,
                opaque: None,
                overrides: HashMap::new(),
                haptic_actions: HashSet::new(),
                has_opaque_ext: has_opaque,
            });

            layer_log!(info, "[ClearXR Layer] Instance created, layer active.");
            return xr::Result::SUCCESS;
        }

        chain_idx += 1;
        next_info = ni.next;
    }

    layer_log!(error, "[ClearXR Layer] Could not find '{}' in chain after {} entries.", LAYER_NAME, chain_idx);
    xr::Result::ERROR_INITIALIZATION_FAILED
}

/// Check if an extension is available by calling xrEnumerateInstanceExtensionProperties
/// through the next layer (with NULL instance).
unsafe fn check_extension_available(
    next_gpa: xr::pfn::GetInstanceProcAddr,
    ext_name: &str,
) -> bool {
    let mut fp: Option<xr::pfn::VoidFunction> = None;
    let r = (next_gpa)(
        xr::Instance::NULL,
        b"xrEnumerateInstanceExtensionProperties\0".as_ptr() as *const c_char,
        &mut fp,
    );
    let enumerate: xr::pfn::EnumerateInstanceExtensionProperties = match fp {
        Some(f) => std::mem::transmute(f),
        None => {
            layer_log!(warn, "[ClearXR Layer] Could not load xrEnumerateInstanceExtensionProperties (result={:?})", r);
            return false;
        }
    };

    let mut count: u32 = 0;
    let r = (enumerate)(std::ptr::null(), 0, &mut count, std::ptr::null_mut());
    if r != xr::Result::SUCCESS || count == 0 {
        layer_log!(warn, "[ClearXR Layer] EnumerateExtensions(count) failed: {:?}, count={}", r, count);
        return false;
    }

    let mut props: Vec<xr::ExtensionProperties> = vec![
        xr::ExtensionProperties {
            ty: xr::ExtensionProperties::TYPE,
            next: std::ptr::null_mut(),
            extension_name: [0; 128],
            extension_version: 0,
        };
        count as usize
    ];
    let r = (enumerate)(std::ptr::null(), count, &mut count, props.as_mut_ptr());
    if r != xr::Result::SUCCESS {
        layer_log!(warn, "[ClearXR Layer] EnumerateExtensions(props) failed: {:?}", r);
        return false;
    }

    for p in &props {
        let name = CStr::from_ptr(p.extension_name.as_ptr());
        if let Ok(s) = name.to_str() {
            if s == ext_name {
                return true;
            }
        }
    }
    false
}

/// Load a function pointer from the next layer's dispatch.
unsafe fn load_fn<T>(
    gpa: xr::pfn::GetInstanceProcAddr,
    instance: xr::Instance,
    name: &[u8], // null-terminated
) -> T {
    let mut fp: Option<xr::pfn::VoidFunction> = None;
    (gpa)(instance, name.as_ptr() as *const c_char, &mut fp);
    std::mem::transmute_copy(&fp.expect(&format!(
        "Failed to load {}",
        std::str::from_utf8(&name[..name.len() - 1]).unwrap_or("?")
    )))
}

unsafe fn build_dispatch(gpa: xr::pfn::GetInstanceProcAddr, instance: xr::Instance) -> NextDispatch {
    NextDispatch {
        get_instance_proc_addr: gpa,
        destroy_instance: load_fn(gpa, instance, b"xrDestroyInstance\0"),
        get_system: load_fn(gpa, instance, b"xrGetSystem\0"),
        create_session: load_fn(gpa, instance, b"xrCreateSession\0"),
        destroy_session: load_fn(gpa, instance, b"xrDestroySession\0"),
        suggest_interaction_profile_bindings: load_fn(gpa, instance, b"xrSuggestInteractionProfileBindings\0"),
        sync_actions: load_fn(gpa, instance, b"xrSyncActions\0"),
        get_action_state_boolean: load_fn(gpa, instance, b"xrGetActionStateBoolean\0"),
        apply_haptic_feedback: load_fn(gpa, instance, b"xrApplyHapticFeedback\0"),
        stop_haptic_feedback: load_fn(gpa, instance, b"xrStopHapticFeedback\0"),
        path_to_string: load_fn(gpa, instance, b"xrPathToString\0"),
        string_to_path: load_fn(gpa, instance, b"xrStringToPath\0"),
    }
}

// ============================================================
// xrGetInstanceProcAddr — dispatch to our hooks or pass through
// ============================================================

unsafe extern "system" fn layer_get_instance_proc_addr(
    instance: xr::Instance,
    name: *const c_char,
    function: *mut Option<xr::pfn::VoidFunction>,
) -> xr::Result {
    if name.is_null() || function.is_null() {
        return xr::Result::ERROR_VALIDATION_FAILURE;
    }

    let name_str = CStr::from_ptr(name);

    // Return our intercepted functions
    macro_rules! intercept {
        ($fn_name:expr, $fn_ptr:expr) => {
            if name_str.to_bytes() == $fn_name {
                *function = Some(std::mem::transmute($fn_ptr as *const ()));
                return xr::Result::SUCCESS;
            }
        };
    }

    intercept!(b"xrGetInstanceProcAddr", layer_get_instance_proc_addr as xr::pfn::GetInstanceProcAddr);
    intercept!(b"xrDestroyInstance", hook_destroy_instance as xr::pfn::DestroyInstance);
    intercept!(b"xrGetSystem", hook_get_system as xr::pfn::GetSystem);
    intercept!(b"xrCreateSession", hook_create_session as xr::pfn::CreateSession);
    intercept!(b"xrDestroySession", hook_destroy_session as xr::pfn::DestroySession);
    intercept!(b"xrSuggestInteractionProfileBindings", hook_suggest_bindings as xr::pfn::SuggestInteractionProfileBindings);
    intercept!(b"xrSyncActions", hook_sync_actions as xr::pfn::SyncActions);
    intercept!(b"xrGetActionStateBoolean", hook_get_action_state_boolean as xr::pfn::GetActionStateBoolean);
    intercept!(b"xrApplyHapticFeedback", hook_apply_haptic_feedback as xr::pfn::ApplyHapticFeedback);
    intercept!(b"xrStopHapticFeedback", hook_stop_haptic_feedback as xr::pfn::StopHapticFeedback);

    // Pass through to next layer
    let guard = LAYER.lock().unwrap();
    if let Some(ref state) = *guard {
        return (state.next.get_instance_proc_addr)(instance, name, function);
    }

    // During negotiation, before instance exists, try stored next_gpa
    drop(guard);
    if let Some(next_gpa) = *NEXT_GPA.lock().unwrap() {
        return (next_gpa)(instance, name, function);
    }

    xr::Result::ERROR_HANDLE_INVALID
}

unsafe fn hand_from_path(state: &LayerState, path: xr::Path) -> Option<Hand> {
    if path == xr::Path::NULL {
        return None;
    }

    let mut buf = [0u8; 256];
    let mut len: u32 = 0;
    let r = (state.next.path_to_string)(
        state.instance,
        path,
        buf.len() as u32,
        &mut len,
        buf.as_mut_ptr() as *mut c_char,
    );
    if r != xr::Result::SUCCESS || len == 0 {
        return None;
    }

    let path_str = std::str::from_utf8(&buf[..len as usize - 1]).ok()?;
    if path_str.contains("left") {
        Some(Hand::Left)
    } else if path_str.contains("right") {
        Some(Hand::Right)
    } else {
        None
    }
}

// ============================================================
// Intercepted functions
// ============================================================

unsafe extern "system" fn hook_destroy_instance(instance: xr::Instance) -> xr::Result {
    let next_fn;
    {
        let guard = LAYER.lock().unwrap();
        next_fn = guard.as_ref().map(|s| s.next.destroy_instance);
    }
    let result = if let Some(f) = next_fn {
        (f)(instance)
    } else {
        xr::Result::ERROR_HANDLE_INVALID
    };

    // Clean up layer state
    *LAYER.lock().unwrap() = None;
    log::info!("[ClearXR Layer] Instance destroyed, layer cleaned up.");
    result
}

unsafe extern "system" fn hook_get_system(
    instance: xr::Instance,
    get_info: *const xr::SystemGetInfo,
    system_id: *mut xr::SystemId,
) -> xr::Result {
    let mut guard = LAYER.lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    let result = (state.next.get_system)(instance, get_info, system_id);
    if result == xr::Result::SUCCESS {
        state.system_id = (*system_id).into_raw();
        log::info!("[ClearXR Layer] Got system_id: {}", state.system_id);

        // Now create the opaque channel if extension was enabled
        if state.has_opaque_ext && state.opaque.is_none() {
            if let Some(mut ch) = OpaqueChannel::load(
                state.next.get_instance_proc_addr,
                instance,
                state.system_id,
            ) {
                if ch.create_channel() {
                    state.opaque = Some(ch);
                } else {
                    log::warn!("[ClearXR Layer] Opaque channel create failed.");
                }
            } else {
                log::warn!("[ClearXR Layer] Opaque channel functions not available.");
            }
        }
    }
    result
}

unsafe extern "system" fn hook_create_session(
    instance: xr::Instance,
    ci: *const xr::SessionCreateInfo,
    session: *mut xr::Session,
) -> xr::Result {
    let guard = LAYER.lock().unwrap();
    let state = match guard.as_ref() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };
    (state.next.create_session)(instance, ci, session)
}

unsafe extern "system" fn hook_destroy_session(session: xr::Session) -> xr::Result {
    let guard = LAYER.lock().unwrap();
    let state = match guard.as_ref() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };
    (state.next.destroy_session)(session)
}

unsafe extern "system" fn hook_suggest_bindings(
    instance: xr::Instance,
    suggested_bindings: *const xr::InteractionProfileSuggestedBinding,
) -> xr::Result {
    let mut guard = LAYER.lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    // Record the bindings so we know which actions to override later
    let sb = &*suggested_bindings;
    let bindings = std::slice::from_raw_parts(sb.suggested_bindings, sb.count_suggested_bindings as usize);

    for binding in bindings {
        // Convert the XrPath to a string
        let mut buf = [0u8; 512];
        let mut len: u32 = 0;
        let r = (state.next.path_to_string)(
            instance,
            binding.binding,
            buf.len() as u32,
            &mut len,
            buf.as_mut_ptr() as *mut c_char,
        );
        if r != xr::Result::SUCCESS || len == 0 { continue; }

        let path_str = match std::str::from_utf8(&buf[..len as usize - 1]) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if let Some((hand, component)) = parse_binding_path(path_str) {
            let action_raw = binding.action.into_raw();
            if let Some(bit) = component_to_bit(component) {
                log::info!(
                    "[ClearXR Layer] Recorded binding: action 0x{:x} {:?} {} → bit 0x{:x}",
                    action_raw, hand, path_str, bit
                );
                state.overrides.insert((action_raw, hand), bit);
            }

            if is_haptic_output_path(component) {
                layer_log!(
                    info,
                    "[ClearXR Layer] Recorded haptic binding: action 0x{:x} {:?} {}",
                    action_raw,
                    hand,
                    path_str
                );
                state.haptic_actions.insert((action_raw, hand));
            }
        }
    }

    // Pass through to next layer
    (state.next.suggest_interaction_profile_bindings)(instance, suggested_bindings)
}

unsafe extern "system" fn hook_sync_actions(
    session: xr::Session,
    sync_info: *const xr::ActionsSyncInfo,
) -> xr::Result {
    let mut guard = LAYER.lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    // Pass through first
    let result = (state.next.sync_actions)(session, sync_info);

    // Then poll the opaque channel
    if let Some(ref mut ch) = state.opaque {
        ch.poll();
    }

    result
}

unsafe extern "system" fn hook_apply_haptic_feedback(
    session: xr::Session,
    haptic_action_info: *const xr::HapticActionInfo,
    haptic_feedback: *const xr::HapticBaseHeader,
) -> xr::Result {
    let mut guard = LAYER.lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    let result = (state.next.apply_haptic_feedback)(session, haptic_action_info, haptic_feedback);
    if result != xr::Result::SUCCESS || haptic_action_info.is_null() || haptic_feedback.is_null() {
        return result;
    }

    let info = &*haptic_action_info;
    let feedback = &*haptic_feedback;
    if feedback.ty != xr::HapticVibration::TYPE {
        layer_log!(
            warn,
            "[ClearXR Layer] Unsupported haptic type {:?}; only vibration packets are forwarded.",
            feedback.ty
        );
        return result;
    }

    let vibration = &*(haptic_feedback as *const xr::HapticVibration);
    let duration_ns = if vibration.duration.as_nanos() < 0 {
        0
    } else {
        vibration.duration.as_nanos() as u64
    };
    let action_raw = info.action.into_raw();

    let mut hands = Vec::with_capacity(2);
    if let Some(hand) = hand_from_path(state, info.subaction_path) {
        hands.push(hand);
    } else {
        if state.haptic_actions.contains(&(action_raw, Hand::Left)) {
            hands.push(Hand::Left);
        }
        if state.haptic_actions.contains(&(action_raw, Hand::Right)) {
            hands.push(Hand::Right);
        }
    }

    if hands.is_empty() {
        layer_log!(
            warn,
            "[ClearXR Layer] No hand mapping found for haptic action 0x{:x}.",
            action_raw
        );
        return result;
    }

    if let Some(ref mut ch) = state.opaque {
        for hand in hands {
            let hand_idx = match hand {
                Hand::Left => 0,
                Hand::Right => 1,
            };
            let sent = ch.send_haptic(hand_idx, duration_ns, vibration.frequency, vibration.amplitude);
            layer_log!(
                info,
                "[ClearXR Layer] Apply haptic action=0x{:x} hand={:?} duration_ns={} frequency={} amplitude={} sent={}",
                action_raw,
                hand,
                duration_ns,
                vibration.frequency,
                vibration.amplitude,
                sent
            );
        }
    } else {
        layer_log!(warn, "[ClearXR Layer] Haptic request received before opaque channel was ready.");
    }

    result
}

unsafe extern "system" fn hook_stop_haptic_feedback(
    session: xr::Session,
    haptic_action_info: *const xr::HapticActionInfo,
) -> xr::Result {
    let mut guard = LAYER.lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    let result = (state.next.stop_haptic_feedback)(session, haptic_action_info);
    if result != xr::Result::SUCCESS || haptic_action_info.is_null() {
        return result;
    }

    let info = &*haptic_action_info;
    let action_raw = info.action.into_raw();

    let mut hands = Vec::with_capacity(2);
    if let Some(hand) = hand_from_path(state, info.subaction_path) {
        hands.push(hand);
    } else {
        if state.haptic_actions.contains(&(action_raw, Hand::Left)) {
            hands.push(Hand::Left);
        }
        if state.haptic_actions.contains(&(action_raw, Hand::Right)) {
            hands.push(Hand::Right);
        }
    }

    if let Some(ref mut ch) = state.opaque {
        for hand in hands {
            let hand_idx = match hand {
                Hand::Left => 0,
                Hand::Right => 1,
            };
            let sent = ch.send_haptic(hand_idx, 0, 0.0, 0.0);
            layer_log!(
                info,
                "[ClearXR Layer] Stop haptic action=0x{:x} hand={:?} sent={}",
                action_raw,
                hand,
                sent
            );
        }
    }

    result
}

unsafe extern "system" fn hook_get_action_state_boolean(
    session: xr::Session,
    get_info: *const xr::ActionStateGetInfo,
    state_out: *mut xr::ActionStateBoolean,
) -> xr::Result {
    let mut guard = LAYER.lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    // Call the next layer first to get the base result
    let result = (state.next.get_action_state_boolean)(session, get_info, state_out);
    if result != xr::Result::SUCCESS { return result; }

    // Check if we have opaque channel data to override with
    let pkt = match state.opaque.as_ref().and_then(|ch| ch.latest) {
        Some(p) => p,
        None => return result,
    };

    let info = &*get_info;
    let action_raw = info.action.into_raw();

    // Determine which hand the subaction path refers to
    let hand = if info.subaction_path == xr::Path::NULL {
        // No subaction path — check both hands. Prefer left (arbitrary).
        if state.overrides.contains_key(&(action_raw, Hand::Left)) {
            Hand::Left
        } else if state.overrides.contains_key(&(action_raw, Hand::Right)) {
            Hand::Right
        } else {
            return result;
        }
    } else {
        // Resolve subaction path to hand
        let mut buf = [0u8; 256];
        let mut len: u32 = 0;
        let r = (state.next.path_to_string)(
            state.instance,
            info.subaction_path,
            buf.len() as u32,
            &mut len,
            buf.as_mut_ptr() as *mut c_char,
        );
        if r != xr::Result::SUCCESS { return result; }
        let path_str = std::str::from_utf8(&buf[..len as usize - 1]).unwrap_or("");
        if path_str.contains("left") {
            Hand::Left
        } else if path_str.contains("right") {
            Hand::Right
        } else {
            return result;
        }
    };

    // Look up the override
    if let Some(&bit) = state.overrides.get(&(action_raw, hand)) {
        let buttons = match hand {
            Hand::Left => {
                if pkt.active_hands & 0x01 != 0 { pkt.left.buttons } else { return result; }
            }
            Hand::Right => {
                if pkt.active_hands & 0x02 != 0 { pkt.right.buttons } else { return result; }
            }
        };

        let pressed = buttons & bit != 0;
        let out = &mut *state_out;
        out.current_state = if pressed { xr::TRUE } else { xr::FALSE };
        out.is_active = xr::TRUE;
        // changed_since_last_sync is left as the runtime reported it
    }

    result
}

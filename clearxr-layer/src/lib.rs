/// Clear XR API Layer — intercepts OpenXR action state queries and injects
/// controller touch/click events from the NV opaque data channel.
///
/// This allows any OpenXR app running on CloudXR to receive correct capacitive
/// touch and button click events from PS Sense controllers, working around
/// CloudXR bugs.

mod opaque;
mod vk_backend;

use opaque::*;
use overlay::*;
use openxr_sys as xr;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr};
use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

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
const BUILD_MARKER: &str = "OVERLAY_TRACE_BUILD_2026-03-25_00-05_ET";

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

fn layer_log_file() -> &'static Mutex<Option<File>> {
    static FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
    FILE.get_or_init(|| Mutex::new(open_layer_log_file()))
}

fn trace_log_file() -> &'static Mutex<Option<File>> {
    static FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
    FILE.get_or_init(|| Mutex::new(open_trace_log_file()))
}

fn open_layer_log_file() -> Option<File> {
    let base_dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
    let log_dir = base_dir.join("ClearXR").join("logs");
    create_dir_all(&log_dir).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("clearxr-layer.log"))
        .ok()
}

fn open_trace_log_file() -> Option<File> {
    let log_dir = PathBuf::from(r"C:\Apps\clearxr-server\clearxr-layer\target-local");
    create_dir_all(&log_dir).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("clearxr-layer-trace.log"))
        .ok()
}

fn direct_trace(message: &str) {
    if let Ok(mut file_guard) = trace_log_file().lock() {
        if let Some(file) = file_guard.as_mut() {
            let _ = writeln!(file, "{}", message);
            let _ = file.flush();
        }
    }
}

pub(crate) fn debug_log(level: log::Level, message: &str) {
    log::log!(level, "{}", message);
    if let Ok(mut file_guard) = layer_log_file().lock() {
        if let Some(file) = file_guard.as_mut() {
            let _ = writeln!(file, "[{}] {}", level, message);
            let _ = file.flush();
        }
    }
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

mod overlay;

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

#[derive(Clone, Copy)]
struct NextDispatch {
    get_instance_proc_addr: xr::pfn::GetInstanceProcAddr,
    destroy_instance: xr::pfn::DestroyInstance,
    get_system: xr::pfn::GetSystem,
    get_system_properties: xr::pfn::GetSystemProperties,
    create_session: xr::pfn::CreateSession,
    destroy_session: xr::pfn::DestroySession,
    end_frame: xr::pfn::EndFrame,
    create_reference_space: xr::pfn::CreateReferenceSpace,
    destroy_space: xr::pfn::DestroySpace,
    enumerate_swapchain_formats: xr::pfn::EnumerateSwapchainFormats,
    create_swapchain: xr::pfn::CreateSwapchain,
    destroy_swapchain: xr::pfn::DestroySwapchain,
    enumerate_swapchain_images: xr::pfn::EnumerateSwapchainImages,
    acquire_swapchain_image: xr::pfn::AcquireSwapchainImage,
    wait_swapchain_image: xr::pfn::WaitSwapchainImage,
    release_swapchain_image: xr::pfn::ReleaseSwapchainImage,
    suggest_interaction_profile_bindings: xr::pfn::SuggestInteractionProfileBindings,
    get_current_interaction_profile: xr::pfn::GetCurrentInteractionProfile,
    sync_actions: xr::pfn::SyncActions,
    get_action_state_boolean: xr::pfn::GetActionStateBoolean,
    apply_haptic_feedback: xr::pfn::ApplyHapticFeedback,
    stop_haptic_feedback: xr::pfn::StopHapticFeedback,
    path_to_string: xr::pfn::PathToString,
    string_to_path: xr::pfn::StringToPath,
    create_action_space: xr::pfn::CreateActionSpace,
    locate_space: xr::pfn::LocateSpace,
    get_action_state_float: xr::pfn::GetActionStateFloat,
    get_action_state_vector2f: xr::pfn::GetActionStateVector2f,
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
    oculus_touch_profile: xr::Path,
    overlay: Option<DashboardOverlay>,
    /// Action handles that are aim pose actions (path contains "/aim/pose")
    aim_actions: HashSet<u64>,
    /// Action handles that are trigger float actions (path contains "/trigger/value")
    trigger_actions: HashMap<(u64, Hand), ()>,
    /// Action handles that are squeeze float actions (path contains "/squeeze/value")
    squeeze_actions: HashMap<(u64, Hand), ()>,
    /// Space handle → (hand) for aim spaces created via xrCreateActionSpace
    aim_spaces: HashMap<u64, Hand>,
    /// Per-hand controller state captured from xrLocateSpace / xrGetActionStateFloat
    controller_state: [ControllerHandState; 2], // 0=left, 1=right
    /// Subaction paths for left/right hands
    left_hand_path: xr::Path,
    right_hand_path: xr::Path,
    /// Session handle (needed for active float queries)
    session: xr::Session,
}

/// Captured controller state for one hand.
#[derive(Default, Clone, Copy)]
struct ControllerHandState {
    aim_pos: [f32; 3],
    aim_orient: [f32; 4], // quaternion xyzw
    trigger: f32,
    squeeze: f32,
    thumbstick_x: f32,
    thumbstick_y: f32,
    active: bool,
}

static LAYER: Mutex<Option<LayerState>> = Mutex::new(None);

// Store the next layer's getInstanceProcAddr for use during instance creation
static NEXT_GPA: Mutex<Option<xr::pfn::GetInstanceProcAddr>> = Mutex::new(None);
static NEXT_CREATE: Mutex<Option<CreateApiLayerInstanceFn>> = Mutex::new(None);

// Next-layer dispatch table — set once at instance creation, read from every hook
// without locking LAYER. This is the key to avoiding mutex contention.
static NEXT: OnceLock<NextDispatch> = OnceLock::new();

/// Determine hand from a subaction path using the stored path values (no mutex needed).
fn hand_from_subaction_path(subaction: xr::Path) -> Option<Hand> {
    if subaction == xr::Path::NULL { return None; }
    let raw = subaction.into_raw();
    let left = LEFT_HAND_PATH.load(std::sync::atomic::Ordering::Relaxed);
    let right = RIGHT_HAND_PATH.load(std::sync::atomic::Ordering::Relaxed);
    if raw == left { Some(Hand::Left) }
    else if raw == right { Some(Hand::Right) }
    else { None }
}

// Hot-path state for xrLocateSpace / xrGetActionStateFloat hooks.
// These are called dozens of times per frame — must be lock-free or near-lock-free.
// Function pointers are write-once — use OnceLock (already imported above).
// Maps and controller state use RwLock (readers don't block each other).
use std::sync::RwLock;

static CONTROLLER_STATE: RwLock<[ControllerHandState; 2]> = RwLock::new([
    ControllerHandState { aim_pos: [0.0; 3], aim_orient: [0.0, 0.0, 0.0, 1.0], trigger: 0.0, squeeze: 0.0, thumbstick_x: 0.0, thumbstick_y: 0.0, active: false },
    ControllerHandState { aim_pos: [0.0; 3], aim_orient: [0.0, 0.0, 0.0, 1.0], trigger: 0.0, squeeze: 0.0, thumbstick_x: 0.0, thumbstick_y: 0.0, active: false },
]);
// These maps are populated at init time and read-only during the frame loop.
static AIM_SPACES: OnceLock<RwLock<HashMap<u64, Hand>>> = OnceLock::new();
static TRIGGER_ACTIONS: OnceLock<RwLock<HashMap<(u64, Hand), ()>>> = OnceLock::new();
static SQUEEZE_ACTIONS: OnceLock<RwLock<HashMap<(u64, Hand), ()>>> = OnceLock::new();
static THUMBSTICK_ACTIONS: OnceLock<RwLock<HashMap<u64, ()>>> = OnceLock::new();
static NEXT_LOCATE_SPACE: OnceLock<xr::pfn::LocateSpace> = OnceLock::new();
static NEXT_GET_FLOAT: OnceLock<xr::pfn::GetActionStateFloat> = OnceLock::new();
static NEXT_GET_VEC2: OnceLock<xr::pfn::GetActionStateVector2f> = OnceLock::new();
// Hand subaction paths — set during suggest_bindings, used by float hooks to determine hand
static LEFT_HAND_PATH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RIGHT_HAND_PATH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
    direct_trace(&format!(
        "BUILD {} negotiate entered",
        BUILD_MARKER
    ));
    layer_log!(
        warn,
        "[ClearXR Layer] BUILD MARKER {} loaded from DLL. If you see this, the installed DLL is definitely updated.",
        BUILD_MARKER
    );

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
    direct_trace(&format!(
        "BUILD {} create_api_layer_instance entered",
        BUILD_MARKER
    ));
    layer_log!(
        warn,
        "[ClearXR Layer] BUILD MARKER {} reached layer_create_api_layer_instance.",
        BUILD_MARKER
    );

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
            let oculus_touch_profile = string_to_path(
                &dispatch,
                instance,
                b"/interaction_profiles/oculus/touch_controller\0",
            )
            .unwrap_or(xr::Path::NULL);

            // Resolve hand subaction paths for the float/vec2 hooks
            if let Some(left_path) = string_to_path(&dispatch, instance, b"/user/hand/left\0") {
                LEFT_HAND_PATH.store(left_path.into_raw(), std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(right_path) = string_to_path(&dispatch, instance, b"/user/hand/right\0") {
                RIGHT_HAND_PATH.store(right_path.into_raw(), std::sync::atomic::Ordering::Relaxed);
            }

            // Store the dispatch table in a lock-free static for all hooks.
            // This MUST be done before dispatch is moved into LayerState.
            let _ = NEXT.set(dispatch);
            let _ = NEXT_LOCATE_SPACE.set(dispatch.locate_space);
            let _ = NEXT_GET_FLOAT.set(dispatch.get_action_state_float);
            let _ = NEXT_GET_VEC2.set(dispatch.get_action_state_vector2f);
            let _ = THUMBSTICK_ACTIONS.set(RwLock::new(HashMap::new()));
            let _ = AIM_SPACES.set(RwLock::new(HashMap::new()));
            let _ = TRIGGER_ACTIONS.set(RwLock::new(HashMap::new()));
            let _ = SQUEEZE_ACTIONS.set(RwLock::new(HashMap::new()));

            // Store layer state
            *LAYER.lock().unwrap() = Some(LayerState {
                instance,
                next: dispatch,
                system_id: 0,
                opaque: None,
                overrides: HashMap::new(),
                haptic_actions: HashSet::new(),
                has_opaque_ext: has_opaque,
                oculus_touch_profile,
                overlay: None,
                aim_actions: HashSet::new(),
                trigger_actions: HashMap::new(),
                squeeze_actions: HashMap::new(),
                aim_spaces: HashMap::new(),
                controller_state: Default::default(),
                left_hand_path: string_to_path(&dispatch, instance, b"/user/hand/left\0").unwrap_or(xr::Path::NULL),
                right_hand_path: string_to_path(&dispatch, instance, b"/user/hand/right\0").unwrap_or(xr::Path::NULL),
                session: xr::Session::NULL,
            });

            layer_log!(
                info,
                "[ClearXR Layer] Instance created, layer active. Oculus touch profile path={:?}",
                oculus_touch_profile
            );
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
        get_system_properties: load_fn(gpa, instance, b"xrGetSystemProperties\0"),
        create_session: load_fn(gpa, instance, b"xrCreateSession\0"),
        destroy_session: load_fn(gpa, instance, b"xrDestroySession\0"),
        end_frame: load_fn(gpa, instance, b"xrEndFrame\0"),
        create_reference_space: load_fn(gpa, instance, b"xrCreateReferenceSpace\0"),
        destroy_space: load_fn(gpa, instance, b"xrDestroySpace\0"),
        enumerate_swapchain_formats: load_fn(gpa, instance, b"xrEnumerateSwapchainFormats\0"),
        create_swapchain: load_fn(gpa, instance, b"xrCreateSwapchain\0"),
        destroy_swapchain: load_fn(gpa, instance, b"xrDestroySwapchain\0"),
        enumerate_swapchain_images: load_fn(gpa, instance, b"xrEnumerateSwapchainImages\0"),
        acquire_swapchain_image: load_fn(gpa, instance, b"xrAcquireSwapchainImage\0"),
        wait_swapchain_image: load_fn(gpa, instance, b"xrWaitSwapchainImage\0"),
        release_swapchain_image: load_fn(gpa, instance, b"xrReleaseSwapchainImage\0"),
        suggest_interaction_profile_bindings: load_fn(gpa, instance, b"xrSuggestInteractionProfileBindings\0"),
        get_current_interaction_profile: load_fn(gpa, instance, b"xrGetCurrentInteractionProfile\0"),
        sync_actions: load_fn(gpa, instance, b"xrSyncActions\0"),
        get_action_state_boolean: load_fn(gpa, instance, b"xrGetActionStateBoolean\0"),
        apply_haptic_feedback: load_fn(gpa, instance, b"xrApplyHapticFeedback\0"),
        stop_haptic_feedback: load_fn(gpa, instance, b"xrStopHapticFeedback\0"),
        path_to_string: load_fn(gpa, instance, b"xrPathToString\0"),
        string_to_path: load_fn(gpa, instance, b"xrStringToPath\0"),
        create_action_space: load_fn(gpa, instance, b"xrCreateActionSpace\0"),
        locate_space: load_fn(gpa, instance, b"xrLocateSpace\0"),
        get_action_state_float: load_fn(gpa, instance, b"xrGetActionStateFloat\0"),
        get_action_state_vector2f: load_fn(gpa, instance, b"xrGetActionStateVector2f\0"),
    }
}

unsafe fn string_to_path(
    next: &NextDispatch,
    instance: xr::Instance,
    path: &[u8],
) -> Option<xr::Path> {
    let mut out = xr::Path::NULL;
    let result = (next.string_to_path)(instance, path.as_ptr() as *const c_char, &mut out);
    if result == xr::Result::SUCCESS {
        Some(out)
    } else {
        None
    }
}

unsafe fn path_to_string(next: &NextDispatch, instance: xr::Instance, path: xr::Path) -> Option<String> {
    if path == xr::Path::NULL {
        return Some("<null>".to_string());
    }

    let mut buf = [0u8; 256];
    let mut len: u32 = 0;
    let result = (next.path_to_string)(
        instance,
        path,
        buf.len() as u32,
        &mut len,
        buf.as_mut_ptr() as *mut c_char,
    );
    if result != xr::Result::SUCCESS || len == 0 {
        return None;
    }

    std::str::from_utf8(&buf[..len as usize - 1]).ok().map(str::to_owned)
}

unsafe fn system_name_to_string(system_name: &[c_char]) -> String {
    CStr::from_ptr(system_name.as_ptr())
        .to_string_lossy()
        .into_owned()
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
    let tracked_name = name_str.to_bytes();
    if matches!(
        tracked_name,
        b"xrCreateSession"
            | b"xrDestroySession"
            | b"xrEndFrame"
            | b"xrBeginFrame"
            | b"xrWaitFrame"
            | b"xrCreateReferenceSpace"
            | b"xrCreateSwapchain"
    ) {
        direct_trace(&format!(
            "BUILD {} gpa query {} instance={:?}",
            BUILD_MARKER,
            name_str.to_string_lossy(),
            instance
        ));
        layer_log!(
            info,
            "[ClearXR Layer] BUILD MARKER {} GPA query for {} (instance={:?}).",
            BUILD_MARKER,
            name_str.to_string_lossy(),
            instance
        );
    }

    // Return our intercepted functions
    macro_rules! intercept {
        ($fn_name:expr, $fn_ptr:expr) => {
            if name_str.to_bytes() == $fn_name {
                direct_trace(&format!(
                    "BUILD {} intercept {}",
                    BUILD_MARKER,
                    name_str.to_string_lossy()
                ));
                layer_log!(
                    info,
                    "[ClearXR Layer] BUILD MARKER {} intercepting {}.",
                    BUILD_MARKER,
                    name_str.to_string_lossy()
                );
                *function = Some(std::mem::transmute($fn_ptr as *const ()));
                return xr::Result::SUCCESS;
            }
        };
    }

    intercept!(b"xrGetInstanceProcAddr", layer_get_instance_proc_addr as xr::pfn::GetInstanceProcAddr);
    intercept!(b"xrDestroyInstance", hook_destroy_instance as xr::pfn::DestroyInstance);
    intercept!(b"xrGetSystem", hook_get_system as xr::pfn::GetSystem);
    intercept!(b"xrGetSystemProperties", hook_get_system_properties as xr::pfn::GetSystemProperties);
    intercept!(b"xrCreateSession", hook_create_session as xr::pfn::CreateSession);
    intercept!(b"xrDestroySession", hook_destroy_session as xr::pfn::DestroySession);
    intercept!(b"xrEndFrame", hook_end_frame as xr::pfn::EndFrame);
    intercept!(b"xrSuggestInteractionProfileBindings", hook_suggest_bindings as xr::pfn::SuggestInteractionProfileBindings);
    intercept!(b"xrGetCurrentInteractionProfile", hook_get_current_interaction_profile as xr::pfn::GetCurrentInteractionProfile);
    intercept!(b"xrSyncActions", hook_sync_actions as xr::pfn::SyncActions);
    intercept!(b"xrGetActionStateBoolean", hook_get_action_state_boolean as xr::pfn::GetActionStateBoolean);
    intercept!(b"xrApplyHapticFeedback", hook_apply_haptic_feedback as xr::pfn::ApplyHapticFeedback);
    intercept!(b"xrStopHapticFeedback", hook_stop_haptic_feedback as xr::pfn::StopHapticFeedback);
    intercept!(b"xrCreateActionSpace", hook_create_action_space as xr::pfn::CreateActionSpace);
    intercept!(b"xrLocateSpace", hook_locate_space as xr::pfn::LocateSpace);
    // xrGetActionStateFloat and xrGetActionStateVector2f are NOT intercepted.
    // We actively query these values in poll_opaque_and_update_overlay instead.

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

unsafe fn find_vulkan_binding<'a>(
    mut next: *const xr::BaseInStructure,
) -> Option<&'a xr::GraphicsBindingVulkanKHR> {
    while !next.is_null() {
        let candidate = &*next;
        if candidate.ty == xr::GraphicsBindingVulkanKHR::TYPE {
            return Some(&*(next as *const xr::GraphicsBindingVulkanKHR));
        }
        next = candidate.next;
    }
    None
}

/// Actively query a float action value for a specific hand.
unsafe fn query_float(next: &NextDispatch, session: xr::Session, action_raw: u64, subaction: xr::Path) -> Option<f32> {
    let get_info = xr::ActionStateGetInfo {
        ty: xr::ActionStateGetInfo::TYPE,
        next: std::ptr::null(),
        action: xr::Action::from_raw(action_raw),
        subaction_path: subaction,
    };
    let mut state_out = xr::ActionStateFloat {
        ty: xr::ActionStateFloat::TYPE,
        next: std::ptr::null_mut(),
        current_state: 0.0,
        changed_since_last_sync: xr::FALSE,
        last_change_time: xr::Time::from_nanos(0),
        is_active: xr::FALSE,
    };
    let r = (next.get_action_state_float)(session, &get_info, &mut state_out);
    if r == xr::Result::SUCCESS && state_out.is_active.into() {
        Some(state_out.current_state)
    } else {
        None
    }
}

unsafe fn poll_opaque_and_update_overlay(state: &mut LayerState) {
    static DIAG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let diag = DIAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Poll opaque channel for button overrides (menu button for visibility toggle)
    let mut menu_down = false;
    if let Some(ref mut ch) = state.opaque {
        ch.poll();
        if let Some(pkt) = ch.latest {
            let left_menu = (pkt.active_hands & 0x01) != 0 && (pkt.left.buttons & SC_BTN_MENU) != 0;
            let right_menu = (pkt.active_hands & 0x02) != 0 && (pkt.right.buttons & SC_BTN_MENU) != 0;
            menu_down = left_menu || right_menu;
        }
    }

    // Read aim poses captured by hook_locate_space
    let cs = CONTROLLER_STATE.read().unwrap().clone();

    // Actively query trigger/squeeze/thumbstick values (not intercepted — we call directly)
    let next = match NEXT.get() {
        Some(n) => *n,
        None => return,
    };
    let session = state.session;
    if session == xr::Session::NULL { return; }

    let mut left = cs[0];
    let mut right = cs[1];

    // Query trigger + squeeze for each hand
    for (hand_path, hand_state) in [
        (state.left_hand_path, &mut left),
        (state.right_hand_path, &mut right),
    ] {
        if hand_path == xr::Path::NULL { continue; }

        // Find trigger action for this hand
        for &(action_raw, _hand) in state.trigger_actions.keys() {
            let val = query_float(&next, session, action_raw, hand_path);
            if val.is_some() {
                hand_state.trigger = val.unwrap_or(0.0);
                break;
            }
        }
        // Find squeeze action for this hand
        for &(action_raw, _hand) in state.squeeze_actions.keys() {
            let val = query_float(&next, session, action_raw, hand_path);
            if val.is_some() {
                hand_state.squeeze = val.unwrap_or(0.0);
                break;
            }
        }
    }

    let mut active_hands = 0u8;
    if left.active { active_hands |= 0x01; }
    if right.active { active_hands |= 0x02; }

    let pkt = SpatialControllerPacket {
        magic: 0x5343,
        version: 1,
        active_hands,
        left: SpatialControllerHand {
            buttons: 0,
            _reserved: 0,
            thumbstick_x: left.thumbstick_x,
            thumbstick_y: left.thumbstick_y,
            trigger: left.trigger,
            grip: left.squeeze,
            pos_x: left.aim_pos[0],
            pos_y: left.aim_pos[1],
            pos_z: left.aim_pos[2],
            rot_x: left.aim_orient[0],
            rot_y: left.aim_orient[1],
            rot_z: left.aim_orient[2],
            rot_w: left.aim_orient[3],
        },
        right: SpatialControllerHand {
            buttons: 0,
            _reserved: 0,
            thumbstick_x: right.thumbstick_x,
            thumbstick_y: right.thumbstick_y,
            trigger: right.trigger,
            grip: right.squeeze,
            pos_x: right.aim_pos[0],
            pos_y: right.aim_pos[1],
            pos_z: right.aim_pos[2],
            rot_x: right.aim_orient[0],
            rot_y: right.aim_orient[1],
            rot_z: right.aim_orient[2],
            rot_w: right.aim_orient[3],
        },
    };

    if let Some(ref mut overlay) = state.overlay {
        if overlay.update_menu_button(menu_down) {
            layer_log!(info, "[ClearXR Layer] Dashboard overlay visibility toggled -> {}.", overlay.visible());
        }
        if active_hands != 0 {
            overlay.send_controller_input(&pkt);
        }
        if diag < 5 || diag % 360 == 0 {
            layer_log!(info,
                "[ClearXR Layer] Controller state: active=0x{:02x} L=[{:.2},{:.2},{:.2}] R=[{:.2},{:.2},{:.2}] L_trig={:.2} R_trig={:.2}",
                active_hands,
                left.aim_pos[0], left.aim_pos[1], left.aim_pos[2],
                right.aim_pos[0], right.aim_pos[1], right.aim_pos[2],
                left.trigger, right.trigger
            );
        }
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
    let mut guard = LAYER.lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };
    layer_log!(
        info,
        "[ClearXR Layer] hook_create_session entered (instance={:?}, ci_null={}, session_null={}).",
        instance,
        ci.is_null(),
        session.is_null()
    );
    direct_trace(&format!(
        "BUILD {} hook_create_session entered instance={:?} ci_null={} session_null={}",
        BUILD_MARKER,
        instance,
        ci.is_null(),
        session.is_null()
    ));
    let result = (state.next.create_session)(instance, ci, session);
    layer_log!(
        info,
        "[ClearXR Layer] hook_create_session next returned {:?} (session={:?}).",
        result,
        if session.is_null() { xr::Session::NULL } else { *session }
    );
    if result != xr::Result::SUCCESS {
        return result;
    }
    if ci.is_null() || session.is_null() {
        return result;
    }

    state.session = *session;

    if let Some(binding) = find_vulkan_binding((*ci).next as *const xr::BaseInStructure) {
        layer_log!(
            info,
            "[ClearXR Layer] hook_create_session found Vulkan binding: queue_family={} queue_index={}.",
            binding.queue_family_index,
            binding.queue_index
        );
        match DashboardOverlay::new(&state.next, *session, binding) {
            Ok(overlay) => {
                state.overlay = Some(overlay);
                layer_log!(
                    info,
                    "[ClearXR Layer] Dashboard overlay attached to session {:?} using Vulkan binding; default visible.",
                    *session
                );
            }
            Err(err) => {
                layer_log!(warn, "[ClearXR Layer] Dashboard overlay disabled: {}", err);
                state.overlay = None;
            }
        }
    } else {
        let mut chain = (*ci).next as *const xr::BaseInStructure;
        while !chain.is_null() {
            let candidate = &*chain;
            layer_log!(
                info,
                "[ClearXR Layer] hook_create_session saw create-info chain struct {:?}.",
                candidate.ty
            );
            chain = candidate.next;
        }
        layer_log!(
            warn,
            "[ClearXR Layer] Session created without XR_KHR_vulkan_enable binding; dashboard overlay spike is inactive."
        );
        state.overlay = None;
    }

    let _ = instance;
    result
}

unsafe extern "system" fn hook_get_system_properties(
    instance: xr::Instance,
    system_id: xr::SystemId,
    properties: *mut xr::SystemProperties,
) -> xr::Result {
    let guard = LAYER.lock().unwrap();
    let state = match guard.as_ref() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    let result = (state.next.get_system_properties)(instance, system_id, properties);
    if result != xr::Result::SUCCESS || properties.is_null() {
        return result;
    }

    let props = &*properties;
    let system_name = system_name_to_string(&props.system_name);
    layer_log!(
        info,
        "[ClearXR Layer] xrGetSystemProperties system_id={} vendor_id={} system_name={:?} orientation_tracking={} position_tracking={}",
        system_id.into_raw(),
        props.vendor_id,
        system_name,
        props.tracking_properties.orientation_tracking,
        props.tracking_properties.position_tracking
    );

    result
}

unsafe extern "system" fn hook_destroy_session(session: xr::Session) -> xr::Result {
    let next = match NEXT.get() {
        Some(n) => *n,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    // Drop the overlay if it belongs to this session. Drop impl handles all cleanup.
    if let Ok(mut guard) = LAYER.lock() {
        if let Some(state) = guard.as_mut() {
            if let Some(overlay) = state.overlay.take() {
                if overlay.is_for_session(session) {
                    drop(overlay); // Drop impl cleans up Vulkan + OpenXR resources
                    layer_log!(info, "[ClearXR Layer] Dashboard overlay detached from session {:?}.", session);
                } else {
                    state.overlay = Some(overlay);
                }
            }
        }
    }

    (next.destroy_session)(session)
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

            // Track aim pose actions for xrLocateSpace interception
            if component == "/input/aim/pose" {
                state.aim_actions.insert(action_raw);
                layer_log!(info, "[ClearXR Layer] Tracked aim action: 0x{:x} {:?}", action_raw, hand);
            }
            // Track trigger/squeeze float actions
            if component == "/input/trigger/value" {
                state.trigger_actions.insert((action_raw, hand), ());
                if let Some(lock) = TRIGGER_ACTIONS.get() { if let Ok(mut m) = lock.write() { m.insert((action_raw, hand), ()); } }
                layer_log!(info, "[ClearXR Layer] Tracked trigger action: 0x{:x} {:?}", action_raw, hand);
            }
            if component == "/input/squeeze/value" {
                state.squeeze_actions.insert((action_raw, hand), ());
                if let Some(lock) = SQUEEZE_ACTIONS.get() { if let Ok(mut m) = lock.write() { m.insert((action_raw, hand), ()); } }
                layer_log!(info, "[ClearXR Layer] Tracked squeeze action: 0x{:x} {:?}", action_raw, hand);
            }
            if component == "/input/thumbstick" {
                if let Some(lock) = THUMBSTICK_ACTIONS.get() { if let Ok(mut m) = lock.write() { m.insert(action_raw, ()); } }
                layer_log!(info, "[ClearXR Layer] Tracked thumbstick action: 0x{:x}", action_raw);
            }

            if is_haptic_output_path(component) {
                state.haptic_actions.insert((action_raw, hand));
            }
        }
    }

    // Pass through to next layer
    (state.next.suggest_interaction_profile_bindings)(instance, suggested_bindings)
}

unsafe extern "system" fn hook_get_current_interaction_profile(
    session: xr::Session,
    top_level_user_path: xr::Path,
    interaction_profile: *mut xr::InteractionProfileState,
) -> xr::Result {
    let mut guard = LAYER.lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    let result = (state.next.get_current_interaction_profile)(
        session,
        top_level_user_path,
        interaction_profile,
    );
    if result != xr::Result::SUCCESS || interaction_profile.is_null() {
        return result;
    }

    let current = &mut *interaction_profile;
    let top_level_user_path_str = path_to_string(&state.next, state.instance, top_level_user_path)
        .unwrap_or_else(|| format!("<unresolved {:?}>", top_level_user_path));
    let current_profile_str = path_to_string(&state.next, state.instance, current.interaction_profile)
        .unwrap_or_else(|| format!("<unresolved {:?}>", current.interaction_profile));
    layer_log!(
        info,
        "[ClearXR Layer] xrGetCurrentInteractionProfile top_level_user_path={} returned_profile={}",
        top_level_user_path_str,
        current_profile_str
    );

    if current.interaction_profile == xr::Path::NULL && state.oculus_touch_profile != xr::Path::NULL {
        current.interaction_profile = state.oculus_touch_profile;
        let substituted_profile_str = path_to_string(
            &state.next,
            state.instance,
            current.interaction_profile,
        )
        .unwrap_or_else(|| format!("<unresolved {:?}>", current.interaction_profile));
        layer_log!(
            info,
            "[ClearXR Layer] Substituted Oculus Touch interaction profile for top-level path {} -> {}.",
            top_level_user_path_str,
            substituted_profile_str
        );
    }

    result
}

unsafe extern "system" fn hook_sync_actions(
    session: xr::Session,
    sync_info: *const xr::ActionsSyncInfo,
) -> xr::Result {
    // Call runtime WITHOUT holding the LAYER mutex
    let next = match NEXT.get() {
        Some(n) => *n,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };
    // Just pass through — poll_opaque_and_update_overlay is called in hook_end_frame
    (next.sync_actions)(session, sync_info)
}

unsafe extern "system" fn hook_end_frame(
    session: xr::Session,
    frame_end_info: *const xr::FrameEndInfo,
) -> xr::Result {
    let next = match NEXT.get() {
        Some(n) => *n,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };
    if frame_end_info.is_null() {
        return xr::Result::ERROR_VALIDATION_FAILURE;
    }

    // Lock LAYER briefly to update overlay + get the quad layer info, then drop.
    let overlay_quad = {
        let mut guard = match LAYER.lock() {
            Ok(g) => g,
            Err(_) => return (next.end_frame)(session, frame_end_info),
        };
        let state = match guard.as_mut() {
            Some(s) => s,
            None => return (next.end_frame)(session, frame_end_info),
        };

        poll_opaque_and_update_overlay(state);

        match state.overlay.as_mut() {
            Some(overlay) if overlay.is_for_session(session) && overlay.visible() => {
                if let Err(err) = overlay.render_frame(&next) {
                    layer_log!(warn, "[ClearXR Layer] Dashboard overlay render failed: {}", err);
                }
                Some(overlay.quad_layer())
            }
            _ => None,
        }
        // LAYER mutex dropped here
    };

    let Some(overlay_quad) = overlay_quad else {
        return (next.end_frame)(session, frame_end_info);
    };

    // Build the extended layer list on the stack (no heap allocation).
    let end_info = &*frame_end_info;
    let base_count = if end_info.layer_count == 0 || end_info.layers.is_null() {
        0usize
    } else {
        end_info.layer_count as usize
    };
    // Stack array: up to 8 base layers + 1 overlay. Most apps use 1-4.
    let mut layers: [*const xr::CompositionLayerBaseHeader; 9] =
        [std::ptr::null(); 9];
    let total = (base_count + 1).min(9);
    for i in 0..base_count.min(8) {
        layers[i] = *end_info.layers.add(i);
    }
    layers[base_count.min(8)] =
        &overlay_quad as *const xr::CompositionLayerQuad as *const xr::CompositionLayerBaseHeader;

    let wrapped_end_info = xr::FrameEndInfo {
        ty: end_info.ty,
        next: end_info.next,
        display_time: end_info.display_time,
        environment_blend_mode: end_info.environment_blend_mode,
        layer_count: total as u32,
        layers: layers.as_ptr(),
    };

    (next.end_frame)(session, &wrapped_end_info)
}

unsafe extern "system" fn hook_create_action_space(
    session: xr::Session,
    create_info: *const xr::ActionSpaceCreateInfo,
    space: *mut xr::Space,
) -> xr::Result {
    let mut guard = LAYER.lock().unwrap();
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    let result = (state.next.create_action_space)(session, create_info, space);
    if result != xr::Result::SUCCESS || create_info.is_null() || space.is_null() {
        return result;
    }

    let ci = &*create_info;
    let action_raw = ci.action.into_raw();

    // If this action is a tracked aim action, record the space → hand mapping
    if state.aim_actions.contains(&action_raw) {
        if let Some(hand) = hand_from_path(state, ci.subaction_path) {
            let space_raw = (*space).into_raw();
            state.aim_spaces.insert(space_raw, hand);
            // Also update the separate static for the hot-path hook
            if let Some(lock) = AIM_SPACES.get() {
                if let Ok(mut map) = lock.write() { map.insert(space_raw, hand); }
            }
            layer_log!(info,
                "[ClearXR Layer] Aim space created: space=0x{:x} action=0x{:x} {:?}",
                space_raw, action_raw, hand
            );
        }
    }

    result
}

unsafe extern "system" fn hook_locate_space(
    space: xr::Space,
    base_space: xr::Space,
    time: xr::Time,
    location: *mut xr::SpaceLocation,
) -> xr::Result {
    // Hot path — use OnceLock/RwLock, NOT the LAYER mutex. Zero contention on read path.
    let next_fn = match NEXT_LOCATE_SPACE.get() {
        Some(&f) => f,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    let result = (next_fn)(space, base_space, time, location);
    if result != xr::Result::SUCCESS || location.is_null() {
        return result;
    }

    // Quick check: is this one of our tracked aim spaces? (read lock — no contention with other readers)
    let space_raw = space.into_raw();
    let hand = AIM_SPACES.get().and_then(|lock| lock.read().ok()).and_then(|map| map.get(&space_raw).copied());

    if let Some(hand) = hand {
        let loc = &*location;
        let valid = loc.location_flags.contains(
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID
        );
        let idx = match hand { Hand::Left => 0, Hand::Right => 1 };
        if let Ok(mut cs) = CONTROLLER_STATE.write() {
            cs[idx].active = valid;
            if valid {
                cs[idx].aim_pos = [loc.pose.position.x, loc.pose.position.y, loc.pose.position.z];
                cs[idx].aim_orient = [loc.pose.orientation.x, loc.pose.orientation.y, loc.pose.orientation.z, loc.pose.orientation.w];
            }
        }
    }

    result
}

unsafe extern "system" fn hook_get_action_state_float(
    session: xr::Session,
    get_info: *const xr::ActionStateGetInfo,
    state_out: *mut xr::ActionStateFloat,
) -> xr::Result {
    // Hot path — use OnceLock/RwLock, NOT the LAYER mutex.
    let next_fn = match NEXT_GET_FLOAT.get() {
        Some(&f) => f,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };

    let result = (next_fn)(session, get_info, state_out);
    if result != xr::Result::SUCCESS || get_info.is_null() || state_out.is_null() {
        return result;
    }

    let info = &*get_info;
    let action_raw = info.action.into_raw();
    let out = &*state_out;

    // Read locks — no contention with other readers
    let trigger_guard = TRIGGER_ACTIONS.get().and_then(|l| l.read().ok());
    let squeeze_guard = SQUEEZE_ACTIONS.get().and_then(|l| l.read().ok());

    // Determine hand from subaction path (lock-free)
    let hand = hand_from_subaction_path(info.subaction_path);

    static FDIAG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let fd = FDIAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if fd < 8 {
        let sub_raw = info.subaction_path.into_raw();
        let left_raw = LEFT_HAND_PATH.load(std::sync::atomic::Ordering::Relaxed);
        let right_raw = RIGHT_HAND_PATH.load(std::sync::atomic::Ordering::Relaxed);
        log::info!(
            "[ClearXR Layer] Float: action=0x{:x} subaction=0x{:x} hand={:?} left_path=0x{:x} right_path=0x{:x} val={:.2}",
            action_raw, sub_raw, hand, left_raw, right_raw, out.current_state
        );
    }

    if let Some(hand) = hand {
        let idx = match hand { Hand::Left => 0, Hand::Right => 1 };
        let is_trigger = trigger_guard.as_ref().map_or(false, |m| m.contains_key(&(action_raw, hand)));
        let is_squeeze = squeeze_guard.as_ref().map_or(false, |m| m.contains_key(&(action_raw, hand)));

        if is_trigger || is_squeeze {
            if let Ok(mut cs) = CONTROLLER_STATE.write() {
                if is_trigger { cs[idx].trigger = out.current_state; }
                if is_squeeze { cs[idx].squeeze = out.current_state; }
            }
        }
    }

    result
}

unsafe extern "system" fn hook_get_action_state_vector2f(
    session: xr::Session,
    get_info: *const xr::ActionStateGetInfo,
    state_out: *mut xr::ActionStateVector2f,
) -> xr::Result {
    let next = match NEXT_GET_VEC2.get() {
        Some(&f) => f,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };
    let result = (next)(session, get_info, state_out);
    if result != xr::Result::SUCCESS || get_info.is_null() || state_out.is_null() {
        return result;
    }

    let info = &*get_info;
    let action_raw = info.action.into_raw();
    let out = &*state_out;

    let is_thumbstick = THUMBSTICK_ACTIONS.get()
        .and_then(|l| l.read().ok())
        .map_or(false, |m| m.contains_key(&action_raw));

    if is_thumbstick && out.is_active.into() {
        if let Some(hand) = hand_from_subaction_path(info.subaction_path) {
            let idx = match hand { Hand::Left => 0, Hand::Right => 1 };
            if let Ok(mut cs) = CONTROLLER_STATE.write() {
                cs[idx].thumbstick_x = out.current_state.x;
                cs[idx].thumbstick_y = out.current_state.y;
            }
        }
    }

    result
}

unsafe extern "system" fn hook_apply_haptic_feedback(
    session: xr::Session,
    haptic_action_info: *const xr::HapticActionInfo,
    haptic_feedback: *const xr::HapticBaseHeader,
) -> xr::Result {
    // Call runtime first without lock
    let next = match NEXT.get() {
        Some(n) => *n,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };
    let result = (next.apply_haptic_feedback)(session, haptic_action_info, haptic_feedback);
    if result != xr::Result::SUCCESS || haptic_action_info.is_null() || haptic_feedback.is_null() {
        return result;
    }

    let mut guard = match LAYER.lock() {
        Ok(g) => g,
        Err(_) => return result,
    };
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return result,
    };

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
    let next = match NEXT.get() {
        Some(n) => *n,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };
    let result = (next.stop_haptic_feedback)(session, haptic_action_info);
    if result != xr::Result::SUCCESS || haptic_action_info.is_null() {
        return result;
    }

    let mut guard = match LAYER.lock() {
        Ok(g) => g,
        Err(_) => return result,
    };
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return result,
    };

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
    // Call the runtime WITHOUT holding the LAYER mutex
    let next = match NEXT.get() {
        Some(n) => *n,
        None => return xr::Result::ERROR_HANDLE_INVALID,
    };
    let result = (next.get_action_state_boolean)(session, get_info, state_out);

    // Now lock for override check
    let mut guard = match LAYER.lock() {
        Ok(g) => g,
        Err(_) => return result,
    };
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return result,
    };
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
        let r = (next.path_to_string)(
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

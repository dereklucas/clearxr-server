use std::cmp::Ordering;
use std::fs;
use std::iter;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use log::info;

use crate::models::OpenXrRegistrationStatus;

#[cfg(windows)]
use windows_registry::{Key, Value, CURRENT_USER, LOCAL_MACHINE};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HWND};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
#[cfg(windows)]
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOCLOSEPROCESS};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const OPENXR_RUNTIME_REGISTRY_KEY: &str = "SOFTWARE\\Khronos\\OpenXR\\1";
const OPENXR_LAYER_REGISTRY_KEY: &str = "SOFTWARE\\Khronos\\OpenXR\\1\\ApiLayers\\Implicit";
const OPENXR_RUNTIME_VALUE_NAME: &str = "ActiveRuntime";
const RUNTIME_MANIFEST_NAME: &str = "openxr_cloudxr.json";
const LAYER_MANIFEST_NAME: &str = "clear-xr-layer.json";
const LAYER_DLL_NAME: &str = "clear_xr_layer.dll";
const REGISTER_RUNTIME_ELEVATED_ARG: &str = "--clearxr-register-runtime-elevated";
const DEREGISTER_LAYER_ELEVATED_ARG: &str = "--clearxr-deregister-layer-elevated";

#[derive(Debug, Clone)]
struct LayerRegistration {
    scope: String,
}

#[derive(Debug, Clone)]
struct LayerRegistryEntry {
    registry_name: String,
    manifest_path: PathBuf,
}

pub fn get_openxr_registration_status() -> Result<OpenXrRegistrationStatus> {
    get_openxr_registration_status_impl()
}

pub fn register_openxr_runtime_and_layer() -> Result<OpenXrRegistrationStatus> {
    register_openxr_runtime_and_layer_impl()
}

pub fn deregister_openxr_layer() -> Result<OpenXrRegistrationStatus> {
    deregister_openxr_layer_impl()
}

pub fn register_runtime_from_elevated_helper(runtime_manifest_path: PathBuf) -> Result<()> {
    register_runtime_from_elevated_helper_impl(runtime_manifest_path)
}

pub fn deregister_layer_from_elevated_helper(layer_manifest_path: PathBuf) -> Result<()> {
    deregister_layer_from_elevated_helper_impl(layer_manifest_path)
}

#[cfg(windows)]
fn get_openxr_registration_status_impl() -> Result<OpenXrRegistrationStatus> {
    let runtime_manifest_path = find_runtime_manifest_path()?;
    let layer_manifest_path = find_layer_manifest_path()?;
    let active_runtime_path = read_active_runtime_path();
    let runtime_is_active = active_runtime_path
        .as_ref()
        .is_some_and(|current| paths_match(current, &runtime_manifest_path));
    let layer_registration = read_layer_registration(&layer_manifest_path);
    let layer_is_registered = layer_registration.is_some();

    let runtime_detail = if !runtime_manifest_path.exists() {
        format!(
            "Runtime manifest is missing from {}.",
            runtime_manifest_path.display()
        )
    } else if runtime_is_active {
        format!(
            "Clear XR is the active OpenXR runtime at {}.",
            runtime_manifest_path.display()
        )
    } else if let Some(current) = &active_runtime_path {
        format!("Windows is currently using {}.", current.display())
    } else {
        "No active OpenXR runtime was found in the Windows registry.".to_string()
    };

    let layer_detail = if !layer_manifest_path.exists() {
        format!(
            "Layer manifest is missing from {}.",
            layer_manifest_path.display()
        )
    } else if !layer_manifest_path.with_file_name(LAYER_DLL_NAME).exists() {
        format!(
            "Layer DLL is missing from {}.",
            layer_manifest_path.with_file_name(LAYER_DLL_NAME).display()
        )
    } else if let Some(registration) = &layer_registration {
        format!(
            "Clear XR layer is registered in {}.",
            registration.scope
        )
    } else {
        "Clear XR layer is not registered yet.".to_string()
    };

    Ok(OpenXrRegistrationStatus {
        runtime_is_active,
        runtime_manifest_path: runtime_manifest_path.display().to_string(),
        active_runtime_path: active_runtime_path.map(|path| path.display().to_string()),
        runtime_detail,
        layer_is_registered,
        layer_manifest_path: layer_manifest_path.display().to_string(),
        layer_registration_scope: layer_registration.map(|registration| registration.scope),
        layer_detail,
    })
}

#[cfg(windows)]
fn register_openxr_runtime_and_layer_impl() -> Result<OpenXrRegistrationStatus> {
    let layer_manifest_path = require_existing_layer_manifest_path()?;
    let runtime_manifest_path = require_existing_runtime_manifest_path()?;
    let layer_manifest = path_to_registry_string(&layer_manifest_path)?;
    let runtime_target = path_to_registry_string(&runtime_manifest_path)?;

    let layer_key = CURRENT_USER
        .create(OPENXR_LAYER_REGISTRY_KEY)
        .context("failed to open the current-user OpenXR layer registry key")?;
    let current_user_entries = read_layer_registry_entries(&layer_key)
        .context("failed to enumerate current-user OpenXR layer entries")?;
    let stale_entries = find_matching_layer_entries(&current_user_entries, &layer_manifest_path);
    for entry in stale_entries
        .iter()
        .filter(|entry| !paths_match(&entry.manifest_path, &layer_manifest_path))
    {
        info!(
            "Removing stale Clear XR OpenXR layer registration in HKEY_CURRENT_USER ({})",
            entry.manifest_path.display()
        );
        layer_key
            .remove_value(&entry.registry_name)
            .context("failed to remove a stale Clear XR OpenXR layer registration")?;
    }

    if let Some(existing) = find_layer_entry(&read_layer_registry_entries(&layer_key)?, &layer_manifest_path) {
        info!(
            "Clear XR OpenXR layer is already registered in HKEY_CURRENT_USER ({})",
            existing.manifest_path.display()
        );
        if existing.registry_name != layer_manifest {
            layer_key
                .remove_value(&existing.registry_name)
                .context("failed to replace the current-user Clear XR OpenXR layer registration")?;
            layer_key
                .set_value(&layer_manifest, &Value::from(0u32))
                .context("failed to normalize the current-user Clear XR OpenXR layer registration")?;
        } else {
            layer_key
                .set_value(&layer_manifest, &Value::from(0u32))
                .context("failed to keep the current-user Clear XR OpenXR layer enabled")?;
        }
    } else {
        info!(
            "Registering Clear XR OpenXR layer in HKEY_CURRENT_USER ({})",
            layer_manifest
        );
        layer_key
            .set_value(&layer_manifest, &Value::from(0u32))
            .context("failed to register the Clear XR OpenXR layer")?;
        info!("Registered Clear XR OpenXR layer successfully");
    }

    let current_runtime = read_active_runtime_path();
    if !current_runtime
        .as_ref()
        .is_some_and(|current| paths_match(current, &runtime_manifest_path))
    {
        set_active_runtime_with_optional_elevation(&runtime_manifest_path, true)?;
    } else {
        info!(
            "Clear XR is already the active OpenXR runtime at {}",
            runtime_target
        );
    }

    get_openxr_registration_status_impl()
}

#[cfg(not(windows))]
fn get_openxr_registration_status_impl() -> Result<OpenXrRegistrationStatus> {
    Err(anyhow!(
        "OpenXR runtime registration is only supported on Windows."
    ))
}

#[cfg(not(windows))]
fn register_openxr_runtime_and_layer_impl() -> Result<OpenXrRegistrationStatus> {
    Err(anyhow!(
        "OpenXR runtime registration is only supported on Windows."
    ))
}

#[cfg(windows)]
fn register_runtime_from_elevated_helper_impl(runtime_manifest_path: PathBuf) -> Result<()> {
    info!(
        "Elevated helper registering Clear XR as the active OpenXR runtime from {}",
        runtime_manifest_path.display()
    );
    set_active_runtime_direct(&runtime_manifest_path)
}

#[cfg(not(windows))]
fn register_runtime_from_elevated_helper_impl(_runtime_manifest_path: PathBuf) -> Result<()> {
    Err(anyhow!(
        "OpenXR runtime registration is only supported on Windows."
    ))
}

#[cfg(windows)]
fn deregister_openxr_layer_impl() -> Result<OpenXrRegistrationStatus> {
    let layer_manifest_path = find_layer_manifest_path()?;
    let removed_current_user = remove_matching_layer_registrations(CURRENT_USER, "HKEY_CURRENT_USER", &layer_manifest_path)
        .context("failed to remove current-user Clear XR OpenXR layer registrations")?;
    cleanup_empty_layer_registry_key(CURRENT_USER, "HKEY_CURRENT_USER")
        .context("failed to clean up the empty current-user OpenXR layer registry key")?;
    let removed_local_machine = match remove_matching_layer_registrations(
        LOCAL_MACHINE,
        "HKEY_LOCAL_MACHINE",
        &layer_manifest_path,
    ) {
        Ok(removed) => removed,
        Err(error) => {
            info!(
                "Requesting administrator rights to remove machine-wide Clear XR OpenXR layer registrations: {error}"
            );
            request_layer_deregistration_elevation(&layer_manifest_path)?;
            Vec::new()
        }
    };
    cleanup_empty_layer_registry_key(LOCAL_MACHINE, "HKEY_LOCAL_MACHINE")
        .ok();

    if removed_current_user.is_empty() && removed_local_machine.is_empty() {
        info!("No Clear XR OpenXR layer registrations were found to remove");
    }

    get_openxr_registration_status_impl()
}

#[cfg(not(windows))]
fn deregister_openxr_layer_impl() -> Result<OpenXrRegistrationStatus> {
    Err(anyhow!(
        "OpenXR runtime registration is only supported on Windows."
    ))
}

#[cfg(windows)]
fn deregister_layer_from_elevated_helper_impl(layer_manifest_path: PathBuf) -> Result<()> {
    let removed = remove_matching_layer_registrations(
        LOCAL_MACHINE,
        "HKEY_LOCAL_MACHINE",
        &layer_manifest_path,
    )?;
    cleanup_empty_layer_registry_key(LOCAL_MACHINE, "HKEY_LOCAL_MACHINE")?;
    if removed.is_empty() {
        info!("Elevated helper did not find any machine-wide Clear XR OpenXR layer registrations to remove");
    }
    Ok(())
}

#[cfg(not(windows))]
fn deregister_layer_from_elevated_helper_impl(_layer_manifest_path: PathBuf) -> Result<()> {
    Err(anyhow!(
        "OpenXR runtime registration is only supported on Windows."
    ))
}

#[cfg(windows)]
fn require_existing_runtime_manifest_path() -> Result<PathBuf> {
    let runtime_manifest_path = find_runtime_manifest_path()?;
    if runtime_manifest_path.exists() {
        Ok(runtime_manifest_path)
    } else {
        Err(anyhow!(
            "Clear XR runtime manifest is missing at {}. Build clearxr-streamer after vendoring CloudXR.",
            runtime_manifest_path.display()
        ))
    }
}

#[cfg(windows)]
fn set_active_runtime_with_optional_elevation(
    runtime_manifest_path: &Path,
    allow_elevation: bool,
) -> Result<()> {
    match set_active_runtime_direct(runtime_manifest_path) {
        Ok(()) => Ok(()),
        Err(error) if allow_elevation => {
            info!(
                "Requesting administrator rights to register the Clear XR OpenXR runtime: {error}"
            );
            request_runtime_registration_elevation(runtime_manifest_path)?;
            info!("Administrator-approved OpenXR runtime registration completed");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn set_active_runtime_direct(runtime_manifest_path: &Path) -> Result<()> {
    let runtime_target = path_to_registry_string(runtime_manifest_path)?;
    let runtime_key = LOCAL_MACHINE
        .create(OPENXR_RUNTIME_REGISTRY_KEY)
        .context("failed to open the machine-wide OpenXR runtime registry key")?;
    info!(
        "Registering Clear XR as the active OpenXR runtime in HKEY_LOCAL_MACHINE at {}",
        runtime_target
    );
    runtime_key
        .set_string(OPENXR_RUNTIME_VALUE_NAME, &runtime_target)
        .context(
            "failed to make Clear XR the active OpenXR runtime. Try running Clear XR Server as administrator.",
        )?;
    info!("Registered Clear XR as the active OpenXR runtime successfully");
    Ok(())
}

#[cfg(windows)]
fn request_runtime_registration_elevation(runtime_manifest_path: &Path) -> Result<()> {
    let exe_path =
        std::env::current_exe().context("failed to resolve the current executable path")?;
    let parameters = format!(
        "{} \"{}\"",
        REGISTER_RUNTIME_ELEVATED_ARG,
        escape_windows_argument(runtime_manifest_path)
    );
    let file = wide_null(exe_path.as_os_str());
    let verb = wide_null(std::ffi::OsStr::new("runas"));
    let params = wide_null(std::ffi::OsStr::new(&parameters));

    let mut execute_info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: 0 as HWND,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ptr(),
        lpDirectory: std::ptr::null(),
        nShow: SW_SHOWNORMAL,
        hInstApp: std::ptr::null_mut(),
        lpIDList: std::ptr::null_mut(),
        lpClass: std::ptr::null(),
        hkeyClass: std::ptr::null_mut(),
        dwHotKey: 0,
        Anonymous: Default::default(),
        hProcess: 0 as HANDLE,
    };

    let launched = unsafe { ShellExecuteExW(&mut execute_info) };
    if launched == 0 {
        let error_code = unsafe { GetLastError() };
        return Err(anyhow!(
            "failed to request administrator rights for OpenXR runtime registration (Windows error {})",
            error_code
        ));
    }

    let process_handle = execute_info.hProcess;
    if process_handle.is_null() {
        return Err(anyhow!(
            "administrator-elevated OpenXR runtime registration did not return a process handle"
        ));
    }

    let wait_result = unsafe { WaitForSingleObject(process_handle, INFINITE) };
    if wait_result == u32::MAX {
        unsafe { CloseHandle(process_handle) };
        return Err(anyhow!(
            "failed while waiting for administrator-elevated OpenXR runtime registration to finish"
        ));
    }

    let mut exit_code = 0u32;
    let exit_code_result = unsafe { GetExitCodeProcess(process_handle, &mut exit_code) };
    unsafe { CloseHandle(process_handle) };
    if exit_code_result == 0 {
        return Err(anyhow!(
            "failed to get the exit code from the administrator-elevated OpenXR runtime registration helper"
        ));
    }
    if exit_code != 0 {
        return Err(anyhow!(
            "administrator-elevated OpenXR runtime registration failed with exit code {}",
            exit_code
        ));
    }

    Ok(())
}

#[cfg(windows)]
fn require_existing_layer_manifest_path() -> Result<PathBuf> {
    let layer_manifest_path = find_layer_manifest_path()?;
    if !layer_manifest_path.exists() {
        return Err(anyhow!(
            "Clear XR layer manifest is missing at {}. Build clearxr-layer and clearxr-streamer first.",
            layer_manifest_path.display()
        ));
    }

    let layer_dll_path = layer_manifest_path.with_file_name(LAYER_DLL_NAME);
    if !layer_dll_path.exists() {
        return Err(anyhow!(
            "Clear XR layer DLL is missing at {}. Build clearxr-layer and clearxr-streamer first.",
            layer_dll_path.display()
        ));
    }

    Ok(layer_manifest_path)
}

fn find_runtime_manifest_path() -> Result<PathBuf> {
    let app_dir = app_dir()?;
    let search_roots = runtime_manifest_search_roots(&app_dir);
    let mut fallback = None;

    for root in &search_roots {
        let releases_dir = root.join("Server").join("releases");
        fallback = Some(releases_dir.join(RUNTIME_MANIFEST_NAME));
        if let Some(manifest_path) = find_latest_runtime_manifest_in_dir(&releases_dir)? {
            return Ok(manifest_path);
        }
    }

    fallback.ok_or_else(|| anyhow!("failed to determine an OpenXR runtime manifest search path"))
}

fn find_layer_manifest_path() -> Result<PathBuf> {
    Ok(app_dir()?.join(LAYER_MANIFEST_NAME))
}

fn app_dir() -> Result<PathBuf> {
    let exe_path =
        std::env::current_exe().context("failed to resolve the running executable path")?;
    exe_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("failed to resolve the application directory"))
}

#[cfg(windows)]
fn read_active_runtime_path() -> Option<PathBuf> {
    LOCAL_MACHINE
        .open(OPENXR_RUNTIME_REGISTRY_KEY)
        .ok()
        .and_then(|key| key.get_string(OPENXR_RUNTIME_VALUE_NAME).ok())
        .as_deref()
        .and_then(path_from_registry_string)
}

#[cfg(windows)]
fn read_layer_registration(layer_manifest_path: &Path) -> Option<LayerRegistration> {
    let current_user_entry = CURRENT_USER
        .open(OPENXR_LAYER_REGISTRY_KEY)
        .ok()
        .and_then(|key| read_layer_registry_entries(&key).ok())
        .and_then(|entries| find_layer_entry(&entries, layer_manifest_path).cloned());
    if current_user_entry.is_some() {
        return Some(LayerRegistration {
            scope: "HKEY_CURRENT_USER".to_string(),
        });
    }

    let local_machine_entry = LOCAL_MACHINE
        .open(OPENXR_LAYER_REGISTRY_KEY)
        .ok()
        .and_then(|key| read_layer_registry_entries(&key).ok())
        .and_then(|entries| find_layer_entry(&entries, layer_manifest_path).cloned());
    local_machine_entry.map(|_| LayerRegistration {
        scope: "HKEY_LOCAL_MACHINE".to_string(),
    })
}

#[cfg(windows)]
fn read_layer_registry_entries(key: &Key) -> Result<Vec<LayerRegistryEntry>> {
    let mut entries = Vec::new();
    for (name, _value) in key.values()? {
        let manifest_path = match path_from_registry_string(&name) {
            Some(path) => path,
            None => continue,
        };
        entries.push(LayerRegistryEntry {
            registry_name: name,
            manifest_path,
        });
    }
    Ok(entries)
}

#[cfg(windows)]
fn find_layer_entry<'a>(
    entries: &'a [LayerRegistryEntry],
    layer_manifest_path: &Path,
) -> Option<&'a LayerRegistryEntry> {
    entries
        .iter()
        .find(|entry| paths_match(&entry.manifest_path, layer_manifest_path))
}

#[cfg(windows)]
fn find_matching_layer_entries<'a>(
    entries: &'a [LayerRegistryEntry],
    layer_manifest_path: &Path,
) -> Vec<&'a LayerRegistryEntry> {
    entries
        .iter()
        .filter(|entry| is_clearxr_layer_entry(&entry.manifest_path, layer_manifest_path))
        .collect()
}

#[cfg(windows)]
fn remove_matching_layer_registrations(
    root: &Key,
    scope: &str,
    layer_manifest_path: &Path,
) -> Result<Vec<PathBuf>> {
    let key = match root.create(OPENXR_LAYER_REGISTRY_KEY) {
        Ok(key) => key,
        Err(_) => return Ok(Vec::new()),
    };
    let entries = read_layer_registry_entries(&key)?;
    let matches = find_matching_layer_entries(&entries, layer_manifest_path);
    let mut removed = Vec::new();

    for entry in matches {
        info!(
            "Removing Clear XR OpenXR layer registration from {} ({})",
            scope,
            entry.manifest_path.display()
        );
        key.remove_value(&entry.registry_name)?;
        removed.push(entry.manifest_path.clone());
    }

    Ok(removed)
}

#[cfg(windows)]
fn cleanup_empty_layer_registry_key(root: &Key, scope: &str) -> Result<()> {
    let key = match root.open(OPENXR_LAYER_REGISTRY_KEY) {
        Ok(key) => key,
        Err(_) => return Ok(()),
    };

    let has_values = key.values()?.next().is_some();
    let has_subkeys = key.keys()?.next().is_some();
    if has_values || has_subkeys {
        return Ok(());
    }

    info!(
        "Removing empty OpenXR implicit-layer registry key from {}",
        scope
    );
    root.remove_tree(OPENXR_LAYER_REGISTRY_KEY)?;
    Ok(())
}

#[cfg(windows)]
fn is_clearxr_layer_entry(entry_path: &Path, current_layer_manifest_path: &Path) -> bool {
    if paths_match(entry_path, current_layer_manifest_path) {
        return true;
    }

    entry_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(LAYER_MANIFEST_NAME))
}

fn path_to_registry_string(path: &Path) -> Result<String> {
    let absolute = match path.canonicalize() {
        Ok(path) => path,
        Err(_) => path.to_path_buf(),
    };
    Ok(strip_verbatim_prefix(&absolute).display().to_string())
}

#[cfg(windows)]
fn request_layer_deregistration_elevation(layer_manifest_path: &Path) -> Result<()> {
    run_elevated_helper(DEREGISTER_LAYER_ELEVATED_ARG, layer_manifest_path).with_context(|| {
        format!(
            "failed to remove machine-wide Clear XR OpenXR layer registrations for {}",
            layer_manifest_path.display()
        )
    })
}

#[cfg(windows)]
fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(iter::once(0)).collect()
}

#[cfg(windows)]
fn escape_windows_argument(path: &Path) -> String {
    path.as_os_str().to_string_lossy().replace('"', "\\\"")
}

#[cfg(windows)]
fn run_elevated_helper(command_arg: &str, manifest_path: &Path) -> Result<()> {
    let exe_path =
        std::env::current_exe().context("failed to resolve the current executable path")?;
    let parameters = format!(
        "{} \"{}\"",
        command_arg,
        escape_windows_argument(manifest_path)
    );
    let file = wide_null(exe_path.as_os_str());
    let verb = wide_null(std::ffi::OsStr::new("runas"));
    let params = wide_null(std::ffi::OsStr::new(&parameters));

    let mut execute_info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: 0 as HWND,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ptr(),
        lpDirectory: std::ptr::null(),
        nShow: SW_SHOWNORMAL,
        hInstApp: std::ptr::null_mut(),
        lpIDList: std::ptr::null_mut(),
        lpClass: std::ptr::null(),
        hkeyClass: std::ptr::null_mut(),
        dwHotKey: 0,
        Anonymous: Default::default(),
        hProcess: 0 as HANDLE,
    };

    let launched = unsafe { ShellExecuteExW(&mut execute_info) };
    if launched == 0 {
        let error_code = unsafe { GetLastError() };
        return Err(anyhow!(
            "failed to request administrator rights (Windows error {})",
            error_code
        ));
    }

    let process_handle = execute_info.hProcess;
    if process_handle.is_null() {
        return Err(anyhow!("administrator-elevated helper did not return a process handle"));
    }

    let wait_result = unsafe { WaitForSingleObject(process_handle, INFINITE) };
    if wait_result == u32::MAX {
        unsafe { CloseHandle(process_handle) };
        return Err(anyhow!(
            "failed while waiting for the administrator-elevated helper to finish"
        ));
    }

    let mut exit_code = 0u32;
    let exit_code_result = unsafe { GetExitCodeProcess(process_handle, &mut exit_code) };
    unsafe { CloseHandle(process_handle) };
    if exit_code_result == 0 {
        return Err(anyhow!(
            "failed to get the exit code from the administrator-elevated helper"
        ));
    }
    if exit_code != 0 {
        return Err(anyhow!(
            "administrator-elevated helper failed with exit code {}",
            exit_code
        ));
    }

    Ok(())
}

fn path_from_registry_string(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn paths_match(left: &Path, right: &Path) -> bool {
    normalize_for_compare(left) == normalize_for_compare(right)
}

fn normalize_for_compare(path: &Path) -> String {
    let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim_prefix(&normalized)
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

fn compare_release_paths(left: &Path, right: &Path) -> Ordering {
    compare_release_versions(
        left.parent()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
        right
            .parent()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
    )
}

fn runtime_manifest_search_roots(app_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(profile_dir_name) = app_dir.file_name().and_then(|name| name.to_str()) {
        if matches!(profile_dir_name, "debug" | "release") {
            if let Some(target_root) = app_dir.parent() {
                if profile_dir_name != "release" {
                    roots.push(target_root.join("release"));
                }
                roots.push(app_dir.to_path_buf());
                if profile_dir_name != "debug" {
                    roots.push(target_root.join("debug"));
                }
            }
        }
    }

    if !roots.iter().any(|root| root == app_dir) {
        roots.push(app_dir.to_path_buf());
    }

    roots
}

fn find_latest_runtime_manifest_in_dir(releases_dir: &Path) -> Result<Option<PathBuf>> {
    if !releases_dir.exists() {
        return Ok(None);
    }

    let mut manifests = Vec::new();
    for entry in fs::read_dir(releases_dir)
        .with_context(|| format!("failed to read {}", releases_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let manifest_path = entry.path().join(RUNTIME_MANIFEST_NAME);
            if manifest_path.exists() {
                manifests.push(manifest_path);
            }
        }
    }

    manifests.sort_by(|left, right| compare_release_paths(right, left));
    Ok(manifests.into_iter().next())
}

fn compare_release_versions(left: &str, right: &str) -> Ordering {
    let left_components = version_components(left);
    let right_components = version_components(right);
    let max_len = left_components.len().max(right_components.len());

    for index in 0..max_len {
        let left_component = left_components.get(index).copied().unwrap_or_default();
        let right_component = right_components.get(index).copied().unwrap_or_default();
        match left_component.cmp(&right_component) {
            Ordering::Equal => continue,
            non_equal => return non_equal,
        }
    }

    left.cmp(right)
}

fn version_components(version: &str) -> Vec<u32> {
    version
        .split('.')
        .map(|component| component.parse::<u32>().unwrap_or_default())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        compare_release_versions, normalize_for_compare, runtime_manifest_search_roots,
    };

    #[test]
    fn release_versions_are_compared_numerically() {
        assert!(compare_release_versions("6.0.10", "6.0.4").is_gt());
        assert!(compare_release_versions("6.1.0", "6.0.99").is_gt());
    }

    #[test]
    fn path_normalization_ignores_case_and_separator_style() {
        let left = std::path::Path::new(r"C:/Temp/ClearXR/Server/openxr_cloudxr.json");
        let right = std::path::Path::new(r"c:\temp\clearxr\server\openxr_cloudxr.json");

        assert_eq!(normalize_for_compare(left), normalize_for_compare(right));
    }

    #[test]
    fn runtime_manifest_prefers_release_root_over_debug_root() {
        let app_dir = std::path::Path::new(r"C:\repo\clearxr-streamer\target\debug");
        let roots = runtime_manifest_search_roots(app_dir);

        assert_eq!(
            roots[0],
            std::path::PathBuf::from(r"C:\repo\clearxr-streamer\target\release")
        );
        assert_eq!(
            roots[1],
            std::path::PathBuf::from(r"C:\repo\clearxr-streamer\target\debug")
        );
    }

}

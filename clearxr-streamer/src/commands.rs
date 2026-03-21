use log::info;
use tauri::State;

use crate::app_state::AppState;
use crate::bonjour::BonjourService;
use crate::cloudxr::CloudXrService;
use crate::models::{
    AppConfig, BarcodePayload, ConnectionHealth, OpenXrRegistrationStatus, RuntimeSnapshot,
    StatusBlock,
};
use crate::openxr_registration::{
    deregister_openxr_layer as deregister_openxr_layer_impl,
    get_openxr_registration_status as get_openxr_registration_status_impl,
    register_openxr_runtime_and_layer as register_openxr_runtime_and_layer_impl,
};
use crate::qr::render_pairing_qr_data_url;
use crate::server_id::get_server_id_with_fallback;
use crate::session_management::SessionManagementService;
use crate::settings::ensure_settings_file;

#[tauri::command]
pub async fn bootstrap_app_state(state: State<'_, AppState>) -> Result<RuntimeSnapshot, String> {
    let (server_id, used_fallback) = get_server_id_with_fallback();
    let settings_note = match ensure_settings_file() {
        Ok(path) => format!(
            "Default app launch can be configured in {}.",
            path.display()
        ),
        Err(error) => {
            format!("The default-app launch settings file could not be prepared: {error}")
        }
    };

    Ok(state
        .update(|snapshot| {
            snapshot.server_id = Some(server_id.clone());
            snapshot.bonjour.health = ConnectionHealth::Stopped;
            snapshot.bonjour.detail = "Ready to advertise over Bonjour".to_string();
            snapshot.session_management.health = ConnectionHealth::Stopped;
            snapshot.session_management.detail =
                "Ready to start the Tokio session-management server".to_string();
            snapshot.cloudxr.health = ConnectionHealth::Stopped;
            snapshot.cloudxr.detail =
                "Ready to start NvStreamManager and connect to CloudXR".to_string();
            snapshot.notes = vec![
                "QR generation and ServerID persistence are already ported.".to_string(),
                "The Tokio session-management server can now be started from Tauri.".to_string(),
                "Bonjour advertisements can now be started from Tauri as well.".to_string(),
                "CloudXR now starts automatically with the session server and uses file-backed logs instead of GUI log windows.".to_string(),
                "clear-xr.exe can be launched automatically as the default OpenXR app once streaming is ready.".to_string(),
                settings_note.clone(),
                "Rust logs go to disk through tauri-plugin-log; the old in-memory GUI log windows are intentionally not being carried over.".to_string(),
                "mdns-sd logs are suppressed by default. Set STREAMING_SESSION_ENABLE_MDNS_LOGS=1 before launch to re-enable them.".to_string(),
            ];

            if used_fallback {
                snapshot.notes.push(
                    "Registry access was unavailable, so this run is using an ephemeral ServerID."
                        .to_string(),
                );
            }
        })
        .await)
}

#[tauri::command]
pub async fn get_runtime_snapshot(state: State<'_, AppState>) -> Result<RuntimeSnapshot, String> {
    Ok(state.snapshot().await)
}

#[tauri::command]
pub fn get_default_config() -> AppConfig {
    AppConfig::default()
}

#[tauri::command]
pub fn get_local_ip_addresses() -> Result<Vec<String>, String> {
    let mut addresses: Vec<String> = if_addrs::get_if_addrs()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(|interface| match interface.addr.ip() {
            std::net::IpAddr::V4(ip)
                if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() =>
            {
                Some(ip.to_string())
            }
            _ => None,
        })
        .collect();

    addresses.sort();
    addresses.dedup();

    if addresses.is_empty() {
        addresses.push("127.0.0.1".to_string());
    }

    Ok(addresses)
}

#[tauri::command]
pub fn generate_pairing_qr_preview(
    client_token: String,
    certificate_fingerprint: String,
) -> Result<String, String> {
    let payload = BarcodePayload {
        client_token,
        certificate_fingerprint,
    };

    render_pairing_qr_data_url(&payload).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn get_openxr_registration_status() -> Result<OpenXrRegistrationStatus, String> {
    get_openxr_registration_status_impl().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn register_openxr_runtime_and_layer() -> Result<OpenXrRegistrationStatus, String> {
    register_openxr_runtime_and_layer_impl().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn deregister_openxr_layer() -> Result<OpenXrRegistrationStatus, String> {
    deregister_openxr_layer_impl().map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn start_cloudxr(state: State<'_, AppState>) -> Result<RuntimeSnapshot, String> {
    let app_state = state.inner().clone();

    if app_state.has_cloudxr().await {
        return Err("CloudXR is already running.".to_string());
    }

    let service = CloudXrService::start(app_state.clone())
        .await
        .map_err(|error| error.to_string())?;
    app_state.replace_cloudxr(Some(service)).await;

    Ok(app_state.snapshot().await)
}

#[tauri::command]
pub async fn stop_cloudxr(state: State<'_, AppState>) -> Result<RuntimeSnapshot, String> {
    let app_state = state.inner().clone();

    if let Some(service) = app_state.replace_cloudxr(None).await {
        service.stop().await;
    }

    Ok(app_state
        .update(|snapshot| {
            snapshot.cloudxr = StatusBlock::stopped("Not started");
        })
        .await)
}

#[tauri::command]
pub async fn start_session_management(
    state: State<'_, AppState>,
    config: AppConfig,
) -> Result<RuntimeSnapshot, String> {
    let app_state = state.inner().clone();
    let mut started_cloudxr = false;

    if app_state.has_session_management().await {
        return Err("The session-management server is already running.".to_string());
    }

    if !app_state.has_cloudxr().await {
        info!("Starting CloudXR automatically as part of session-server startup.");
        let cloudxr = CloudXrService::start(app_state.clone())
            .await
            .map_err(|error| format!("failed to start CloudXR: {error}"))?;
        app_state.replace_cloudxr(Some(cloudxr)).await;
        started_cloudxr = true;
    }

    let service = SessionManagementService::start(app_state.clone(), config.clone()).await;
    let service = match service {
        Ok(service) => service,
        Err(error) => {
            if started_cloudxr {
                if let Some(cloudxr) = app_state.replace_cloudxr(None).await {
                    cloudxr.stop().await;
                }
            }
            return Err(error.to_string());
        }
    };
    let local_addr = service.local_addr();

    app_state.replace_session_management(Some(service)).await;

    Ok(app_state
        .update(|snapshot| {
            snapshot.config = AppConfig {
                host_address: local_addr.ip().to_string(),
                port: local_addr.port(),
                ..config.clone()
            };
            snapshot.session_management.health = ConnectionHealth::Paused;
            snapshot.session_management.detail = format!("Listening on {local_addr}");
            snapshot.qr_data_url = None;
            if snapshot.cloudxr.detail == "Not started" {
                snapshot.cloudxr.detail =
                    "NvStreamManager launched. Waiting for CloudXR service.".to_string();
            }
        })
        .await)
}

#[tauri::command]
pub async fn start_server(
    state: State<'_, AppState>,
    config: AppConfig,
) -> Result<RuntimeSnapshot, String> {
    let app_state = state.inner().clone();

    if app_state.has_session_management().await || app_state.has_bonjour().await {
        return Err("The Clear XR Server is already running.".to_string());
    }

    let bonjour = BonjourService::start(&config).map_err(|error| error.to_string())?;
    app_state.replace_bonjour(Some(bonjour)).await;
    let _ = app_state
        .update(|snapshot| {
            snapshot.config = config.clone();
            snapshot.bonjour.health = ConnectionHealth::Running;
            snapshot.bonjour.detail = format!(
                "Advertising {}:{} for bundle {}",
                config.host_address, config.port, config.bundle_id
            );
        })
        .await;

    let session_result = start_session_management(state, config.clone()).await;
    match session_result {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => {
            if let Some(service) = app_state.replace_bonjour(None).await {
                service.stop();
            }
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn stop_session_management(
    state: State<'_, AppState>,
) -> Result<RuntimeSnapshot, String> {
    let app_state = state.inner().clone();

    if let Some(service) = app_state.replace_session_management(None).await {
        service.stop().await;
    }

    if let Some(service) = app_state.replace_cloudxr(None).await {
        info!("Stopping CloudXR automatically as part of session-server shutdown.");
        service.stop().await;
    }

    Ok(app_state
        .update(|snapshot| {
            snapshot.session_management = StatusBlock::stopped("Not started");
            snapshot.cloudxr = StatusBlock::stopped("Not started");
            snapshot.qr_data_url = None;
        })
        .await)
}

#[tauri::command]
pub async fn stop_server(state: State<'_, AppState>) -> Result<RuntimeSnapshot, String> {
    let app_state = state.inner().clone();
    let snapshot = stop_session_management(state).await?;

    if let Some(service) = app_state.replace_bonjour(None).await {
        service.stop();
    }

    Ok(app_state
        .update(|current| {
            current.bonjour = StatusBlock::stopped("Not started");
            current.session_management = snapshot.session_management.clone();
            current.cloudxr = snapshot.cloudxr.clone();
            current.qr_data_url = None;
        })
        .await)
}

#[tauri::command]
pub async fn start_bonjour(
    state: State<'_, AppState>,
    config: AppConfig,
) -> Result<RuntimeSnapshot, String> {
    let app_state = state.inner().clone();

    if app_state.has_bonjour().await {
        return Err("Bonjour advertising is already running.".to_string());
    }

    let service = BonjourService::start(&config).map_err(|error| error.to_string())?;
    app_state.replace_bonjour(Some(service)).await;

    Ok(app_state
        .update(|snapshot| {
            snapshot.config = config.clone();
            snapshot.bonjour.health = ConnectionHealth::Running;
            snapshot.bonjour.detail = format!(
                "Advertising {}:{} for bundle {}",
                config.host_address, config.port, config.bundle_id
            );
        })
        .await)
}

#[tauri::command]
pub async fn stop_bonjour(state: State<'_, AppState>) -> Result<RuntimeSnapshot, String> {
    let app_state = state.inner().clone();

    if let Some(service) = app_state.replace_bonjour(None).await {
        service.stop();
    }

    Ok(app_state
        .update(|snapshot| {
            snapshot.bonjour = StatusBlock::stopped("Not started");
        })
        .await)
}

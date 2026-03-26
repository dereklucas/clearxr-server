mod app_state;
mod bonjour;
mod cloudxr;
mod commands;
pub mod dashboard_service;
mod job_object;
mod models;
mod openxr_registration;
mod protocol;
mod qr;
mod server_id;
mod session_management;
mod settings;

use app_state::AppState;
use anyhow::{anyhow, Result};
use tauri_plugin_log::log::LevelFilter;
use tauri_plugin_log::{Target, TargetKind};

const ENABLE_MDNS_LOGS_ENV: &str = "STREAMING_SESSION_ENABLE_MDNS_LOGS";
const REGISTER_RUNTIME_ELEVATED_ARG: &str = "--clearxr-register-runtime-elevated";
const DEREGISTER_LAYER_ELEVATED_ARG: &str = "--clearxr-deregister-layer-elevated";

fn mdns_logs_enabled() -> bool {
    std::env::var(ENABLE_MDNS_LOGS_ENV)
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

pub fn handle_startup_mode() -> Result<bool> {
    let mut args = std::env::args_os();
    let _program = args.next();

    match args.next().as_deref() {
        Some(arg) if arg == std::ffi::OsStr::new(REGISTER_RUNTIME_ELEVATED_ARG) => {
            let manifest_path = args
                .next()
                .ok_or_else(|| anyhow!("missing runtime manifest path for elevated registration"))?;
            openxr_registration::register_runtime_from_elevated_helper(std::path::PathBuf::from(
                manifest_path,
            ))?;
            Ok(true)
        }
        Some(arg) if arg == std::ffi::OsStr::new(DEREGISTER_LAYER_ELEVATED_ARG) => {
            let manifest_path = args
                .next()
                .ok_or_else(|| anyhow!("missing layer manifest path for elevated deregistration"))?;
            openxr_registration::deregister_layer_from_elevated_helper(std::path::PathBuf::from(
                manifest_path,
            ))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut log_builder = tauri_plugin_log::Builder::new()
        .clear_targets()
        .target(Target::new(TargetKind::Stdout))
        .target(Target::new(TargetKind::LogDir {
            file_name: Some("streaming-session".to_string()),
        }))
        .level_for("mio", LevelFilter::Info)
        .level_for("polling", LevelFilter::Info);

    if !mdns_logs_enabled() {
        log_builder = log_builder.level_for("mdns_sd", LevelFilter::Off);
    }

    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(log_builder.build())
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap_app_state,
            commands::deregister_openxr_layer,
            commands::get_default_config,
            commands::get_local_ip_addresses,
            commands::get_openxr_registration_status,
            commands::get_runtime_snapshot,
            commands::generate_pairing_qr_preview,
            commands::register_openxr_runtime_and_layer,
            commands::start_bonjour,
            commands::start_cloudxr,
            commands::start_server,
            commands::start_session_management,
            commands::stop_bonjour,
            commands::stop_cloudxr,
            commands::stop_server,
            commands::stop_session_management,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Clear XR");
}

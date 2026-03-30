use std::io;
use std::net::{IpAddr, SocketAddr};
use std::process::{Child as StdChild, Command as StdCommand};
use std::sync::Arc;

use anyhow::{Context, Result};
use log::{info, warn};
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::models::{AppConfig, BarcodePayload, ConnectionHealth, SessionInformation};
use crate::protocol::{
    read_frame, write_frame, AcknowledgeBarcodePresentationMessage, AcknowledgeConnectionMessage,
    EventEnvelope, MediaStreamIsReadyMessage, RequestBarcodePresentationMessage,
    RequestConnectionMessage, RequestSessionDisconnectMessage, SessionStatusDidChangeMessage,
    SESSION_STATUS_CONNECTED, SESSION_STATUS_CONNECTING, SESSION_STATUS_DISCONNECTED,
    SESSION_STATUS_PAUSED, SESSION_STATUS_WAITING, SUPPORTED_PROTOCOL_VERSION,
};
use crate::qr::render_pairing_qr_data_url;
use crate::server_id::get_server_id_with_fallback;
use crate::settings::load_settings;

struct SessionRuntimeState {
    previous_session_status: Option<String>,
    clearxr_process: Option<StdChild>,
    dashboard: Option<clearxr_dashboard::DashboardService>,
    fallback_session: Option<clearxr_dashboard::fallback_session::FallbackSession>,
}

impl Default for SessionRuntimeState {
    fn default() -> Self {
        Self {
            previous_session_status: None,
            clearxr_process: None,
            dashboard: None,
            fallback_session: None,
        }
    }
}

pub struct SessionManagementService {
    local_addr: SocketAddr,
    shutdown_tx: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl SessionManagementService {
    pub async fn start(app_state: AppState, config: AppConfig) -> Result<Self> {
        let (server_id, used_fallback) = get_server_id_with_fallback();

        if used_fallback {
            let _ = app_state
                .update(|snapshot| {
                    if !snapshot.notes.iter().any(|note| {
                        note == "Registry access was unavailable, so this run is using an ephemeral ServerID."
                    }) {
                        snapshot.notes.push(
                            "Registry access was unavailable, so this run is using an ephemeral ServerID."
                                .to_string(),
                        );
                    }
                })
                .await;
        }

        info!(
            "Starting session-management listener on {}:{}",
            config.host_address, config.port
        );

        Self::start_with_server_id(app_state, config, server_id).await
    }

    async fn start_with_server_id(
        app_state: AppState,
        config: AppConfig,
        server_id: String,
    ) -> Result<Self> {
        let runtime_state = Arc::new(Mutex::new(SessionRuntimeState::default()));
        let bind_ip: IpAddr = config
            .host_address
            .parse()
            .with_context(|| format!("invalid host address '{}'", config.host_address))?;
        let listener = TcpListener::bind(SocketAddr::new(bind_ip, config.port))
            .await
            .context("failed to bind the session-management listener")?;
        let local_addr = listener.local_addr()?;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(run_listener(
            listener,
            app_state,
            config,
            server_id,
            runtime_state,
            shutdown_rx,
        ));

        Ok(Self {
            local_addr,
            shutdown_tx,
            task,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub async fn stop(self) {
        info!(
            "Stopping session-management listener on {}",
            self.local_addr
        );
        let _ = self.shutdown_tx.send(true);
        let _ = self.task.await;
    }

    #[cfg(test)]
    pub async fn start_for_tests(app_state: AppState, config: AppConfig) -> Result<Self> {
        Self::start_with_server_id(app_state, config, "test-server-id".to_string()).await
    }
}

async fn run_listener(
    listener: TcpListener,
    app_state: AppState,
    config: AppConfig,
    server_id: String,
    runtime_state: Arc<Mutex<SessionRuntimeState>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let local_addr = listener
        .local_addr()
        .ok()
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], config.port)));

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow() {
                    break;
                }
            }
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, peer_addr)) => {
                        info!("Session-management client connected: {peer_addr}");
                        let _ = app_state.update(|snapshot| {
                            snapshot.session_management.health = ConnectionHealth::Paused;
                            snapshot.session_management.detail = format!("Client connected: {peer_addr}");
                        }).await;

                        if let Err(error) = handle_connection(
                            stream,
                            peer_addr,
                            local_addr,
                            app_state.clone(),
                            config.clone(),
                            server_id.clone(),
                            runtime_state.clone(),
                            shutdown_rx.clone(),
                        ).await {
                            warn!("Session-management connection ended with error: {error}");
                            let _ = app_state.update(|snapshot| {
                                snapshot.session_management.health = ConnectionHealth::Stopped;
                                snapshot.session_management.detail = format!("Session error: {error}");
                                snapshot.qr_data_url = None;
                            }).await;
                        }
                    }
                    Err(error) => {
                        warn!("Session-management listener accept failed: {error}");
                        let _ = app_state.update(|snapshot| {
                            snapshot.session_management.health = ConnectionHealth::Stopped;
                            snapshot.session_management.detail = format!("Listener error: {error}");
                        }).await;
                        break;
                    }
                }
            }
        }
    }

    let _ = app_state
        .update(|snapshot| {
            snapshot.session_management.health = ConnectionHealth::Stopped;
            snapshot.session_management.detail = "Not started".to_string();
            snapshot.qr_data_url = None;
        })
        .await;
}

async fn handle_connection(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    local_addr: SocketAddr,
    app_state: AppState,
    config: AppConfig,
    server_id: String,
    runtime_state: Arc<Mutex<SessionRuntimeState>>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let mut current_session: Option<SessionInformation> = None;

    loop {
        let payload = tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow() {
                    if let Some(session) = current_session.as_ref() {
                        let _ = send_disconnect_message(&mut stream, &session.session_id).await;
                    }
                    break;
                }
                continue;
            }
            read_result = read_frame(&mut stream) => {
                match read_result {
                    Ok(payload) => payload,
                    Err(error) if is_connection_close(&error) => break,
                    Err(error) => return Err(error.into()),
                }
            }
        };

        let envelope: EventEnvelope = match serde_json::from_slice(&payload) {
            Ok(envelope) => envelope,
            Err(_) => continue,
        };

        let Some(session_id) = envelope.session_id.clone() else {
            continue;
        };

        if envelope.event != "RequestConnection"
            && current_session
                .as_ref()
                .map(|session| session.session_id.as_str())
                != Some(session_id.as_str())
        {
            send_disconnect_message(&mut stream, &session_id).await?;
            continue;
        }

        match envelope.event.as_str() {
            "RequestConnection" => {
                let request: RequestConnectionMessage = serde_json::from_slice(&payload)?;
                info!(
                    "Received RequestConnection for session {} from client {} ({peer_addr})",
                    request.session_id, request.client_id
                );

                if current_session.is_some() {
                    send_disconnect_message(&mut stream, &request.session_id).await?;
                    continue;
                }

                if request.protocol_version != SUPPORTED_PROTOCOL_VERSION {
                    send_disconnect_message(&mut stream, &request.session_id).await?;
                    return Err(anyhow::anyhow!(
                        "unsupported protocol version {}",
                        request.protocol_version
                    ));
                }

                let barcode = match app_state.cloudxr().await {
                    Some(cloudxr) => match cloudxr.generate_barcode(&request.client_id).await {
                        Ok(barcode) => barcode,
                        Err(error) => {
                            warn!(
                                "CloudXR barcode generation failed, falling back to stub data: {error}"
                            );
                            generate_stub_barcode(&request.client_id)
                        }
                    },
                    None => generate_stub_barcode(&request.client_id),
                };
                let session = SessionInformation {
                    session_id: request.session_id.clone(),
                    client_id: request.client_id.clone(),
                    barcode: barcode.clone(),
                };

                let acknowledge = AcknowledgeConnectionMessage {
                    event: "AcknowledgeConnection".to_string(),
                    session_id: request.session_id.clone(),
                    server_id: server_id.clone(),
                    certificate_fingerprint: (!config.force_qr_code)
                        .then_some(barcode.certificate_fingerprint.clone()),
                };

                send_json(&mut stream, &acknowledge).await?;
                info!(
                    "Sent AcknowledgeConnection for session {} to client {}",
                    request.session_id, request.client_id
                );
                current_session = Some(session.clone());

                let _ = app_state
                    .update(|snapshot| {
                        snapshot.session_management.health = ConnectionHealth::Paused;
                        snapshot.session_management.detail = format!(
                            "Accepted session {} from {} ({peer_addr})",
                            session.session_id, session.client_id
                        );
                        snapshot.qr_data_url = None;
                    })
                    .await;
            }
            "RequestBarcodePresentation" => {
                let request: RequestBarcodePresentationMessage = serde_json::from_slice(&payload)?;
                let Some(session) = current_session.as_ref() else {
                    continue;
                };
                info!(
                    "Received RequestBarcodePresentation for session {} from client {}",
                    request.session_id, session.client_id
                );

                let qr_data_url = render_pairing_qr_data_url(&session.barcode)?;
                let acknowledge = AcknowledgeBarcodePresentationMessage {
                    event: "AcknowledgeBarcodePresentation".to_string(),
                    session_id: request.session_id,
                };

                send_json(&mut stream, &acknowledge).await?;
                info!(
                    "Displayed pairing QR for session {} and client {}",
                    session.session_id, session.client_id
                );
                let _ = app_state
                    .update(|snapshot| {
                        snapshot.session_management.health = ConnectionHealth::Paused;
                        snapshot.session_management.detail =
                            format!("Presenting pairing QR for {}", session.client_id);
                        snapshot.qr_data_url = Some(qr_data_url.clone());
                    })
                    .await;
            }
            "SessionStatusDidChange" => {
                let status: SessionStatusDidChangeMessage = serde_json::from_slice(&payload)?;
                info!(
                    "Received SessionStatusDidChange for session {} -> {}",
                    status.session_id, status.status
                );

                let previous_status = {
                    let mut runtime_state = runtime_state.lock().await;
                    let previous_status = runtime_state.previous_session_status.clone();
                    runtime_state.previous_session_status = Some(status.status.clone());
                    previous_status
                };

                if let Some(cloudxr) = app_state.cloudxr().await {
                    cloudxr
                        .set_session_paused(status.status == SESSION_STATUS_PAUSED)
                        .await;
                }
                apply_status_update(&app_state, &status).await;

                if status.status == SESSION_STATUS_WAITING {
                    let cloudxr = app_state.cloudxr().await.ok_or_else(|| {
                        anyhow::anyhow!(
                            "CloudXR service was not running when session {} entered WAITING",
                            status.session_id
                        )
                    })?;
                    info!(
                        "Starting CloudXR presentation for session {} after WAITING",
                        status.session_id
                    );
                    cloudxr.start_presentation().await.with_context(|| {
                        format!(
                            "failed to start the CloudXR presentation for session {}",
                            status.session_id
                        )
                    })?;

                    let ready_message = MediaStreamIsReadyMessage {
                        event: "MediaStreamIsReady".to_string(),
                        session_id: status.session_id.clone(),
                    };
                    send_json(&mut stream, &ready_message).await?;
                    info!("Sent MediaStreamIsReady to the Vision Pro");
                    let _ = app_state
                        .update(|snapshot| {
                            snapshot.session_management.health = ConnectionHealth::Paused;
                            snapshot.session_management.detail = format!(
                                "WAITING for session {}. Sent MediaStreamIsReady.",
                                status.session_id
                            );
                            snapshot.qr_data_url = None;
                        })
                        .await;
                    spawn_default_app_launch_if_enabled(app_state.clone(), runtime_state.clone());
                } else if status.status == SESSION_STATUS_CONNECTED
                    && previous_status.as_deref() == Some(SESSION_STATUS_PAUSED)
                {
                    info!(
                        "Session {} resumed from PAUSED to CONNECTED, checking whether clear-xr should launch",
                        status.session_id
                    );
                    spawn_default_app_launch_if_enabled(app_state.clone(), runtime_state.clone());
                } else if status.status == SESSION_STATUS_DISCONNECTED {
                    if let Some(cloudxr) = app_state.cloudxr().await {
                        info!(
                            "Stopping CloudXR presentation for session {} after DISCONNECTED",
                            status.session_id
                        );
                        if let Err(error) = cloudxr.stop_presentation().await {
                            warn!("Failed to stop CloudXR presentation: {error}");
                        }
                    }

                    current_session = None;
                }
            }
            _ => {}
        }
    }

    let _ = app_state
        .update(|snapshot| {
            snapshot.session_management.health = ConnectionHealth::Paused;
            snapshot.session_management.detail = format!("Listening on {local_addr}");
            snapshot.qr_data_url = None;
        })
        .await;

    Ok(())
}

async fn apply_status_update(app_state: &AppState, status: &SessionStatusDidChangeMessage) {
    info!(
        "Session {} moved to status {}",
        status.session_id, status.status
    );
    let _ = app_state
        .update(|snapshot| {
            snapshot.qr_data_url = None;
            match status.status.as_str() {
                SESSION_STATUS_WAITING => {
                    snapshot.session_management.health = ConnectionHealth::Paused;
                    snapshot.session_management.detail = format!(
                        "WAITING for session {}. Starting CloudXR presentation.",
                        status.session_id
                    );
                }
                SESSION_STATUS_CONNECTING => {
                    snapshot.session_management.health = ConnectionHealth::Paused;
                    snapshot.session_management.detail =
                        format!("CONNECTING session {}", status.session_id);
                }
                SESSION_STATUS_CONNECTED => {
                    snapshot.session_management.health = ConnectionHealth::Running;
                    snapshot.session_management.detail =
                        format!("CONNECTED session {}", status.session_id);
                }
                SESSION_STATUS_PAUSED => {
                    snapshot.session_management.health = ConnectionHealth::Paused;
                    snapshot.session_management.detail =
                        format!("PAUSED session {}", status.session_id);
                }
                SESSION_STATUS_DISCONNECTED => {
                    snapshot.session_management.health = ConnectionHealth::Stopped;
                    snapshot.session_management.detail =
                        format!("DISCONNECTED session {}", status.session_id);
                }
                other => {
                    snapshot.session_management.health = ConnectionHealth::Paused;
                    snapshot.session_management.detail =
                        format!("Unknown session status '{other}' for {}", status.session_id);
                }
            }
        })
        .await;
}

async fn send_json<T>(stream: &mut TcpStream, value: &T) -> Result<()>
where
    T: Serialize,
{
    let payload = serde_json::to_vec(value)?;
    write_frame(stream, &payload).await?;
    Ok(())
}

async fn send_disconnect_message(stream: &mut TcpStream, session_id: &str) -> Result<()> {
    let disconnect = RequestSessionDisconnectMessage {
        event: "RequestSessionDisconnect".to_string(),
        session_id: session_id.to_string(),
    };
    send_json(stream, &disconnect).await
}

fn generate_stub_barcode(client_id: &str) -> BarcodePayload {
    let suffix = Uuid::new_v4().simple().to_string();
    BarcodePayload {
        client_token: format!("stub-token-{suffix}"),
        certificate_fingerprint: format!("stub-sha256-{client_id}-{suffix}"),
    }
}

fn is_connection_close(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
    )
}

fn spawn_default_app_launch_if_enabled(
    app_state: AppState,
    runtime_state: Arc<Mutex<SessionRuntimeState>>,
) {
    tokio::spawn(async move {
        let (settings, settings_path) = match load_settings() {
            Ok(result) => result,
            Err(error) => {
                warn!("Skipping clear-xr launch because the settings could not be loaded: {error}");
                return;
            }
        };

        if !settings.launch_default_app {
            info!(
                "Skipping clear-xr launch because launchDefaultApp is false in {}.",
                settings_path.display()
            );
            return;
        }

        sleep(Duration::from_secs(settings.clearxr_launch_delay_seconds)).await;

        let Some(cloudxr) = app_state.cloudxr().await else {
            warn!("Skipping clear-xr launch because CloudXR is unavailable.");
            return;
        };

        match cloudxr.query_status_once().await {
            Ok(status) if status.game_is_connected => {
                info!("Skipping clear-xr launch because an OpenXR app is already connected.");
                return;
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    "Skipping clear-xr launch because CloudXR status could not be queried: {error}"
                );
                return;
            }
        }

        let runtime_state_arc = runtime_state.clone();
        let mut runtime_state = runtime_state.lock().await;
        if let Some(process) = runtime_state.clearxr_process.as_mut() {
            match process.try_wait() {
                Ok(None) => {
                    info!("Skipping clear-xr launch because it is already running.");
                    return;
                }
                Ok(Some(_)) => {
                    runtime_state.clearxr_process = None;
                }
                Err(error) => {
                    warn!("Failed checking existing clear-xr process: {error}");
                    runtime_state.clearxr_process = None;
                }
            }
        }

        let clearxr_exe_path = resolve_clearxr_exe_path(&settings.clearxr_exe_path);
        if !clearxr_exe_path.exists() {
            warn!(
                "Skipping clear-xr launch because '{}' does not exist.",
                clearxr_exe_path.display()
            );
            return;
        }

        // Start the dashboard rendering service (SHM + named pipe for layer overlay)
        if runtime_state.dashboard.is_none() {
            match crate::dashboard_service::start() {
                Ok(service) => {
                    runtime_state.dashboard = Some(service);
                }
                Err(error) => {
                    warn!("Failed to start dashboard service: {error}");
                }
            }
        }

        // Start fallback OpenXR session (keeps dashboard visible when no game is running).
        // The layer auto-loads as an implicit API layer and injects the dashboard quad.
        if runtime_state.fallback_session.is_none() {
            match clearxr_dashboard::fallback_session::FallbackSession::start() {
                Ok(fb) => {
                    runtime_state.fallback_session = Some(fb);
                    // Give the fallback session a moment to create the XR session
                    drop(runtime_state);
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    runtime_state = runtime_state_arc.lock().await;
                }
                Err(error) => {
                    warn!("Failed to start fallback session: {error}");
                }
            }
        }

        // Yield the fallback session so Space can create its own
        if let Some(ref fb) = runtime_state.fallback_session {
            fb.yield_session();
            // Give the runtime a moment to tear down
            drop(runtime_state);
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            runtime_state = runtime_state_arc.lock().await;
        }

        match StdCommand::new(&clearxr_exe_path).spawn() {
            Ok(child) => {
                info!("Started clear-xr.exe with pid {}", child.id());
                runtime_state.clearxr_process = Some(child);
            }
            Err(error) => {
                warn!("Failed to start clear-xr.exe: {error}");
                // Space failed to launch — reclaim the fallback session
                if let Some(ref fb) = runtime_state.fallback_session {
                    fb.reclaim_session();
                }
            }
        }

        // Drop the lock before the polling loop
        drop(runtime_state);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            let actions = {
                let rt = runtime_state_arc.lock().await;
                rt.dashboard.as_ref().map(|d| d.drain_actions()).unwrap_or_default()
            };

            for action in actions {
                match action {
                    clearxr_dashboard::dashboard::DashboardAction::LaunchGame(app_id) => {
                        info!("Dashboard requested game launch: app_id={}", app_id);
                        let mut rt = runtime_state_arc.lock().await;
                        // Kill Space first
                        if let Some(ref mut process) = rt.clearxr_process {
                            info!("Killing Space (pid {}) for game launch.", process.id());
                            let _ = process.kill();
                            let _ = process.wait();
                            rt.clearxr_process = None;
                        }
                        // Reclaim fallback session (dashboard stays visible during transition)
                        if let Some(ref fb) = rt.fallback_session {
                            fb.reclaim_session();
                        }
                        drop(rt);
                        // Give the fallback session time to create the XR session
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

                        // Yield fallback session for the game
                        let rt = runtime_state_arc.lock().await;
                        if let Some(ref fb) = rt.fallback_session {
                            fb.yield_session();
                        }
                        drop(rt);
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                        // Launch via Steam
                        let url = format!("steam://rungameid/{}", app_id);
                        info!("Launching: {}", url);
                        let _ = StdCommand::new("cmd")
                            .args(["/C", "start", "", &url])
                            .spawn();

                        // If game doesn't connect within 30s, reclaim + re-launch Space
                        let rt_clone = runtime_state_arc.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                            let rt = rt_clone.lock().await;
                            if rt.clearxr_process.is_none() {
                                info!("Game didn't launch within 30s, reclaiming fallback session.");
                                if let Some(ref fb) = rt.fallback_session {
                                    fb.reclaim_session();
                                }
                            }
                        });
                    }
                    clearxr_dashboard::dashboard::DashboardAction::SaveConfig => {
                        info!("Dashboard requested config save.");
                    }
                    clearxr_dashboard::dashboard::DashboardAction::QuitApp => {
                        info!("Dashboard requested app quit.");
                        let mut rt = runtime_state_arc.lock().await;
                        if let Some(ref mut process) = rt.clearxr_process {
                            let _ = process.kill();
                            let _ = process.wait();
                            rt.clearxr_process = None;
                        }
                    }
                    _ => {}
                }
            }
        }
    });
}

fn resolve_clearxr_exe_path(configured_path: &str) -> std::path::PathBuf {
    let configured_path = std::path::PathBuf::from(configured_path);
    if configured_path.is_absolute() {
        return configured_path;
    }

    for base in relative_path_bases() {
        let candidate = base.join(&configured_path);
        if candidate.exists() {
            return candidate;
        }
    }

    configured_path
}

fn relative_path_bases() -> Vec<std::path::PathBuf> {
    let mut bases = Vec::new();

    if let Ok(current_dir) = std::env::current_dir() {
        push_unique_path(&mut bases, current_dir);
    }

    if let Ok(current_exe) = std::env::current_exe() {
        let mut cursor = current_exe.parent().map(|path| path.to_path_buf());
        for _ in 0..4 {
            let Some(path) = cursor else {
                break;
            };
            push_unique_path(&mut bases, path.clone());
            cursor = path.parent().map(|parent| parent.to_path_buf());
        }
    }

    bases
}

fn push_unique_path(paths: &mut Vec<std::path::PathBuf>, candidate: std::path::PathBuf) {
    if !paths.iter().any(|existing| existing == &candidate) {
        paths.push(candidate);
    }
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;
    use serde::Serialize;
    use tokio::net::TcpStream;
    use tokio::time::{sleep, timeout, Duration};

    use super::{relative_path_bases, SessionManagementService};
    use crate::app_state::AppState;
    use crate::models::{AppConfig, ConnectionHealth};
    use crate::protocol::{
        read_frame, write_frame, AcknowledgeBarcodePresentationMessage,
        AcknowledgeConnectionMessage, RequestBarcodePresentationMessage, RequestConnectionMessage,
    };

    #[tokio::test]
    async fn request_connection_receives_acknowledgement() {
        let app_state = AppState::default();
        let service = SessionManagementService::start_for_tests(
            app_state.clone(),
            AppConfig {
                host_address: "127.0.0.1".to_string(),
                port: 0,
                force_qr_code: false,
                ..AppConfig::default()
            },
        )
        .await
        .unwrap();

        let mut stream = TcpStream::connect(service.local_addr()).await.unwrap();
        write_json(
            &mut stream,
            &RequestConnectionMessage::new("session-1", "client-1"),
        )
        .await;

        let response: AcknowledgeConnectionMessage = read_json(&mut stream).await;
        assert_eq!(response.event, "AcknowledgeConnection");
        assert_eq!(response.session_id, "session-1");
        assert!(response.certificate_fingerprint.is_some());

        let snapshot = app_state.snapshot().await;
        assert_eq!(snapshot.session_management.health, ConnectionHealth::Paused);
        assert!(snapshot
            .session_management
            .detail
            .contains("Accepted session"));

        service.stop().await;
    }

    #[tokio::test]
    async fn barcode_presentation_updates_runtime_snapshot() {
        let app_state = AppState::default();
        let service = SessionManagementService::start_for_tests(
            app_state.clone(),
            AppConfig {
                host_address: "127.0.0.1".to_string(),
                port: 0,
                force_qr_code: true,
                ..AppConfig::default()
            },
        )
        .await
        .unwrap();

        let mut stream = TcpStream::connect(service.local_addr()).await.unwrap();
        write_json(
            &mut stream,
            &RequestConnectionMessage::new("session-qr", "client-qr"),
        )
        .await;

        let response: AcknowledgeConnectionMessage = read_json(&mut stream).await;
        assert!(response.certificate_fingerprint.is_none());

        write_json(
            &mut stream,
            &RequestBarcodePresentationMessage {
                event: "RequestBarcodePresentation".to_string(),
                session_id: "session-qr".to_string(),
            },
        )
        .await;

        let response: AcknowledgeBarcodePresentationMessage = read_json(&mut stream).await;
        assert_eq!(response.event, "AcknowledgeBarcodePresentation");

        let snapshot = timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = app_state.snapshot().await;
                if snapshot.qr_data_url.is_some() {
                    break snapshot;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        assert!(snapshot
            .qr_data_url
            .as_deref()
            .unwrap()
            .starts_with("data:image/png;base64,"));

        service.stop().await;
    }

    async fn write_json<T>(stream: &mut TcpStream, value: &T)
    where
        T: Serialize,
    {
        let payload = serde_json::to_vec(value).unwrap();
        write_frame(stream, &payload).await.unwrap();
    }

    async fn read_json<T>(stream: &mut TcpStream) -> T
    where
        T: DeserializeOwned,
    {
        let payload = read_frame(stream).await.unwrap();
        serde_json::from_slice(&payload).unwrap()
    }

    #[test]
    fn relative_path_bases_include_the_current_directory() {
        let current_dir = std::env::current_dir().unwrap();
        assert!(relative_path_bases().contains(&current_dir));
    }
}

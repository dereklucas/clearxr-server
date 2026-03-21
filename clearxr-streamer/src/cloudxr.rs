use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use libloading::{Library, Symbol};
use log::{info, warn};
use sysinfo::{ProcessesToUpdate, Signal, System};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use crate::app_state::AppState;
use crate::job_object::ProcessJobObject;
use crate::models::{BarcodePayload, ConnectionHealth};

const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(200);
const STATE_CHANGE_POLL_DELAY: Duration = Duration::from_millis(50);
const STATE_CHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_RESTART_DELAY: Duration = Duration::from_secs(2);
const PROCESS_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const RPC_OPERATION_RETRY_TIMEOUT: Duration = Duration::from_secs(10);
const RPC_FAILURE_WATCHDOG_THRESHOLD: usize = 5;
const TOKEN_BUFFER_LEN: usize = 256;
const FINGERPRINT_BUFFER_LEN: usize = 256;

#[derive(Debug, Clone)]
struct CloudXrPaths {
    server_dir: PathBuf,
    manager_exe: PathBuf,
    rpc_dll: PathBuf,
    runtime_json: PathBuf,
    runtime_version: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloudXrState {
    pub cloudxr_client_connected: bool,
    pub openxr_runtime_running: bool,
    pub game_is_connected: bool,
}

#[derive(Debug, Default)]
struct CloudXrControlState {
    presentation_requested: bool,
    session_paused: bool,
    previous_client_connected: bool,
    consecutive_rpc_failures: usize,
}

#[derive(Clone)]
pub struct CloudXrService {
    inner: Arc<CloudXrInner>,
}

struct CloudXrInner {
    paths: CloudXrPaths,
    child: Mutex<Option<Child>>,
    control: Mutex<CloudXrControlState>,
    job_object: ProcessJobObject,
    rpc_client: Mutex<Option<PersistentRpcClient>>,
    shutdown_tx: watch::Sender<bool>,
    poll_task: Mutex<Option<JoinHandle<()>>>,
}

type RawNvRpcHandle = *mut c_void;
type NvRpcClientCreate = unsafe extern "C" fn(*const c_char, *mut RawNvRpcHandle) -> c_int;
type NvRpcClientDestroy = unsafe extern "C" fn(RawNvRpcHandle) -> c_int;
type NvRpcClientConnect = unsafe extern "C" fn(RawNvRpcHandle) -> c_int;
type NvRpcClientDisconnect = unsafe extern "C" fn(RawNvRpcHandle) -> c_int;
type NvRpcClientSetClientId =
    unsafe extern "C" fn(RawNvRpcHandle, *const c_char, usize, *mut u8, usize, *mut usize) -> c_int;
type NvRpcClientStartCxrService =
    unsafe extern "C" fn(RawNvRpcHandle, *const c_char, usize) -> c_int;
type NvRpcClientStopCxrService = unsafe extern "C" fn(RawNvRpcHandle) -> c_int;
type NvRpcClientGetCryptoKeyFingerprint =
    unsafe extern "C" fn(RawNvRpcHandle, c_int, *mut c_char, usize) -> c_int;
type NvRpcClientGetErrorString = unsafe extern "C" fn(c_int) -> *const c_char;
type NvRpcClientGetCxrServiceStatus =
    unsafe extern "C" fn(RawNvRpcHandle, *mut NvServiceStatus) -> c_int;

#[repr(C)]
struct NvServiceStatus {
    openxr_runtime_running: bool,
    openxr_app_connected: bool,
    cloudxr_client_connected: bool,
    openxr_log_file_path: [c_char; 256],
    openxr_log_file_path_length: usize,
    reserved: [i32; 4096],
}

struct NvRpcLibrary {
    library: Library,
}

#[derive(Clone, Copy)]
struct NvRpcHandle(usize);

impl NvRpcHandle {
    fn from_raw(handle: RawNvRpcHandle) -> Self {
        Self(handle as usize)
    }

    fn as_raw(self) -> RawNvRpcHandle {
        self.0 as RawNvRpcHandle
    }
}

struct PersistentRpcClient {
    library: NvRpcLibrary,
    handle: NvRpcHandle,
    connected: bool,
    service_running: bool,
}

impl NvRpcLibrary {
    fn load(path: &Path) -> Result<Self> {
        let library = unsafe { Library::new(path) }
            .with_context(|| format!("failed to load {}", path.display()))?;
        Ok(Self { library })
    }
}

impl PersistentRpcClient {
    fn new(path: &Path) -> Result<Self> {
        let library = NvRpcLibrary::load(path)?;
        let create: Symbol<'_, NvRpcClientCreate> =
            unsafe { library.symbol(b"nv_rpc_client_create\0")? };

        let mut raw_handle: RawNvRpcHandle = ptr::null_mut();
        let result = unsafe { create(ptr::null(), &mut raw_handle as *mut _) };
        library.check_result(result, "nv_rpc_client_create")?;

        Ok(Self {
            library,
            handle: NvRpcHandle::from_raw(raw_handle),
            connected: false,
            service_running: false,
        })
    }

    fn generate_barcode(&mut self, client_id: &str) -> Result<BarcodePayload> {
        self.ensure_connected()?;

        let client_id = CString::new(client_id)?;
        let set_client_id: Symbol<'_, NvRpcClientSetClientId> =
            unsafe { self.library.symbol(b"nv_rpc_client_set_client_id\0")? };
        let get_fingerprint: Symbol<'_, NvRpcClientGetCryptoKeyFingerprint> = unsafe {
            self.library
                .symbol(b"nv_rpc_client_get_crypto_key_fingerprint\0")?
        };

        let mut token_buffer = [0_u8; TOKEN_BUFFER_LEN];
        let mut token_size = 0_usize;
        let result = unsafe {
            set_client_id(
                self.handle.as_raw(),
                client_id.as_ptr(),
                client_id.as_bytes().len(),
                token_buffer.as_mut_ptr(),
                token_buffer.len(),
                &mut token_size as *mut _,
            )
        };
        self.library
            .check_result(result, "nv_rpc_client_set_client_id")?;

        let client_token = decode_u8_buffer(&token_buffer, token_size);

        let mut fingerprint_buffer = [0_i8; FINGERPRINT_BUFFER_LEN];
        let result = unsafe {
            get_fingerprint(
                self.handle.as_raw(),
                2,
                fingerprint_buffer.as_mut_ptr(),
                fingerprint_buffer.len(),
            )
        };
        self.library
            .check_result(result, "nv_rpc_client_get_crypto_key_fingerprint")?;

        let certificate_fingerprint = unsafe {
            CStr::from_ptr(fingerprint_buffer.as_ptr())
                .to_string_lossy()
                .into_owned()
        };

        Ok(BarcodePayload {
            client_token,
            certificate_fingerprint,
        })
    }

    fn start_service(&mut self, version: &str) -> Result<()> {
        self.ensure_connected()?;

        let version = CString::new(version)?;
        let start: Symbol<'_, NvRpcClientStartCxrService> =
            unsafe { self.library.symbol(b"nv_rpc_client_start_cxr_service\0")? };
        let result = unsafe {
            start(
                self.handle.as_raw(),
                version.as_ptr(),
                version.as_bytes().len(),
            )
        };
        self.library
            .check_result(result, "nv_rpc_client_start_cxr_service")?;
        self.service_running = true;
        Ok(())
    }

    fn stop_service(&mut self) -> Result<()> {
        if !self.connected {
            self.service_running = false;
            return Ok(());
        }

        let stop: Symbol<'_, NvRpcClientStopCxrService> =
            unsafe { self.library.symbol(b"nv_rpc_client_stop_cxr_service\0")? };
        let result = unsafe { stop(self.handle.as_raw()) };
        self.service_running = false;
        self.library
            .check_result(result, "nv_rpc_client_stop_cxr_service")
    }

    fn query_status(&mut self) -> Result<CloudXrState> {
        self.ensure_connected()?;

        let query: Symbol<'_, NvRpcClientGetCxrServiceStatus> = unsafe {
            self.library
                .symbol(b"nv_rpc_client_get_cxr_service_status\0")?
        };
        let mut status = MaybeUninit::<NvServiceStatus>::zeroed();
        let result = unsafe { query(self.handle.as_raw(), status.as_mut_ptr()) };
        self.library
            .check_result(result, "nv_rpc_client_get_cxr_service_status")?;
        let status = unsafe { status.assume_init() };

        Ok(CloudXrState {
            cloudxr_client_connected: status.cloudxr_client_connected,
            openxr_runtime_running: status.openxr_runtime_running,
            game_is_connected: status.openxr_app_connected,
        })
    }

    fn ensure_connected(&mut self) -> Result<()> {
        if self.connected {
            return Ok(());
        }

        let connect: Symbol<'_, NvRpcClientConnect> =
            unsafe { self.library.symbol(b"nv_rpc_client_connect\0")? };
        let result = unsafe { connect(self.handle.as_raw()) };
        self.library.check_result(result, "nv_rpc_client_connect")?;
        self.connected = true;
        Ok(())
    }
}

impl Drop for PersistentRpcClient {
    fn drop(&mut self) {
        if self.service_running {
            let stop: Result<Symbol<'_, NvRpcClientStopCxrService>> =
                unsafe { self.library.symbol(b"nv_rpc_client_stop_cxr_service\0") };
            if let Ok(stop) = stop {
                let _ = unsafe { stop(self.handle.as_raw()) };
            }
        }

        if self.connected {
            let disconnect: Result<Symbol<'_, NvRpcClientDisconnect>> =
                unsafe { self.library.symbol(b"nv_rpc_client_disconnect\0") };
            if let Ok(disconnect) = disconnect {
                let _ = unsafe { disconnect(self.handle.as_raw()) };
            }
        }

        let destroy: Result<Symbol<'_, NvRpcClientDestroy>> =
            unsafe { self.library.symbol(b"nv_rpc_client_destroy\0") };
        if let Ok(destroy) = destroy {
            let _ = unsafe { destroy(self.handle.as_raw()) };
        }
    }
}

impl NvRpcLibrary {
    unsafe fn symbol<T>(&self, name: &[u8]) -> Result<Symbol<'_, T>> {
        self.library
            .get(name)
            .map_err(|error| anyhow!("failed to load symbol {}: {error}", symbol_name(name)))
    }

    fn check_result(&self, result: c_int, operation: &str) -> Result<()> {
        if result == 0 {
            return Ok(());
        }

        bail!("{operation} failed: {}", self.error_string(result));
    }

    fn error_string(&self, result: c_int) -> String {
        let symbol = unsafe {
            self.symbol::<NvRpcClientGetErrorString>(b"nv_rpc_client_get_error_string\0")
        };
        match symbol {
            Ok(get_error_string) => {
                let ptr = unsafe { get_error_string(result) };
                if ptr.is_null() {
                    format!("CloudXR RPC error {result}")
                } else {
                    unsafe { CStr::from_ptr(ptr) }
                        .to_string_lossy()
                        .into_owned()
                }
            }
            Err(_) => format!("CloudXR RPC error {result}"),
        }
    }
}

impl CloudXrService {
    pub async fn start(app_state: AppState) -> Result<Self> {
        let paths = locate_paths()?;
        std::env::set_var("XR_RUNTIME_JSON", &paths.runtime_json);

        let job_object = ProcessJobObject::new()
            .context("failed to create the Windows job object for NvStreamManager")?;
        let child = spawn_manager_process(&paths, &job_object)?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let service = Self {
            inner: Arc::new(CloudXrInner {
                paths: paths.clone(),
                child: Mutex::new(Some(child)),
                control: Mutex::new(CloudXrControlState::default()),
                job_object,
                rpc_client: Mutex::new(Some(PersistentRpcClient::new(&paths.rpc_dll)?)),
                shutdown_tx,
                poll_task: Mutex::new(None),
            }),
        };

        let poll_service = service.clone();
        let poll_state = app_state.clone();
        let poll_handle = tokio::spawn(async move {
            poll_service.poll_loop(poll_state, shutdown_rx).await;
        });
        *service.inner.poll_task.lock().await = Some(poll_handle);

        let _ = app_state
            .update(|snapshot| {
                snapshot.cloudxr.health = ConnectionHealth::Paused;
                snapshot.cloudxr.detail = format!(
                    "NvStreamManager launched. Runtime {} is configured.",
                    paths.runtime_version
                );
            })
            .await;

        Ok(service)
    }

    pub async fn stop(&self) {
        info!("Stopping NvStreamManager.exe");
        let _ = self.inner.shutdown_tx.send(true);

        if let Some(handle) = self.inner.poll_task.lock().await.take() {
            let _ = handle.await;
        }

        {
            let mut control = self.inner.control.lock().await;
            control.presentation_requested = false;
            control.session_paused = false;
            control.previous_client_connected = false;
            control.consecutive_rpc_failures = 0;
        }

        if let Err(error) = self.stop_manager().await {
            warn!("Failed to stop NvStreamManager cleanly: {error}");
        }

        self.inner.rpc_client.lock().await.take();
    }

    pub async fn start_presentation(&self) -> Result<()> {
        {
            let mut control = self.inner.control.lock().await;
            control.presentation_requested = true;
            control.session_paused = false;
        }

        let start_result = self
            .retry_rpc_operation("start the CloudXR service", || {
                self.with_rpc_client(|rpc| rpc.start_service(&self.inner.paths.runtime_version))
            })
            .await;

        if let Err(error) = start_result {
            self.inner.control.lock().await.presentation_requested = false;
            return Err(error);
        }

        self.wait_for_runtime_state(true).await
    }

    pub async fn stop_presentation(&self) -> Result<()> {
        {
            let mut control = self.inner.control.lock().await;
            control.presentation_requested = false;
            control.session_paused = false;
            control.previous_client_connected = false;
            control.consecutive_rpc_failures = 0;
        }

        self.retry_rpc_operation("stop the CloudXR service", || {
            self.with_rpc_client(|rpc| rpc.stop_service())
        })
        .await?;

        self.wait_for_runtime_state(false).await
    }

    pub async fn generate_barcode(&self, client_id: &str) -> Result<BarcodePayload> {
        self.retry_rpc_operation("generate a CloudXR pairing token", || {
            self.with_rpc_client(|rpc| rpc.generate_barcode(client_id))
        })
        .await
    }

    pub async fn query_status_once(&self) -> Result<CloudXrState> {
        self.with_rpc_client(|rpc| rpc.query_status())
    }

    pub async fn set_session_paused(&self, paused: bool) {
        self.inner.control.lock().await.session_paused = paused;
    }

    async fn wait_for_runtime_state(&self, expected: bool) -> Result<()> {
        timeout(STATE_CHANGE_TIMEOUT, async {
            loop {
                match self.query_status_once().await {
                    Ok(status) if status.openxr_runtime_running == expected => break,
                    _ => sleep(STATE_CHANGE_POLL_DELAY).await,
                }
            }
        })
        .await
        .map_err(|_| anyhow!("timed out waiting for CloudXR runtime state {expected}"))?;

        Ok(())
    }

    fn with_rpc_client<T>(
        &self,
        operation: impl FnOnce(&mut PersistentRpcClient) -> Result<T>,
    ) -> Result<T> {
        tokio::task::block_in_place(|| {
            let mut rpc_client = self.inner.rpc_client.blocking_lock();
            let rpc_client = rpc_client
                .as_mut()
                .context("the CloudXR RPC client is not available")?;
            operation(rpc_client)
        })
    }

    async fn retry_rpc_operation<T>(
        &self,
        description: &str,
        mut operation: impl FnMut() -> Result<T>,
    ) -> Result<T> {
        let deadline = Instant::now() + RPC_OPERATION_RETRY_TIMEOUT;

        loop {
            match operation() {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(error)
                            .with_context(|| format!("timed out while trying to {description}"));
                    }

                    sleep(STATUS_POLL_INTERVAL).await;
                }
            }
        }
    }

    async fn poll_loop(&self, app_state: AppState, mut shutdown_rx: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_ok() && *shutdown_rx.borrow() {
                        break;
                    }
                }
                _ = sleep(STATUS_POLL_INTERVAL) => {
                    if self.manager_exited().await {
                        if let Err(error) = self
                            .restart_manager(
                                &app_state,
                                "NvStreamManager exited unexpectedly. Restarting CloudXR manager.",
                            )
                            .await
                        {
                            let _ = app_state.update(|snapshot| {
                                snapshot.cloudxr.health = ConnectionHealth::Stopped;
                                snapshot.cloudxr.detail = format!("CloudXR manager restart failed: {error}");
                            }).await;
                            break;
                        }

                        continue;
                    }

                    let presentation_requested = {
                        self.inner.control.lock().await.presentation_requested
                    };

                    if !presentation_requested {
                        let _ = app_state.update(|snapshot| {
                            snapshot.cloudxr.health = ConnectionHealth::Paused;
                            snapshot.cloudxr.detail =
                                "NvStreamManager is ready. Waiting for the session to request CloudXR presentation."
                                    .to_string();
                        }).await;
                        continue;
                    }

                    match self.query_status_once().await {
                        Ok(status) => {
                            let mut should_bounce = false;
                            {
                                let mut control = self.inner.control.lock().await;
                                control.consecutive_rpc_failures = 0;
                                if control.previous_client_connected
                                    && !status.cloudxr_client_connected
                                    && control.presentation_requested
                                    && !control.session_paused
                                {
                                    should_bounce = true;
                                }
                                control.previous_client_connected = status.cloudxr_client_connected;
                            }

                            if should_bounce {
                                if let Err(error) = self
                                    .restart_manager(
                                        &app_state,
                                        "CloudXR client disconnected unexpectedly. Restarting NvStreamManager.",
                                    )
                                    .await
                                {
                                    let _ = app_state.update(|snapshot| {
                                        snapshot.cloudxr.health = ConnectionHealth::Stopped;
                                        snapshot.cloudxr.detail = format!("CloudXR watchdog restart failed: {error}");
                                    }).await;
                                    break;
                                }

                                continue;
                            }

                            let detail = render_state_detail(status);
                            let health = render_state_health(status);
                            let _ = app_state.update(|snapshot| {
                                snapshot.cloudxr.health = health;
                                snapshot.cloudxr.detail = detail.clone();
                            }).await;
                        }
                        Err(error) => {
                            let mut should_bounce = false;
                            let presentation_requested = {
                                let mut control = self.inner.control.lock().await;
                                if control.presentation_requested {
                                    control.consecutive_rpc_failures += 1;
                                    should_bounce =
                                        control.consecutive_rpc_failures >= RPC_FAILURE_WATCHDOG_THRESHOLD;
                                    if should_bounce {
                                        control.consecutive_rpc_failures = 0;
                                        control.previous_client_connected = false;
                                    }
                                } else {
                                    control.consecutive_rpc_failures = 0;
                                }

                                control.presentation_requested
                            };

                            if should_bounce {
                                if let Err(restart_error) = self
                                    .restart_manager(
                                        &app_state,
                                        "CloudXR RPC stopped responding. Restarting NvStreamManager.",
                                    )
                                    .await
                                {
                                    let _ = app_state.update(|snapshot| {
                                        snapshot.cloudxr.health = ConnectionHealth::Stopped;
                                        snapshot.cloudxr.detail = format!("CloudXR watchdog restart failed: {restart_error}");
                                    }).await;
                                    break;
                                }

                                continue;
                            }

                            let detail = if presentation_requested {
                                format!("CloudXR is starting or reconnecting: {error}")
                            } else {
                                format!("NvStreamManager launched. RPC not ready yet: {error}")
                            };
                            let _ = app_state.update(|snapshot| {
                                snapshot.cloudxr.health = ConnectionHealth::Paused;
                                snapshot.cloudxr.detail = detail.clone();
                            }).await;
                        }
                    }
                }
            }
        }
    }

    async fn restart_manager(&self, app_state: &AppState, reason: &str) -> Result<()> {
        warn!("{reason}");
        let _ = app_state
            .update(|snapshot| {
                snapshot.cloudxr.health = ConnectionHealth::Paused;
                snapshot.cloudxr.detail = reason.to_string();
            })
            .await;

        self.stop_manager().await?;
        sleep(PROCESS_RESTART_DELAY).await;

        let child = spawn_manager_process(&self.inner.paths, &self.inner.job_object)?;
        *self.inner.child.lock().await = Some(child);
        *self.inner.rpc_client.lock().await =
            Some(PersistentRpcClient::new(&self.inner.paths.rpc_dll)?);

        let should_restart_presentation = {
            let mut control = self.inner.control.lock().await;
            control.consecutive_rpc_failures = 0;
            control.previous_client_connected = false;
            control.presentation_requested
        };

        if should_restart_presentation {
            self.retry_rpc_operation(
                "restart the CloudXR service after a manager restart",
                || self.with_rpc_client(|rpc| rpc.start_service(&self.inner.paths.runtime_version)),
            )
            .await?;
        }

        Ok(())
    }

    async fn stop_manager(&self) -> Result<()> {
        let mut child = self.inner.child.lock().await.take();

        if let Some(child) = child.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => {}
                Err(error) => warn!("Failed to check NvStreamManager exit status: {error}"),
            }
        } else {
            return Ok(());
        }

        if let Err(error) = self.inner.job_object.terminate_all_processes() {
            warn!("TerminateJobObject failed, falling back to Child::kill(): {error}");
            if let Some(child) = child.as_mut() {
                let _ = child.kill().await;
            }
        }

        if let Some(child) = child.as_mut() {
            match timeout(PROCESS_WAIT_TIMEOUT, child.wait()).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => warn!("Failed waiting for NvStreamManager to exit: {error}"),
                Err(_) => {
                    warn!("Timed out waiting for NvStreamManager to exit; forcing kill.");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
        }

        Ok(())
    }

    async fn manager_exited(&self) -> bool {
        let mut child = self.inner.child.lock().await;
        if let Some(child) = child.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(_) => true,
            }
        } else {
            true
        }
    }
}

fn spawn_manager_process(paths: &CloudXrPaths, job_object: &ProcessJobObject) -> Result<Child> {
    kill_existing_manager_processes(paths)?;

    info!(
        "Launching NvStreamManager.exe from {}",
        paths.manager_exe.display()
    );

    let mut command = Command::new(&paths.manager_exe);
    command
        .current_dir(&paths.server_dir)
        .env("XR_RUNTIME_JSON", &paths.runtime_json)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to launch {}", paths.manager_exe.display()))?;

    let pid = child
        .id()
        .context("failed to read the NvStreamManager process id after launch")?;
    info!("NvStreamManager.exe launched with pid {pid}");
    job_object.assign_pid(pid)?;

    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(log_output(stdout, format!("CloudXR stdout pid={pid}")));
    }

    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(log_output(stderr, format!("CloudXR stderr pid={pid}")));
    }

    Ok(child)
}

fn render_state_detail(state: CloudXrState) -> String {
    if state.game_is_connected && state.cloudxr_client_connected && state.openxr_runtime_running {
        "OpenXR game is connected and streaming to Vision Pro.".to_string()
    } else {
        format!(
            "OpenXR Game: {}; OpenXR Runtime: {}; CloudXR Client: {}",
            if state.game_is_connected {
                "Connected"
            } else {
                "Disconnected"
            },
            if state.openxr_runtime_running {
                "Running"
            } else {
                "Stopped"
            },
            if state.cloudxr_client_connected {
                "Connected"
            } else {
                "Disconnected"
            },
        )
    }
}

fn render_state_health(state: CloudXrState) -> ConnectionHealth {
    if state.game_is_connected && state.cloudxr_client_connected && state.openxr_runtime_running {
        ConnectionHealth::Running
    } else if state.game_is_connected
        || state.cloudxr_client_connected
        || state.openxr_runtime_running
    {
        ConnectionHealth::Paused
    } else {
        ConnectionHealth::Stopped
    }
}

fn kill_existing_manager_processes(paths: &CloudXrPaths) -> Result<()> {
    let expected_path = normalize_path(&paths.manager_exe)?;
    let process_name = paths
        .manager_exe
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("NvStreamManager");

    let mut system = System::new_all();
    system.refresh_processes(ProcessesToUpdate::All, true);

    for process in system.processes_by_name(process_name.as_ref()) {
        let Some(exe) = process.exe() else {
            continue;
        };

        let Ok(candidate_path) = normalize_path(exe) else {
            continue;
        };

        if candidate_path != expected_path {
            continue;
        }

        warn!(
            "Found an existing NvStreamManager.exe at {} (pid {}). Killing it before launch.",
            exe.display(),
            process.pid()
        );

        if process.kill_with(Signal::Kill).is_none() && !process.kill() {
            bail!(
                "failed to terminate the existing NvStreamManager process with pid {}",
                process.pid()
            );
        }
    }

    Ok(())
}

async fn log_output<T>(stream: T, label: String)
where
    T: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if !line.starts_with("RPC GetCxrServiceStatus received")
            && !line.starts_with("Returning status - ")
        {
            info!("[{label}] {line}");
        }
    }
}

fn normalize_path(path: &Path) -> Result<String> {
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize {}", path.display()))?;
    Ok(canonical.to_string_lossy().to_ascii_lowercase())
}

fn locate_paths() -> Result<CloudXrPaths> {
    let mut failures = Vec::new();

    for root in runtime_root_candidates()? {
        match locate_paths_at_root(&root) {
            Ok(paths) => {
                info!("Using vendored CloudXR runtime from {}", root.display());
                return Ok(paths);
            }
            Err(error) => failures.push(format!("{}: {error}", root.display())),
        }
    }

    bail!(
        "could not locate the vendored CloudXR runtime. Checked {}",
        failures.join(" | ")
    )
}

fn runtime_root_candidates() -> Result<Vec<PathBuf>> {
    let exe_path =
        std::env::current_exe().context("failed to determine the current executable path")?;
    let exe_dir = exe_path
        .parent()
        .context("the current executable path does not have a parent directory")?
        .to_path_buf();
    let manifest_vendor_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vendor");

    let mut roots = Vec::new();
    push_unique_path(&mut roots, exe_dir.clone());
    push_unique_path(&mut roots, exe_dir.join("resources"));
    push_unique_path(&mut roots, exe_dir.join("resources").join("vendor"));
    push_unique_path(&mut roots, manifest_vendor_dir);

    Ok(roots)
}

fn push_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|existing| existing == &candidate) {
        paths.push(candidate);
    }
}

fn locate_paths_at_root(root: &Path) -> Result<CloudXrPaths> {
    let server_dir = root.join("Server");
    let manager_exe = server_dir.join("NvStreamManager.exe");
    let rpc_dll = root.join("NvStreamManagerClient.dll");

    if !server_dir.exists() {
        bail!("missing {}", server_dir.display());
    }

    if !manager_exe.exists() {
        bail!("missing {}", manager_exe.display());
    }

    if !rpc_dll.exists() {
        bail!("missing {}", rpc_dll.display());
    }

    let runtime_candidates =
        find_files_recursive(&server_dir.join("releases"), "openxr_cloudxr.json")?;
    let runtime_json = runtime_candidates
        .first()
        .cloned()
        .context("could not find openxr_cloudxr.json under Server\\releases")?;

    if runtime_candidates.len() > 1 {
        warn!(
            "Found {} CloudXR runtime manifests. Using {}.",
            runtime_candidates.len(),
            runtime_json.display()
        );
    }

    let runtime_version = runtime_json
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .context("could not determine the CloudXR runtime version from openxr_cloudxr.json")?;

    Ok(CloudXrPaths {
        server_dir,
        manager_exe,
        rpc_dll,
        runtime_json,
        runtime_version,
    })
}

fn find_files_recursive(root: &Path, filename: &str) -> Result<Vec<PathBuf>> {
    let mut results = Vec::new();

    if !root.exists() {
        return Ok(results);
    }

    let entries =
        std::fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            results.extend(find_files_recursive(&path, filename)?);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(filename))
        {
            results.push(path);
        }
    }

    results.sort();
    Ok(results)
}

fn decode_u8_buffer(buffer: &[u8], explicit_len: usize) -> String {
    let end = explicit_len.min(buffer.len());
    let trimmed = &buffer[..end];
    let trimmed = match trimmed.iter().position(|byte| *byte == 0) {
        Some(index) => &trimmed[..index],
        None => trimmed,
    };
    String::from_utf8_lossy(trimmed).into_owned()
}

fn symbol_name(name: &[u8]) -> String {
    let nul = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    String::from_utf8_lossy(&name[..nul]).into_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{decode_u8_buffer, find_files_recursive, render_state_health, CloudXrState};
    use crate::models::ConnectionHealth;

    #[test]
    fn decode_u8_buffer_trims_at_the_first_nul() {
        let value = decode_u8_buffer(b"token\0ignored", 12);
        assert_eq!(value, "token");
    }

    #[test]
    fn render_state_health_matches_expected_transitions() {
        assert_eq!(
            render_state_health(CloudXrState::default()),
            ConnectionHealth::Stopped
        );
        assert_eq!(
            render_state_health(CloudXrState {
                openxr_runtime_running: true,
                ..CloudXrState::default()
            }),
            ConnectionHealth::Paused
        );
        assert_eq!(
            render_state_health(CloudXrState {
                openxr_runtime_running: true,
                game_is_connected: true,
                cloudxr_client_connected: true,
            }),
            ConnectionHealth::Running
        );
    }

    #[test]
    fn find_files_recursive_returns_sorted_matches() {
        let root = std::env::temp_dir().join(format!(
            "streaming-session-cloudxr-test-{}",
            std::process::id()
        ));
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("z.json"), "{}").unwrap();
        fs::write(nested.join("openxr_cloudxr.json"), "{}").unwrap();
        fs::write(root.join("a").join("openxr_cloudxr.json"), "{}").unwrap();

        let mut matches = find_files_recursive(Path::new(&root), "openxr_cloudxr.json").unwrap();
        matches.sort();

        assert_eq!(matches.len(), 2);
        assert!(matches[0] <= matches[1]);

        fs::remove_dir_all(root).unwrap();
    }
}

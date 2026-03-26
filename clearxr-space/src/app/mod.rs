pub mod game_scanner;

use std::process::{Child, Command};
use log::info;

/// Information about a launched application.
#[allow(dead_code)] // Fields are part of the public API for future use
pub struct LaunchedApp {
    pub child: Child,
    pub name: String,
    pub app_id: Option<u32>,
    pub is_xr: bool,
}

/// Result of checking a launched app.
#[derive(Debug, PartialEq)]
pub enum AppStatus {
    Running,
    Exited(i32),     // exit code
    ExitedOk,        // exit code 0
}

impl LaunchedApp {
    /// Check if the app is still running.
    pub fn status(&mut self) -> AppStatus {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    AppStatus::ExitedOk
                } else {
                    AppStatus::Exited(status.code().unwrap_or(-1))
                }
            }
            Ok(None) => AppStatus::Running,
            Err(_) => AppStatus::Exited(-1),
        }
    }

    /// Kill the app.
    #[allow(dead_code)] // Used in tests and available as public API
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

/// Launch a flat (non-VR) game. Returns the child process handle.
#[allow(dead_code)] // Used in tests; will be called when flat-game launch is wired up
pub fn launch_flat_game(name: &str, exe_path: &str, app_id: Option<u32>) -> Result<LaunchedApp, String> {
    info!("Launching flat game: {} ({})", name, exe_path);

    let child = Command::new(exe_path)
        .spawn()
        .map_err(|e| format!("Failed to launch '{}': {}", name, e))?;

    Ok(LaunchedApp {
        child,
        name: name.to_string(),
        app_id,
        is_xr: false,
    })
}

/// Launch a Steam game via steam://run/<app_id>.
pub fn launch_steam_game(name: &str, app_id: u32) -> Result<LaunchedApp, String> {
    info!("Launching Steam game: {} (app_id: {})", name, app_id);

    let url = format!("steam://rungameid/{}", app_id);

    #[cfg(target_os = "windows")]
    let child = Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn()
        .map_err(|e| format!("Failed to launch Steam game '{}': {}", name, e))?;

    #[cfg(not(target_os = "windows"))]
    let child = Command::new("open")
        .arg(&url)
        .spawn()
        .map_err(|e| format!("Failed to launch Steam game '{}': {}", name, e))?;

    Ok(LaunchedApp {
        child,
        name: name.to_string(),
        app_id: Some(app_id),
        is_xr: false, // We'll detect VR games later
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_status_variants() {
        assert_eq!(AppStatus::Running, AppStatus::Running);
        assert_eq!(AppStatus::ExitedOk, AppStatus::ExitedOk);
        assert_ne!(AppStatus::Running, AppStatus::ExitedOk);
    }

    #[test]
    fn launch_flat_game_invalid_path() {
        let result = launch_flat_game("test", "/nonexistent/path/game.exe", None);
        assert!(result.is_err());
    }

    #[test]
    fn launch_flat_game_valid() {
        // Launch a quick-exit process to test the happy path
        #[cfg(target_os = "windows")]
        let result = launch_flat_game("test", "cmd", None);
        #[cfg(not(target_os = "windows"))]
        let result = launch_flat_game("test", "true", None);

        if let Ok(mut app) = result {
            assert_eq!(app.name, "test");
            assert!(!app.is_xr);
            // Kill it immediately
            app.kill();
        }
    }
}

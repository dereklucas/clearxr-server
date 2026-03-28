/// Detects installed VR-capable games from Steam (and later, other stores).

use log::info;

#[derive(Debug, Clone)]
pub struct Game {
    pub app_id: u32,
    pub name: String,
    pub install_dir: String,
    pub source: GameSource,
    /// Path to header art (e.g. Steam library cache `<appid>_header.jpg`).
    pub art_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub enum GameSource {
    Steam,
}

/// Scan all known game sources and return a unified, sorted list.
pub fn scan_all() -> Vec<Game> {
    let mut games = Vec::new();
    games.extend(scan_steam());
    games.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    info!(
        "[ClearXR Layer] Game scanner found {} installed games",
        games.len()
    );
    games
}

fn scan_steam() -> Vec<Game> {
    let mut games = Vec::new();

    let locator = match steamlocate::SteamDir::locate() {
        Ok(l) => l,
        Err(e) => {
            info!("[ClearXR Layer] Steam not found: {}", e);
            return games;
        }
    };

    let cache_dir = locator.path().join("appcache").join("librarycache");

    let libraries = match locator.libraries() {
        Ok(iter) => iter,
        Err(e) => {
            info!("[ClearXR Layer] Could not read Steam libraries: {}", e);
            return games;
        }
    };

    for library in libraries {
        let library = match library {
            Ok(l) => l,
            Err(_) => continue,
        };
        for result in library.apps() {
            let app = match result {
                Ok(a) => a,
                Err(_) => continue,
            };
            let name = app
                .name
                .clone()
                .unwrap_or_else(|| format!("App {}", app.app_id));
            let app_cache = cache_dir.join(format!("{}", app.app_id));
            let art_path = ["header.jpg", "library_header.jpg"]
                .iter()
                .map(|name| app_cache.join(name))
                .find(|p| p.exists());
            games.push(Game {
                app_id: app.app_id,
                name,
                install_dir: app.install_dir.clone(),
                source: GameSource::Steam,
                art_path,
            });
        }
    }

    info!(
        "[ClearXR Layer] Steam: found {} installed apps",
        games.len()
    );
    games
}

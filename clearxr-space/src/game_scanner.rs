/// Detects installed VR-capable games from Steam (and later, other stores).

use log::info;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Game {
    pub app_id: u32,
    pub name: String,
    pub install_dir: String,
    pub source: GameSource,
}

#[derive(Debug, Clone, Serialize)]
pub enum GameSource {
    Steam,
}

/// Scan all known game sources and return a unified list.
pub fn scan_all() -> Vec<Game> {
    let mut games = Vec::new();
    games.extend(scan_steam());
    info!("Game scanner found {} installed games", games.len());
    games
}

fn scan_steam() -> Vec<Game> {
    let mut games = Vec::new();

    let locator = match steamlocate::SteamDir::locate() {
        Ok(l) => l,
        Err(e) => {
            info!("Steam not found: {}", e);
            return games;
        }
    };

    let libraries = match locator.libraries() {
        Ok(iter) => iter,
        Err(e) => {
            info!("Could not read Steam libraries: {}", e);
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
            let name = app.name.clone().unwrap_or_else(|| format!("App {}", app.app_id));
            games.push(Game {
                app_id: app.app_id,
                name,
                install_dir: app.install_dir.clone(),
                source: GameSource::Steam,
            });
        }
    }

    info!("Steam: found {} installed apps", games.len());
    games
}

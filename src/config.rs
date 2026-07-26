use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub download_dir: String,
    pub endpoint: String,
    pub download_dir_set: bool,
    pub hf_logged_in: Option<bool>,
    pub language: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            endpoint: String::new(),
            download_dir_set: false,
            hf_logged_in: None,
            language: None,
        }
    }
}

pub fn default_download_dir() -> String {
    // Default to the directory the executable lives in, so downloads stay next to the
    // app (single-folder, portable behavior). Fall back to the OS Downloads / home dir
    // only if the executable path can't be resolved.
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    {
        if let Some(s) = exe_dir.to_str() {
            return s.to_string();
        }
    }
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string())
}

fn config_path() -> PathBuf {
    // Save the config next to the running executable (program's run directory),
    // so it travels with the app. Fall back to the OS config dir if that can't be resolved.
    if let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    {
        return exe.join("config.json");
    }
    let base = dirs::config_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("hf-downloader");
    let _ = fs::create_dir_all(&dir);
    dir.join("config.json")
}

pub struct ConfigState {
    pub config: Mutex<AppConfig>,
}

impl ConfigState {
    pub fn load() -> Self {
        let path = config_path();
        let config = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(data) => {
                    let mut cfg: AppConfig =
                        serde_json::from_str(&data).unwrap_or_default();
                    if !cfg.download_dir_set {
                        cfg.download_dir = default_download_dir();
                    }
                    cfg
                }
                Err(_) => AppConfig::default(),
            }
        } else {
            AppConfig::default()
        };
        Self {
            config: Mutex::new(config),
        }
    }
}

/// Persist a config struct directly (egui holds AppConfig by value).
pub fn save_config(config: &AppConfig) {
    let path = config_path();
    if let Ok(data) = serde_json::to_string_pretty(config) {
        let _ = fs::write(&path, data);
    }
}

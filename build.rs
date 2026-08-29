//! Build script: copy bundled asset directories (`lang/`, `fonts/`) next to the
//! final executable so the app can load them at runtime.
//!
//! OUT_DIR is `<manifest>/target/<profile>/build/<pkg>-<hash>/out`, so climbing 3
//! ancestors lands on `<manifest>/target/<profile>` — i.e. the directory that holds
//! the compiled exe.

use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let out_dir = match std::env::var("OUT_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => return,
    };
    let exe_dir = out_dir
        .ancestors()
        .nth(3)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| out_dir.clone());

    // Copy every `*.json` from src/lang into <exe_dir>/lang.
    copy_assets(&manifest_dir.join("lang"), &exe_dir.join("lang"), &["json"]);
    // Copy the whole src/fonts directory (fonts + license) into <exe_dir>/fonts.
    copy_dir_all(&manifest_dir.join("fonts"), &exe_dir.join("fonts"));

    println!("cargo:rerun-if-changed=lang");
    println!("cargo:rerun-if-changed=fonts");

    // Windows builds embed assets/icon.ico so Explorer / taskbar show the app icon.
    // The runtime window icon is set separately in main.rs via egui::IconData.
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("failed to compile Windows resources");
        println!("cargo:rerun-if-changed=assets/icon.ico");
    }
}

/// Copy files with any of `exts` from `src` to `dest`. No-op if `src` is missing.
fn copy_assets(src: &PathBuf, dest: &PathBuf, exts: &[&str]) {
    if !src.exists() {
        return;
    }
    let _ = fs::create_dir_all(dest);
    if let Ok(entries) = fs::read_dir(src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if exts.contains(&ext) {
                    let _ = fs::copy(&path, dest.join(entry.file_name()));
                }
            }
        }
    }
}

/// Copy every regular file from `src` to `dest`. No-op if `src` is missing.
fn copy_dir_all(src: &PathBuf, dest: &PathBuf) {
    if !src.exists() {
        return;
    }
    let _ = fs::create_dir_all(dest);
    if let Ok(entries) = fs::read_dir(src) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let _ = fs::copy(&path, dest.join(entry.file_name()));
            }
        }
    }
}

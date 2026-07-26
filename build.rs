//! Build script: copy the bundled language files (`lang/*.json`) into the directory
//! next to the final executable so the app can load them at runtime.
//!
//! OUT_DIR is `<manifest>/target/<profile>/build/<pkg>-<hash>/out`, so climbing 3
//! ancestors lands on `<manifest>/target/<profile>` — i.e. the directory that holds
//! the compiled exe.

use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_lang = manifest_dir.join("lang");
    if !src_lang.exists() {
        return;
    }

    let out_dir = match std::env::var("OUT_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => return,
    };
    let exe_dir = out_dir
        .ancestors()
        .nth(3)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| out_dir.clone());
    let dest = exe_dir.join("lang");
    let _ = fs::create_dir_all(&dest);

    if let Ok(entries) = fs::read_dir(&src_lang) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                let _ = fs::copy(&path, dest.join(entry.file_name()));
            }
        }
    }

    // Re-run this script if anything under lang/ changes.
    println!("cargo:rerun-if-changed=lang");
}

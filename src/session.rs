use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// One selected file recorded for a download session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFileEntry {
    pub path: String,
    pub size: u64,
}

/// A persisted snapshot of an in-progress download, so it can be resumed after the
/// app (or the whole machine) is restarted.
///
/// Each running instance writes to its OWN file keyed by process id, so two
/// `hf-downloader-egui.exe` instances downloading different repos at the same time
/// never overwrite each other's record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub pid: u32,
    pub repo_id: String,
    pub repo_type: String,
    pub revision: String,
    pub subpath: String,
    pub endpoint: String,
    pub download_dir: String,
    pub target_dir: String,
    pub files: Vec<SessionFileEntry>,
    pub updated_at: u64,
}

fn sessions_dir() -> PathBuf {
    if let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    {
        return exe.join("sessions");
    }
    PathBuf::from("sessions")
}

/// Path of THIS process's session file (unique per PID).
fn current_session_path() -> PathBuf {
    sessions_dir().join(format!("session-{}.json", std::process::id()))
}

/// Path of a given PID's session file (used to delete a recovered session).
pub fn path_for_pid(pid: u32) -> PathBuf {
    sessions_dir().join(format!("session-{}.json", pid))
}

pub fn write_session(session: &Session) {
    let dir = sessions_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(data) = serde_json::to_string_pretty(session) {
        let _ = fs::write(current_session_path(), data);
    }
}

/// Remove THIS process's session file (called when a download fully completes, so a
/// finished repo is no longer offered for recovery).
pub fn delete_current_session() {
    let _ = fs::remove_file(current_session_path());
}

/// Remove a specific session file (called after we restore from it, so the old PID
/// file doesn't linger and get re-offered next time).
pub fn delete_session_file(path: &std::path::Path) {
    let _ = fs::remove_file(path);
}

pub fn load_session(path: &std::path::Path) -> Option<Session> {
    fs::read_to_string(path)
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
}

/// Scan for sessions left by previous runs, EXCLUDING any whose owning process is
/// still alive (so we don't offer to "resume" a download that's already running in
/// another instance).
pub fn list_recoverable_sessions() -> Vec<Session> {
    let dir = sessions_dir();
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if p
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.starts_with("session-"))
                != Some(true)
            {
                continue;
            }
            if let Some(s) = load_session(&p) {
                if process_alive(s.pid) {
                    continue;
                }
                out.push(s);
            }
        }
    }
    out
}

/// Best-effort check whether a process with `pid` is still running.
#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use std::process::Command;
    if pid == std::process::id() {
        return true;
    }
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid)])
        .output();
    match out {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            let pid_str = pid.to_string();
            // The data row prints the PID as its own whitespace-separated token; the
            // header line only contains the word "PID", so matching the number tells
            // us the process row is present (i.e. it's alive).
            s.lines()
                .any(|l| l.split_whitespace().any(|tok| tok == pid_str))
        }
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn process_alive(_pid: u32) -> bool {
    false
}

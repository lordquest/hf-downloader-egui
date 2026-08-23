use std::sync::mpsc::Sender;

use crate::download::{self, FileStatus, TaskManager};
use crate::hf_api::{FileEntry, RepoInfo, TokenStatus};

/// Messages sent from the download/worker threads back to the egui UI thread.
/// This replaces Tauri's `app_handle.emit(...)` channel.
pub enum UiMsg {
    /// A single file's state changed (progress / status).
    File(crate::download::FileState),
    /// The whole task finished.
    Done { task_id: String },
    /// Repo parsed + file list fetched.
    RepoListed {
        info: RepoInfo,
        entries: Vec<FileEntry>,
    },
    /// API / network error from a worker thread.
    ApiError(String),
    /// Token login/check result.
    TokenChecked(TokenStatus),
}

/// Owns the task registry and a channel back to the UI. The UI calls `start`/`cancel`/
/// `retry`; progress is delivered via the `Sender<UiMsg>` that the engine was built with.
pub struct DownloadEngine {
    manager: TaskManager,
    tx: Sender<UiMsg>,
}

impl DownloadEngine {
    pub fn new(tx: Sender<UiMsg>) -> Self {
        Self {
            manager: TaskManager::new(),
            tx,
        }
    }

    /// Clone the UI message sender so a worker thread can report back.
    pub fn sender(&self) -> Sender<UiMsg> {
        self.tx.clone()
    }

    /// Register the selected files and start downloading them.
    pub fn start(
        &self,
        task_id: &str,
        repo_id: &str,
        repo_type: &str,
        revision: &str,
        target_dir: &str,
        endpoint: &str,
        file_paths: Vec<(String, u64)>,
        lang: String,
    ) {
        // `get_or_create` only briefly locks the registry, so blocking the UI thread is fine.
        let task = download::download_runtime().block_on(self.manager.get_or_create(
            task_id,
            repo_id,
            repo_type,
            revision,
            target_dir,
            endpoint,
        ));
        let tx = self.tx.clone();
        let task_id = task_id.to_string();
        // Spawn the actual per-file downloads on the dedicated download runtime.
        download::download_runtime().spawn(async move {
            download::start_downloads(task_id, task, file_paths, tx, lang).await;
        });
    }

    /// Cancel the whole task (all in-flight files will stop at the next checkpoint).
    /// Kept as the "pause all" operation; the UI currently pauses per-file via
    /// `pause_file`, so this is unused for now.
    #[allow(dead_code)]
    pub fn cancel(&self, task_id: &str) {
        download::download_runtime().block_on(async {
            let tasks = self.manager.tasks.lock().await;
            if let Some(task) = tasks.get(task_id) {
                task.lock()
                    .await
                    .cancelled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
    }

    /// Pause a single file. The running download for that file stops at the next
    /// checkpoint (the partial file stays on disk so it can be resumed later), while
    /// every other file in the task keeps downloading. This is the per-file equivalent
    /// of `cancel`, which stops the entire task.
    pub fn pause_file(&self, task_id: &str, file_path: &str) {
        download::download_runtime().block_on(async {
            let tasks = self.manager.tasks.lock().await;
            if let Some(task) = tasks.get(task_id) {
                let mut t = task.lock().await;
                if let Some(f) = t.files.get_mut(file_path) {
                    f.status = FileStatus::Cancelled;
                }
            }
        });
    }

    /// Pause every file in the task that is still in-flight or queued. Each file stops
    /// independently at its next checkpoint; this is the bulk equivalent of `pause_file`.
    pub fn pause_all(&self, task_id: &str) {
        download::download_runtime().block_on(async {
            let tasks = self.manager.tasks.lock().await;
            if let Some(task) = tasks.get(task_id) {
                let mut t = task.lock().await;
                for f in t.files.values_mut() {
                    if f.status == FileStatus::Downloading || f.status == FileStatus::Pending {
                        f.status = FileStatus::Cancelled;
                    }
                }
            }
        });
    }

    /// Test-only accessor to the engine's task registry (so tests can pre-seed a task
    /// without going through `start`). Not used by the app.
    #[cfg(test)]
    pub(crate) fn _test_manager(&self) -> &TaskManager {
        &self.manager
    }

    /// Re-download a single failed/cancelled file.
    pub fn retry(
        &self,
        task_id: &str,
        file_path: &str,
        endpoint: &str,
        target_dir: &str,
        lang: String,
    ) {
        // Clone the borrowed inputs to owned values so they survive into the `'static`
        // async block spawned on the download runtime.
        let task_id = task_id.to_string();
        let fp = file_path.to_string();
        let ep = endpoint.to_string();
        let td = target_dir.to_string();
        download::download_runtime().block_on(async {
            let tasks = self.manager.tasks.lock().await;
            if let Some(task) = tasks.get(&task_id) {
                let size = {
                    let mut t = task.lock().await;
                    // Clear the task-level cancellation so this retry actually proceeds
                    // (the flag is only ever set on cancel and must be reset to continue).
                    t.cancelled.store(false, std::sync::atomic::Ordering::Relaxed);
                    // Apply the latest endpoint / download dir so a retry after switching
                    // to a mirror (e.g. hf-mirror.com) uses it, just like a fresh start.
                    t.endpoint = ep.clone();
                    t.target_dir = td.clone();
                    // NOTE: do NOT pre-set the file to `Pending` here. `start_downloads`
                    // resets `Failed`/`Cancelled` files itself and spawns them; if we set
                    // `Pending` first it would skip the file and the retry would do nothing.
                    t.files.get(&fp).map(|f| f.total).unwrap_or(0)
                };
                let tx = self.tx.clone();
                let task = task.clone();
                // Spawn just this one file again (start_downloads will reset its status).
                download::download_runtime().spawn(async move {
                    download::start_downloads(task_id, task, vec![(fp, size)], tx, lang)
                        .await;
                });
            }
        });
    }
}

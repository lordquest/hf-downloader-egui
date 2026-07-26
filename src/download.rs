use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::OnceLock;

use futures_util::StreamExt;
use tokio::fs::{self, File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Semaphore};

use crate::engine::UiMsg;
use crate::hf_api::read_token;
use crate::i18n;

/// A dedicated multi-threaded tokio runtime used to drive downloads, independent of
/// any other runtime. The standalone-equivalent of `#[tokio::main]`.
///
/// Why: the same streaming code hangs at ~80KB when driven by some host runtimes
/// (the socket read future is not polled promptly), but completes instantly on a plain
/// tokio runtime. Spawning downloads here reproduces the working standalone behavior.
pub fn download_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("failed to build download runtime")
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Pending,
    Downloading,
    Done,
    Exists,
    Failed,
    Cancelled,
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileStatus::Pending => write!(f, "pending"),
            FileStatus::Downloading => write!(f, "downloading"),
            FileStatus::Done => write!(f, "done"),
            FileStatus::Exists => write!(f, "exists"),
            FileStatus::Failed => write!(f, "failed"),
            FileStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    pub path: String,
    pub status: FileStatus,
    pub downloaded: u64,
    pub total: u64,
    pub speed: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct DownloadTask {
    pub repo_id: String,
    pub repo_type: String,
    pub revision: String,
    pub target_dir: String,
    pub endpoint: String,
    pub files: HashMap<String, FileState>,
    pub cancelled: Arc<AtomicBool>,
}

impl DownloadTask {
    pub fn new(
        repo_id: String,
        repo_type: String,
        revision: String,
        target_dir: String,
        endpoint: String,
    ) -> Self {
        Self {
            repo_id,
            repo_type,
            revision,
            target_dir,
            endpoint,
            files: HashMap::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_done(&self) -> bool {
        self.files
            .values()
            .all(|f| !matches!(f.status, FileStatus::Pending | FileStatus::Downloading))
    }
}

pub struct TaskManager {
    pub tasks: Mutex<HashMap<String, Arc<Mutex<DownloadTask>>>>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
        }
    }

    pub async fn get_or_create(
        &self,
        task_id: &str,
        repo_id: &str,
        repo_type: &str,
        revision: &str,
        target_dir: &str,
        endpoint: &str,
    ) -> Arc<Mutex<DownloadTask>> {
        let mut tasks = self.tasks.lock().await;
        if let Some(t) = tasks.get(task_id) {
            t.clone()
        } else {
            let task = Arc::new(Mutex::new(DownloadTask::new(
                repo_id.to_string(),
                repo_type.to_string(),
                revision.to_string(),
                target_dir.to_string(),
                endpoint.to_string(),
            )));
            tasks.insert(task_id.to_string(), task.clone());
            task
        }
    }
}

/// Start downloading files for a task. Spawns tokio tasks for each file.
pub async fn start_downloads(
    task_id: String,
    task: Arc<Mutex<DownloadTask>>,
    file_paths: Vec<(String, u64)>,
    tx: Sender<UiMsg>,
    lang: String,
) {
    let sem = Arc::new(Semaphore::new(3)); // max 3 concurrent downloads

    for (path, size) in file_paths {
        // Register file in task
        {
            let mut t = task.lock().await;
            if let Some(existing) = t.files.get(&path) {
                match existing.status {
                    FileStatus::Failed | FileStatus::Cancelled => {
                        t.files.insert(
                            path.clone(),
                            FileState {
                                path: path.clone(),
                                status: FileStatus::Pending,
                                downloaded: 0,
                                total: size,
                                speed: 0.0,
                                error: None,
                            },
                        );
                    }
                    FileStatus::Done | FileStatus::Exists | FileStatus::Downloading => {
                        continue;
                    }
                    FileStatus::Pending => {
                        continue;
                    }
                }
            } else {
                t.files.insert(
                    path.clone(),
                    FileState {
                        path: path.clone(),
                        status: FileStatus::Pending,
                        downloaded: 0,
                        total: size,
                        speed: 0.0,
                        error: None,
                    },
                );
            }
        }

        let sem = sem.clone();
        let task = task.clone();
        let tx = tx.clone();
        let tid = task_id.clone();
        let file_path = path.clone();

        let lang = lang.clone();
        download_runtime().spawn(async move {
            let _permit = sem.acquire().await;
            download_file(task, tx, &tid, &file_path, lang).await;
        });
    }
}

async fn download_file(
    task: Arc<Mutex<DownloadTask>>,
    tx: Sender<UiMsg>,
    task_id: &str,
    file_path: &str,
    lang: String,
) {
    // Get download params
    let (repo_id, repo_type, revision, target_dir, endpoint, cancelled) = {
        let t = task.lock().await;
        (
            t.repo_id.clone(),
            t.repo_type.clone(),
            t.revision.clone(),
            t.target_dir.clone(),
            t.endpoint.clone(),
            t.cancelled.clone(),
        )
    };

    // Set status to downloading
    {
        let mut t = task.lock().await;
        if let Some(f) = t.files.get_mut(file_path) {
            if f.status == FileStatus::Cancelled {
                return;
            }
            f.status = FileStatus::Downloading;
        }
    }
    send_file(&tx, &task, file_path).await;

    // Build local path
    let local_path = Path::new(&target_dir).join(file_path);
    if let Some(parent) = local_path.parent() {
        let _ = fs::create_dir_all(parent).await;
    }

    // Check resume: if file exists and is complete
    let total = {
        let t = task.lock().await;
        t.files.get(file_path).map(|f| f.total).unwrap_or(0)
    };

    let existing_size = if local_path.exists() {
        fs::metadata(&local_path).await.map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    if existing_size > 0 && total > 0 && existing_size >= total {
        {
            let mut t = task.lock().await;
            if let Some(f) = t.files.get_mut(file_path) {
                f.status = FileStatus::Exists;
                f.downloaded = existing_size;
                f.total = existing_size;
            }
        }
        send_file(&tx, &task, file_path).await;
        return;
    }

    // Build download URL
    let base = if endpoint.is_empty() {
        "https://huggingface.co".to_string()
    } else {
        endpoint.trim_end_matches('/').to_string()
    };
    let type_prefix = match repo_type.as_str() {
        "model" => "",
        "dataset" => "datasets/",
        "space" => "spaces/",
        _ => "",
    };
    let url = format!(
        "{}/{}{}/resolve/{}/{}",
        base, type_prefix, repo_id, revision, file_path
    );

    // Download with resume support.
    // Use HTTP/2 by default (reqwest needs the `http2` feature compiled in). huggingface_hub
    // / Python's httpx also use HTTP/2, and HF's resolve-cache proxy streams reliably over
    // HTTP/2. Forcing HTTP/1.1 here was the cause of the ~80KB mid-stream stall: the proxy
    // keeps the HTTP/1.1 connection open without ever signalling EOF, so the body read hangs.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let mut req = client
        .get(&url)
        .header("User-Agent", "hf-downloader-egui/0.1");

    // Attach token
    if let Some(token) = read_token() {
        req = req.bearer_auth(token);
    }

    // Resume from existing_size
    if existing_size > 0 {
        req = req.header("Range", format!("bytes={}-", existing_size));
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            {
                let mut t = task.lock().await;
                if let Some(f) = t.files.get_mut(file_path) {
                    f.status = FileStatus::Failed;
                    f.error = Some(format!("{}: {}", i18n::t("err_request", &lang), e));
                }
            }
            send_file(&tx, &task, file_path).await;
            return;
        }
    };

    let status = resp.status();
    if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        {
            let mut t = task.lock().await;
            if let Some(f) = t.files.get_mut(file_path) {
                f.status = FileStatus::Failed;
                f.error = Some(format!("HTTP {}", status));
            }
        }
        send_file(&tx, &task, file_path).await;
        return;
    }

    // Determine total size.
    // NOTE: hf-mirror's resolve-cache returns `Content-Length: None`. Fall back to the
    // size we already know from the file list, so the UI shows a real total and we can
    // detect completion even when the stream never sends EOF (proxy keeps connection open).
    let known_total = {
        let t = task.lock().await;
        t.files.get(file_path).map(|f| f.total).unwrap_or(0)
    };
    let content_length = resp.content_length().unwrap_or(0);
    let file_total = if status == reqwest::StatusCode::PARTIAL_CONTENT {
        existing_size + content_length
    } else if content_length > 0 {
        content_length
    } else {
        known_total
    };

    // Update total
    {
        let mut t = task.lock().await;
        if let Some(f) = t.files.get_mut(file_path) {
            f.total = file_total;
            f.downloaded = existing_size;
        }
    }

    // Open file for writing (append if resuming)
    let mut file = if existing_size > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
        match OpenOptions::new().append(true).open(&local_path).await {
            Ok(f) => f,
            Err(e) => {
                {
                    let mut t = task.lock().await;
                    if let Some(f) = t.files.get_mut(file_path) {
                        f.status = FileStatus::Failed;
                        f.error = Some(format!("{}: {}", i18n::t("err_open_file", &lang), e));
                    }
                }
                send_file(&tx, &task, file_path).await;
                return;
            }
        }
    } else {
        match File::create(&local_path).await {
            Ok(f) => f,
            Err(e) => {
                {
                    let mut t = task.lock().await;
                    if let Some(f) = t.files.get_mut(file_path) {
                        f.status = FileStatus::Failed;
                        f.error = Some(format!("{}: {}", i18n::t("err_create_file", &lang), e));
                    }
                }
                send_file(&tx, &task, file_path).await;
                return;
            }
        }
    };

    // Stream download using the standard `bytes_stream()` (HTTP/2 friendly).
    let mut downloaded = existing_size;
    let mut last_time = std::time::Instant::now();
    let mut last_bytes = downloaded;
    let mut last_progress = std::time::Instant::now();
    let mut emit_counter = 0u32;
    let read_timeout = std::time::Duration::from_secs(60);

    let mut stream = resp.bytes_stream();
    loop {
        // Check cancel
        if cancelled.load(Ordering::Relaxed) {
            {
                let mut t = task.lock().await;
                if let Some(f) = t.files.get_mut(file_path) {
                    f.status = FileStatus::Cancelled;
                }
            }
            send_file(&tx, &task, file_path).await;
            return;
        }
        {
            let is_cancelled = {
                let t = task.lock().await;
                t.files
                    .get(file_path)
                    .map(|f| f.status == FileStatus::Cancelled)
                    .unwrap_or(false)
            };
            if is_cancelled {
                send_file(&tx, &task, file_path).await;
                return;
            }
        }

        let item = stream.next().await;
        let bytes = match item {
            Some(Ok(b)) => b,
            Some(Err(e)) => {
                {
                    let mut t = task.lock().await;
                    if let Some(f) = t.files.get_mut(file_path) {
                        f.status = FileStatus::Failed;
                        f.error = Some(format!("{}: {}", i18n::t("err_download_interrupted", &lang), e));
                    }
                }
                send_file(&tx, &task, file_path).await;
                return;
            }
            None => {
                break;
            }
        };

        if bytes.is_empty() {
            // Some proxies keep the connection open and emit empty frames after the body
            // is done. If we've made no real progress for `read_timeout`, treat it as a
            // stall instead of spinning forever.
            if last_progress.elapsed() > read_timeout {
                {
                    let mut t = task.lock().await;
                    if let Some(f) = t.files.get_mut(file_path) {
                        f.status = FileStatus::Failed;
                        f.error = Some(i18n::t("err_timeout", &lang));
                    }
                }
                send_file(&tx, &task, file_path).await;
                return;
            }
            continue;
        }
            if let Err(e) = file.write_all(&bytes).await {
            {
                let mut t = task.lock().await;
                if let Some(f) = t.files.get_mut(file_path) {
                    f.status = FileStatus::Failed;
                    f.error = Some(format!("{}: {}", i18n::t("err_write", &lang), e));
                }
            }
            send_file(&tx, &task, file_path).await;
            return;
        }
        downloaded += bytes.len() as u64;
        last_progress = std::time::Instant::now();

        // Safety net: if the proxy never sends EOF but we already have the expected number
        // of bytes, treat the download as complete.
        if file_total > 0 && downloaded >= file_total {
            break;
        }

        // Calculate speed & emit progress (throttled)
        emit_counter += 1;
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(last_time).as_secs_f64();

        if elapsed > 0.3 || emit_counter % 50 == 0 {
            let file_snapshot = {
                let mut t = task.lock().await;
                if let Some(f) = t.files.get_mut(file_path) {
                    f.downloaded = downloaded;
                    if elapsed > 0.3 {
                        // Real measurement over the elapsed window; smooth it with an EMA
                        // so the displayed number doesn't jump around frame to frame.
                        let s = (downloaded - last_bytes) as f64 / elapsed;
                        last_time = now;
                        last_bytes = downloaded;
                        f.speed = f.speed * 0.4 + s * 0.6;
                    }
                    // On the 50-chunk throttle path (elapsed <= 0.3s) we keep the previous
                    // speed instead of zeroing it, otherwise the UI hides/shows the speed
                    // label every few chunks and it flickers.
                }
                t.files.get(file_path).cloned()
            };
            if let Some(f) = file_snapshot {
                send_progress(&tx, f);
            }
        }
    }

    // Flush
    let _ = file.flush().await;

    // Mark done
    {
        let mut t = task.lock().await;
        if let Some(f) = t.files.get_mut(file_path) {
            if cancelled.load(Ordering::Relaxed) {
                f.status = FileStatus::Cancelled;
            } else {
                f.status = FileStatus::Done;
                f.downloaded = downloaded;
                f.total = if file_total > 0 { file_total } else { downloaded };
                f.speed = 0.0;
            }
        }
    }
    send_file(&tx, &task, file_path).await;

    // Check if all done
    let all_done = {
        let t = task.lock().await;
        t.is_done()
    };
    if all_done {
        let _ = tx.send(UiMsg::Done {
            task_id: task_id.to_string(),
        });
    }
}

fn send_progress(tx: &Sender<UiMsg>, file: FileState) {
    let _ = tx.send(UiMsg::File(file));
}

/// Lock the shared task, clone the requested file's state, release the lock, then emit.
/// Must NOT be called while the `task` mutex is already held by the caller (tokio's
/// `Mutex` is not reentrant) — that was the original deadlock that froze downloads.
async fn send_file(tx: &Sender<UiMsg>, task: &Arc<Mutex<DownloadTask>>, file_path: &str) {
    let file = {
        let t = task.lock().await;
        match t.files.get(file_path) {
            Some(f) => f.clone(),
            None => return,
        }
    };
    send_progress(tx, file);
}

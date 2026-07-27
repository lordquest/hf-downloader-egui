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
            // Reuse the existing task, but apply any settings the user changed since it was
            // created (mirror endpoint / download dir / revision). Without this, a "continue"
            // after switching to a mirror endpoint would silently keep using the old endpoint.
            let mut task = t.lock().await;
            task.endpoint = endpoint.to_string();
            task.target_dir = target_dir.to_string();
            task.repo_type = repo_type.to_string();
            task.revision = revision.to_string();
            // Clear a previous cancellation so the (re)start actually proceeds. The
            // `cancelled` flag was only ever set to true on cancel and never reset, which
            // made every "continue"/retry instantly bail out (no resume possible).
            task.cancelled.store(false, Ordering::Relaxed);
            drop(task);
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
            f.status = FileStatus::Downloading;
        }
    }
    send_file(&tx, &task, file_path).await;

    // Build local path
    let local_path = Path::new(&target_dir).join(file_path);
    if let Some(parent) = local_path.parent() {
        let _ = fs::create_dir_all(parent).await;
    }

    let known_total = {
        let t = task.lock().await;
        t.files.get(file_path).map(|f| f.total).unwrap_or(0)
    };

    // Bounded retries so a transient network blip (the cause of "unstable speed" that
    // otherwise failed the whole file) heals itself. Each retry re-reads the on-disk
    // size, so it truly resumes from where it left off.
    const MAX_ATTEMPTS: u32 = 4; // 1 initial attempt + up to 3 retries
    let mut attempt: u32 = 0;
    #[allow(unused_assignments)]
    let mut file_total = 0u64;

    'download: loop {
        // Honour cancellation at the top of every (re)attempt.
        if cancelled.load(Ordering::Relaxed) {
            mark_cancelled(&task, &tx, file_path).await;
            return;
        }

        let existing_size = if local_path.exists() {
            fs::metadata(&local_path).await.map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        // Already complete?
        if known_total > 0 && existing_size >= known_total {
            {
                let mut t = task.lock().await;
                if let Some(f) = t.files.get_mut(file_path) {
                    f.status = FileStatus::Exists;
                    f.downloaded = existing_size;
                    f.total = if f.total > 0 { f.total } else { existing_size };
                }
            }
            send_file(&tx, &task, file_path).await;
            break 'download;
        }

        // Build download URL
        let base = if endpoint.trim().is_empty() {
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
            .header("User-Agent", concat!("hf-downloader-egui/", env!("CARGO_PKG_VERSION")));

        // Attach token
        if let Some(token) = read_token() {
            req = req.bearer_auth(token);
        }

        // Resume from existing_size (mirrors/official both honour Range; if a server
        // ignores it and returns 200 we fall back to a clean re-download below).
        if existing_size > 0 {
            req = req.header("Range", format!("bytes={}-", existing_size));
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                if attempt < MAX_ATTEMPTS && !cancelled.load(Ordering::Relaxed) {
                    attempt += 1;
                    backoff(attempt).await;
                    continue 'download;
                }
                fail(
                    &task,
                    &tx,
                    file_path,
                    format!("{}: {}", i18n::t("err_request", &lang), e),
                )
                .await;
                return;
            }
        };

        let status = resp.status();
        if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
            // Retry on transient server errors / rate limits; give up on auth/not-found.
            let retryable = status == reqwest::StatusCode::REQUEST_TIMEOUT
                || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status.as_u16() >= 500;
            if retryable && attempt < MAX_ATTEMPTS && !cancelled.load(Ordering::Relaxed) {
                attempt += 1;
                backoff(attempt).await;
                continue 'download;
            }
            fail(&task, &tx, file_path, format!("HTTP {}", status)).await;
            return;
        }

        // Determine total size.
        // NOTE: hf-mirror's resolve-cache returns `Content-Length: None`. Fall back to the
        // size we already know from the file list, so the UI shows a real total and we can
        // detect completion even when the stream never sends EOF (proxy keeps connection open).
        let content_length = resp.content_length().unwrap_or(0);
        file_total = if status == reqwest::StatusCode::PARTIAL_CONTENT {
            existing_size + content_length
        } else if content_length > 0 {
            content_length
        } else {
            known_total
        };

        // Open file for writing (append if resuming via 206; otherwise create/truncate).
        let mut file = if existing_size > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
            match OpenOptions::new().append(true).open(&local_path).await {
                Ok(f) => f,
                Err(e) => {
                    fail(
                        &task,
                        &tx,
                        file_path,
                        format!("{}: {}", i18n::t("err_open_file", &lang), e),
                    )
                    .await;
                    return;
                }
            }
        } else {
            match File::create(&local_path).await {
                Ok(f) => f,
                Err(e) => {
                    fail(
                        &task,
                        &tx,
                        file_path,
                        format!("{}: {}", i18n::t("err_create_file", &lang), e),
                    )
                    .await;
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
        let mut fatal = false;
        let mut fatal_msg = String::new();
        'stream: loop {
            // Check cancel
            if cancelled.load(Ordering::Relaxed) {
                mark_cancelled(&task, &tx, file_path).await;
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
                    mark_cancelled(&task, &tx, file_path).await;
                    return;
                }
            }

            let item = stream.next().await;
            let bytes = match item {
                Some(Ok(b)) => b,
                Some(Err(e)) => {
                    fatal_msg = format!(
                        "{}: {}",
                        i18n::t("err_download_interrupted", &lang),
                        e
                    );
                    fatal = true;
                    break 'stream;
                }
                None => break 'stream,
            };

            if bytes.is_empty() {
                // Some proxies keep the connection open and emit empty frames after the body
                // is done. If we've made no real progress for `read_timeout`, treat it as a
                // stall instead of spinning forever.
                if last_progress.elapsed() > read_timeout {
                    fatal_msg = i18n::t("err_timeout", &lang);
                    fatal = true;
                    break 'stream;
                }
                continue;
            }
            if let Err(e) = file.write_all(&bytes).await {
                fatal_msg = format!("{}: {}", i18n::t("err_write", &lang), e);
                fatal = true;
                break 'stream;
            }
            downloaded += bytes.len() as u64;
            last_progress = std::time::Instant::now();

            // Safety net: if the proxy never sends EOF but we already have the expected number
            // of bytes, treat the download as complete.
            if file_total > 0 && downloaded >= file_total {
                break 'stream;
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

        if fatal {
            // Persist how much we actually got before (possibly) retrying or failing.
            {
                let mut t = task.lock().await;
                if let Some(f) = t.files.get_mut(file_path) {
                    f.downloaded = downloaded;
                }
            }
            send_file(&tx, &task, file_path).await;
            if attempt < MAX_ATTEMPTS && !cancelled.load(Ordering::Relaxed) {
                attempt += 1;
                backoff(attempt).await;
                continue 'download;
            }
            fail(&task, &tx, file_path, fatal_msg).await;
            return;
        }

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
        break 'download;
    }

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

async fn mark_cancelled(task: &Arc<Mutex<DownloadTask>>, tx: &Sender<UiMsg>, file_path: &str) {
    {
        let mut t = task.lock().await;
        if let Some(f) = t.files.get_mut(file_path) {
            f.status = FileStatus::Cancelled;
        }
    }
    send_file(tx, task, file_path).await;
}

async fn fail(task: &Arc<Mutex<DownloadTask>>, tx: &Sender<UiMsg>, file_path: &str, msg: String) {
    {
        let mut t = task.lock().await;
        if let Some(f) = t.files.get_mut(file_path) {
            f.status = FileStatus::Failed;
            f.error = Some(msg);
        }
    }
    send_file(tx, task, file_path).await;
}

/// Exponential-ish backoff between retry attempts (capped).
async fn backoff(attempt: u32) {
    let secs = [1u64, 2, 5, 10];
    let s = secs
        .get((attempt as usize).saturating_sub(1))
        .copied()
        .unwrap_or(10);
    tokio::time::sleep(std::time::Duration::from_secs(s)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::DownloadEngine;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;

    struct ServerHandle {
        url: String,
        #[allow(dead_code)]
        handle: thread::JoinHandle<()>,
    }

    /// Serve `blob` over HTTP/1.1 with Range support. The first `fail_first`
    /// connections return 500 (to exercise the retry path), then it serves 200/206.
    fn serve_blob(blob: Arc<Vec<u8>>, fail_first: Arc<AtomicUsize>) -> ServerHandle {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                if fail_first.load(Ordering::SeqCst) > 0 {
                    fail_first.fetch_sub(1, Ordering::SeqCst);
                    let body = b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                    continue;
                }
                // Parse "Range: bytes=START-"
                let start: usize = {
                    let low = req.to_ascii_lowercase();
                    if let Some(pos) = low.find("range:") {
                        let rest = &req[pos + 6..];
                        let end = rest
                            .find("\r\n")
                            .unwrap_or_else(|| rest.find('\n').unwrap_or(rest.len()));
                        let val = rest[..end].trim().to_string();
                        val.split('=')
                            .nth(1)
                            .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok())
                            .unwrap_or(0)
                    } else {
                        0
                    }
                };
                let total = blob.len();
                if start > 0 && start < total {
                    let body = &blob[start..];
                    let headers = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        start,
                        total - 1,
                        total,
                        body.len()
                    );
                    let _ = stream.write_all(headers.as_bytes());
                    let _ = stream.write_all(body);
                } else {
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        total
                    );
                    let _ = stream.write_all(headers.as_bytes());
                    let _ = stream.write_all(&blob);
                }
                let _ = stream.flush();
            }
        });
        ServerHandle {
            url: format!("http://{}", addr),
            handle,
        }
    }

    fn run_download(task: Arc<Mutex<DownloadTask>>, file_path: &str, lang: &str) {
        let (tx, _rx) = mpsc::channel::<UiMsg>();
        download_runtime().block_on(download_file(
            task,
            tx,
            "task-test",
            file_path,
            lang.to_string(),
        ));
    }

    fn file_state(task: &Arc<Mutex<DownloadTask>>) -> (FileStatus, u64, u64) {
        download_runtime().block_on(async {
            let t = task.lock().await;
            let f = t.files.get("file.bin").unwrap();
            (f.status.clone(), f.downloaded, f.total)
        })
    }

    #[test]
    fn full_download_succeeds() {
        let blob: Arc<Vec<u8>> = Arc::new((0u8..=255).cycle().take(200_000).collect());
        let srv = serve_blob(blob.clone(), Arc::new(AtomicUsize::new(0)));
        let dir = std::env::temp_dir().join(format!("hf_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("file.bin");
        let _ = std::fs::remove_file(&out);
        let task = Arc::new(Mutex::new({
            let mut t = DownloadTask::new(
                "repo".into(),
                "model".into(),
                "main".into(),
                dir.to_string_lossy().to_string(),
                srv.url.clone(),
            );
            t.files.insert(
                "file.bin".into(),
                FileState {
                    path: "file.bin".into(),
                    status: FileStatus::Pending,
                    downloaded: 0,
                    total: blob.len() as u64,
                    speed: 0.0,
                    error: None,
                },
            );
            t
        }));
        run_download(task.clone(), "file.bin", "en");
        let (st, dl, _) = file_state(&task);
        assert_eq!(st, FileStatus::Done, "expected Done");
        assert_eq!(dl, blob.len() as u64);
        assert_eq!(std::fs::read(&out).unwrap(), *blob);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn resume_after_cancel_continues() {
        let blob: Arc<Vec<u8>> = Arc::new((0u8..=255).cycle().take(200_000).collect());
        let srv = serve_blob(blob.clone(), Arc::new(AtomicUsize::new(0)));
        let dir = std::env::temp_dir().join(format!("hf_test_resume_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("file.bin");
        let _ = std::fs::remove_file(&out);
        let partial = (blob.len() as f64 * 0.4) as usize;
        std::fs::write(&out, &blob[..partial]).unwrap();
        let task = Arc::new(Mutex::new({
            let mut t = DownloadTask::new(
                "repo".into(),
                "model".into(),
                "main".into(),
                dir.to_string_lossy().to_string(),
                srv.url.clone(),
            );
            // Simulate the bug: a previously cancelled task whose flag is stuck true.
            t.cancelled.store(true, Ordering::SeqCst);
            t.files.insert(
                "file.bin".into(),
                FileState {
                    path: "file.bin".into(),
                    status: FileStatus::Cancelled,
                    downloaded: partial as u64,
                    total: blob.len() as u64,
                    speed: 0.0,
                    error: None,
                },
            );
            t
        }));
        // A real "continue" resets cancelled (get_or_create does this); emulate it:
        download_runtime().block_on(async {
            task.lock().await.cancelled.store(false, Ordering::SeqCst);
        });
        run_download(task.clone(), "file.bin", "en");
        let (st, dl, _) = file_state(&task);
        assert_eq!(st, FileStatus::Done, "resume should finish");
        assert_eq!(dl, blob.len() as u64);
        assert_eq!(
            std::fs::read(&out).unwrap(),
            *blob,
            "resumed file must equal original"
        );
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn get_or_create_applies_new_endpoint() {
        let mgr = TaskManager::new();
        let rt = download_runtime();
        let t1 = rt.block_on(mgr.get_or_create(
            "task-x",
            "repo",
            "model",
            "main",
            "/tmp/a",
            "https://huggingface.co",
        ));
        {
            let t = rt.block_on(t1.lock());
            assert_eq!(t.endpoint, "https://huggingface.co");
        }
        // Reuse with a new mirror endpoint + download dir.
        let t2 = rt.block_on(mgr.get_or_create(
            "task-x",
            "repo",
            "model",
            "main",
            "/tmp/b",
            "https://hf-mirror.com",
        ));
        {
            let t = rt.block_on(t2.lock());
            assert_eq!(t.endpoint, "https://hf-mirror.com", "mirror endpoint must take effect on reuse");
            assert_eq!(t.target_dir, "/tmp/b", "download dir must take effect on reuse");
            assert!(
                !t.cancelled.load(Ordering::SeqCst),
                "cancelled must be reset on reuse"
            );
        }
    }

    #[test]
    fn retry_on_transient_errors_succeeds() {
        let blob: Arc<Vec<u8>> = Arc::new((0u8..=255).cycle().take(120_000).collect());
        let fails = Arc::new(AtomicUsize::new(2)); // fail first 2 attempts
        let srv = serve_blob(blob.clone(), fails);
        let dir = std::env::temp_dir().join(format!("hf_test_retry_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("file.bin");
        let _ = std::fs::remove_file(&out);
        let task = Arc::new(Mutex::new({
            let mut t = DownloadTask::new(
                "repo".into(),
                "model".into(),
                "main".into(),
                dir.to_string_lossy().to_string(),
                srv.url.clone(),
            );
            t.files.insert(
                "file.bin".into(),
                FileState {
                    path: "file.bin".into(),
                    status: FileStatus::Pending,
                    downloaded: 0,
                    total: blob.len() as u64,
                    speed: 0.0,
                    error: None,
                },
            );
            t
        }));
        run_download(task.clone(), "file.bin", "en");
        let (st, dl, _) = file_state(&task);
        assert_eq!(st, FileStatus::Done, "should succeed after retries");
        assert_eq!(dl, blob.len() as u64);
        assert_eq!(std::fs::read(&out).unwrap(), *blob);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn retry_cancelled_file_resumes_with_new_endpoint() {
        let blob: Arc<Vec<u8>> = Arc::new((0u8..=255).cycle().take(150_000).collect());
        let srv = serve_blob(blob.clone(), Arc::new(AtomicUsize::new(0)));
        let dir = std::env::temp_dir().join(format!("hf_test_retry2_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("file.bin");
        let _ = std::fs::remove_file(&out);

        let (tx, _rx) = mpsc::channel::<UiMsg>();
        let engine = DownloadEngine::new(tx);
        let rt = download_runtime();
        // Register the task in the ENGINE's own manager (with the OLD endpoint), as if the
        // first download used huggingface.co. This is what engine.retry looks up.
        let task = rt.block_on(
            engine
                ._test_manager()
                .get_or_create("task-r", "repo", "model", "main", &dir.to_string_lossy().to_string(), "https://huggingface.co"),
        );
        // Simulate a stop: file Cancelled + task cancelled flag stuck true.
        rt.block_on(async {
            let mut t = task.lock().await;
            t.cancelled.store(true, Ordering::SeqCst);
            t.files.insert(
                "file.bin".into(),
                FileState {
                    path: "file.bin".into(),
                    status: FileStatus::Cancelled,
                    downloaded: 0,
                    total: blob.len() as u64,
                    speed: 0.0,
                    error: None,
                },
            );
        });

        // Retry with a NEW endpoint (the local mock server stands in for the mirror the
        // user switched to). This is the real scenario: stop -> switch mirror -> retry.
        let new_endpoint = srv.url.clone();
        engine.retry(
            "task-r",
            "file.bin",
            &new_endpoint,
            &dir.to_string_lossy().to_string(),
            "en".to_string(),
        );

        // Wait for the download to actually finish.
        let done = rt.block_on(async {
            for _ in 0..150 {
                let st = {
                    let t = task.lock().await;
                    t.files.get("file.bin").unwrap().status.clone()
                };
                if st == FileStatus::Done {
                    return true;
                }
                if st == FileStatus::Failed {
                    return false;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            false
        });
        assert!(done, "retry of a cancelled file must actually download");
        let ep = rt.block_on(async { task.lock().await.endpoint.clone() });
        assert_eq!(ep, new_endpoint, "retry must apply the new endpoint");
        assert_eq!(std::fs::read(&out).unwrap(), *blob);
        let _ = std::fs::remove_file(&out);
    }
}


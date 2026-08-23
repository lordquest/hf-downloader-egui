//! Internationalization with runtime-loadable language files.
//!
//! How it works
//! ------------
//! - `zh` and `en` are embedded in the binary as fallback, so the app always runs
//!   even if no external files exist.
//! - At startup [`load()`] also scans `<exe_dir>/lang/*.json` and merges them in.
//!   External files can *override* embedded strings and *add new* languages.
//! - [`t(key, lang)`] looks up the active language; if a key is missing it falls back
//!   to English (which is always complete), so the UI never breaks.
//! - [`available()`] returns the list of language codes + native names for the UI.
//!
//! Adding a language (no recompile needed)
//! ----------------------------------------
//! 1. Run the app once so `<exe_dir>/lang/en.json` (the reference template) is created.
//! 2. Copy `en.json` to `fr.json` (the file name *is* the language code).
//! 3. Translate every value; keep the keys unchanged. Add a `"__name__"` entry with the
//!    native language name shown in the language picker.
//! 4. Restart the app — the new language appears automatically in Settings.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

// --- Embedded fallback (zh + en). Always present, always complete. -------------------
const EMBED_ZH: &str = r#"{
  "__name__": "中文",
  "title": "HF 模型下载器",
  "settings": "设置",
  "repo_placeholder": "输入仓库地址,如 owner/repo 或 https://huggingface.co/owner/repo",
  "list_files": "列出文件",
  "select_all": "全选",
  "select_none": "全不选",
  "start": "开始下载",
  "pause_all": "全部暂停",
  "files": "个文件",
  "listed": "已列出",
  "listing": "正在获取文件列表...",
  "starting": "正在开始下载...",
  "done": "下载完成",
  "no_repo": "请先列出文件",
  "no_select": "请先选择要下载的文件",
  "err_empty": "请输入仓库地址",
  "error": "错误",
  "file_list": "文件列表",
  "download_dir": "默认下载目录",
  "endpoint": "镜像/端点 (可留空)",
  "language": "语言",
  "browse": "浏览",
  "save": "保存",
  "close": "关闭",
  "login": "登录 HF",
  "token_placeholder": "粘贴 HF token",
  "status_logged_in": "已登录",
  "status_missing": "未登录",
  "status_invalid": "Token 无效",
  "status_checking": "检查中...",
  "name": "名称",
  "size": "大小",
  "progress": "进度",
  "speed": "速度",
  "eta": "剩余",
  "save_to": "保存到",
  "status": "状态",
  "cancel": "暂停",
  "retry": "继续下载",
  "recovered": "已恢复上次下载会话,并开始继续下载",
  "recovery_title": "恢复下载",
  "recovery_prompt": "检测到上次的下载会话,请选择要恢复的仓库:",
  "resume": "恢复",
  "recovery_cancel": "忽略",
  "saved_at": "保存于",
  "cut": "剪切",
  "copy": "复制",
  "paste": "粘贴",
  "no_files": "暂无文件,请先列出",
  "token_dialog_title": "登录 Hugging Face",
  "settings_title": "设置",
  "about": "HF Downloader (egui 原生版)",
  "pending": "等待中",
  "err_request": "请求失败",
  "err_create_file": "创建文件失败",
  "err_open_file": "打开文件失败",
  "err_write": "写入失败",
  "err_download_interrupted": "下载中断",
  "err_timeout": "下载超时: 长时间无数据",
  "err_parse_url": "无法解析 URL",
  "err_parse_repo": "无法解析仓库标识,期望格式 owner/repo 或完整 HF 网址",
  "err_repo_url": "无法从 URL 解析出仓库标识",
  "err_repo_not_found": "仓库不存在或无权访问: {}",
  "err_gated": "该仓库为受限(gated)仓库,需先授权: {}",
  "err_api": "API 返回错误: {}",
  "err_parse_resp": "解析响应失败: {}",
  "err_token_empty": "token 为空",
  "err_token_invalid": "token 无效 (HTTP {})",
  "downloading": "下载中",
  "exists": "已存在",
  "failed": "失败",
  "cancelled": "已暂停",
  "version": "版本",
  "author": "作者",
  "about_title": "关于"
}"#;

const EMBED_EN: &str = r#"{
  "__name__": "English",
  "title": "HF Model Downloader",
  "settings": "Settings",
  "repo_placeholder": "Enter repo, e.g. owner/repo or https://huggingface.co/owner/repo",
  "list_files": "List Files",
  "select_all": "Select All",
  "select_none": "Select None",
  "start": "Start Download",
  "pause_all": "Pause All",
  "files": "files",
  "listed": "Listed",
  "listing": "Fetching file list...",
  "starting": "Starting download...",
  "done": "Download complete",
  "no_repo": "List files first",
  "no_select": "Select files to download",
  "err_empty": "Enter a repo address",
  "error": "Error",
  "file_list": "File List",
  "download_dir": "Default download directory",
  "endpoint": "Mirror/Endpoint (optional)",
  "language": "Language",
  "browse": "Browse",
  "save": "Save",
  "close": "Close",
  "login": "Login HF",
  "token_placeholder": "Paste HF token",
  "status_logged_in": "Logged in",
  "status_missing": "Not logged in",
  "status_invalid": "Token invalid",
  "status_checking": "Checking...",
  "name": "Name",
  "size": "Size",
  "progress": "Progress",
  "speed": "Speed",
  "eta": "ETA",
  "save_to": "Save to",
  "status": "Status",
  "cancel": "Pause",
  "retry": "Resume",
  "recovered": "Resumed previous download session and continuing",
  "recovery_title": "Resume download",
  "recovery_prompt": "Found previous download sessions. Choose a repository to resume:",
  "resume": "Resume",
  "recovery_cancel": "Dismiss",
  "saved_at": "Saved at",
  "cut": "Cut",
  "copy": "Copy",
  "paste": "Paste",
  "no_files": "No files yet, list first",
  "token_dialog_title": "Login to Hugging Face",
  "settings_title": "Settings",
  "about": "HF Downloader (egui native)",
  "pending": "Pending",
  "err_request": "Request failed",
  "err_create_file": "Failed to create file",
  "err_open_file": "Failed to open file",
  "err_write": "Write failed",
  "err_download_interrupted": "Download interrupted",
  "err_timeout": "Download timeout: no data for a long time",
  "err_parse_url": "Cannot parse URL",
  "err_parse_repo": "Cannot parse repo id, expected owner/repo or a full HF URL",
  "err_repo_url": "Cannot parse repo id from URL",
  "err_repo_not_found": "Repo not found or no access: {}",
  "err_gated": "This is a gated repo, authorize first: {}",
  "err_api": "API returned error: {}",
  "err_parse_resp": "Failed to parse response: {}",
  "err_token_empty": "token is empty",
  "err_token_invalid": "token invalid (HTTP {})",
  "downloading": "Downloading",
  "exists": "Exists",
  "failed": "Failed",
  "cancelled": "Paused",
  "version": "Version",
  "author": "Author",
  "about_title": "About"
}"#;

struct LangRegistry {
    /// lang code -> (key -> translated string)
    langs: HashMap<String, HashMap<String, String>>,
    /// stable display order for the picker (zh, en, then any external langs)
    order: Vec<String>,
}

static REGISTRY: OnceLock<LangRegistry> = OnceLock::new();

fn lang_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("lang")))
}

fn parse_lang_json(json: &str) -> Result<HashMap<String, String>, serde_json::Error> {
    serde_json::from_str::<HashMap<String, String>>(json)
}

fn build_registry() -> LangRegistry {
    let mut langs: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    // 1) Embedded fallback (always complete).
    for (code, json) in [("zh", EMBED_ZH), ("en", EMBED_EN)] {
        if let Ok(map) = parse_lang_json(json) {
            langs.insert(code.to_string(), map);
            order.push(code.to_string());
        }
    }

    // 2) External files: override embedded and add new languages.
    if let Some(dir) = lang_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let Some(code) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if code.is_empty() {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(map) = parse_lang_json(&text) {
                        langs.insert(code.to_string(), map);
                        if !order.contains(&code.to_string()) {
                            order.push(code.to_string());
                        }
                    }
                }
            }
        }
    }

    LangRegistry { langs, order }
}

/// Initialize the translation registry. Safe to call multiple times (only the first
/// call takes effect). Also materializes `<exe_dir>/lang/en.json` (+ zh.json) as a
/// translation template if they don't already exist.
pub fn load() {
    let _ = REGISTRY.set(build_registry());
    ensure_lang_dir();
}

fn ensure_lang_dir() {
    if let Some(dir) = lang_dir() {
        let _ = std::fs::create_dir_all(&dir);
        write_once(&dir.join("en.json"), EMBED_EN);
        write_once(&dir.join("zh.json"), EMBED_ZH);
    }
}

fn write_once(path: &PathBuf, content: &str) {
    if !path.exists() {
        let _ = std::fs::write(path, content);
    }
}

/// Translate `key` for `lang`. Falls back to English, then to the key itself.
pub fn t(key: &str, lang: &str) -> String {
    let reg = REGISTRY.get().expect("i18n::load() not called");
    if let Some(map) = reg.langs.get(lang) {
        if let Some(v) = map.get(key) {
            return v.clone();
        }
    }
    if lang != "en" {
        if let Some(map) = reg.langs.get("en") {
            if let Some(v) = map.get(key) {
                return v.clone();
            }
        }
    }
    key.to_string()
}

/// All available languages as `(code, native_name)` in display order.
pub fn available() -> Vec<(String, String)> {
    let reg = REGISTRY.get().expect("i18n::load() not called");
    let mut out = Vec::new();
    for code in &reg.order {
        let name = reg
            .langs
            .get(code)
            .and_then(|m| m.get("__name__"))
            .cloned()
            .unwrap_or_else(|| code.clone());
        out.push((code.clone(), name));
    }
    out
}

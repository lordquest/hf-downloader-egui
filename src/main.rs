#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod download;
mod engine;
mod hf_api;
mod i18n;
mod session;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;

use eframe::egui;

use crate::config::AppConfig;
use crate::download::{FileState, FileStatus};
use crate::engine::{DownloadEngine, UiMsg};
use crate::hf_api::{FileEntry, RepoInfo, TokenStatus};
use crate::session::Session;

struct App {
    rx: Receiver<UiMsg>,
    engine: DownloadEngine,
    config: AppConfig,
    lang: String,

    repo_input: String,
    repo_info: Option<RepoInfo>,
    file_entries: Vec<FileEntry>,
    selected: HashSet<String>,

    file_states: HashMap<String, FileState>,
    active_task_id: Option<String>,

    status: StatusMsg,
    token_status: Option<TokenStatus>,
    token_input: String,

    show_settings: bool,
    show_token_dialog: bool,
    show_about: bool,
    about_menu_pos: Option<egui::Pos2>,
    busy: bool,

    // --- Download session persistence / recovery ---
    /// Snapshot of the currently running download (written to disk so it can be
    /// resumed after a restart). `None` when no download is active.
    current_session: Option<Session>,
    /// Sessions found at startup that can be resumed (after a restart).
    recovery_sessions: Vec<Session>,
    /// Whether the recovery chooser window is open (only when multiple sessions exist).
    show_recovery: bool,
}

/// Status line is stored as a (possibly parameterized) translation key so it can be
/// re-rendered in the current language whenever the user toggles zh/en.
#[derive(Clone)]
enum StatusMsg {
    None,
    Text(String),
    Tr(&'static str),
    TrCount(&'static str, usize, &'static str),
}

impl App {
    fn new(rx: Receiver<UiMsg>, engine: DownloadEngine) -> Self {
        let cfg = config::ConfigState::load();
        let app_config = cfg
            .config
            .into_inner()
            .unwrap_or_else(|_| AppConfig::default());
        let lang = app_config
            .language
            .clone()
            .unwrap_or_else(detect_system_lang);
        let mut app = App {
            rx,
            engine,
            config: app_config,
            lang,
            repo_input: String::new(),
            repo_info: None,
            file_entries: Vec::new(),
            selected: HashSet::new(),
            file_states: HashMap::new(),
            active_task_id: None,
            status: StatusMsg::None,
            token_status: None,
            token_input: String::new(),
            show_settings: false,
            show_token_dialog: false,
            show_about: false,
            about_menu_pos: None,
            busy: false,
            current_session: None,
            recovery_sessions: Vec::new(),
            show_recovery: false,
        };
        app.check_token_async();
        app.init_recovery();
        app
    }

    /// On startup, look for download sessions left by previous runs. If there's
    /// exactly one, resume it directly; if there are several, open a chooser so the
    /// user can pick which repo to resume.
    fn init_recovery(&mut self) {
        let sessions = session::list_recoverable_sessions();
        if sessions.is_empty() {
            return;
        }
        if sessions.len() == 1 {
            self.restore_session(&sessions[0], true);
        } else {
            self.recovery_sessions = sessions;
            self.show_recovery = true;
        }
    }

    /// Restore app state from a saved session and (optionally) start downloading.
    /// The source session file is deleted so it doesn't linger / get re-offered.
    fn restore_session(&mut self, s: &Session, auto_start: bool) {
        self.config.download_dir = s.download_dir.clone();
        self.config.endpoint = s.endpoint.clone();
        self.repo_info = Some(RepoInfo {
            repo_id: s.repo_id.clone(),
            repo_type: s.repo_type.clone(),
            revision: s.revision.clone(),
            subpath: s.subpath.clone(),
        });
        self.file_entries = s
            .files
            .iter()
            .map(|f| FileEntry {
                path: f.path.clone(),
                size: f.size,
            })
            .collect();
        self.selected = s.files.iter().map(|f| f.path.clone()).collect();
        self.file_states.clear();
        for f in &s.files {
            self.file_states.insert(
                f.path.clone(),
                FileState {
                    path: f.path.clone(),
                    status: FileStatus::Pending,
                    downloaded: 0,
                    total: f.size,
                    speed: 0.0,
                    error: None,
                },
            );
        }
        self.active_task_id = Some(format!("task-{}", s.repo_id));
        // Remove the old PID's session file now that we're taking it over.
        session::delete_session_file(&session::path_for_pid(s.pid));
        self.status = StatusMsg::Tr("recovered");
        if auto_start {
            self.start_download();
        }
    }

    fn t(&self, key: &str) -> String {
        i18n::t(key, &self.lang)
    }

    fn save_config(&self) {
        config::save_config(&self.config);
    }

    /// Render the status line in the current language.
    fn status_text(&self) -> String {
        match &self.status {
            StatusMsg::None => String::new(),
            StatusMsg::Text(s) => s.clone(),
            StatusMsg::Tr(k) => self.t(k),
            StatusMsg::TrCount(k, n, u) => {
                format!("{}: {} {}", self.t(k), n, self.t(u))
            }
        }
    }

    // ---- async workers (spawned on OS threads, report back via the channel) ----

    fn check_token_async(&mut self) {
        let tx = self.engine.sender();
        let endpoint = self.config.endpoint.trim().to_string();
        let lang = self.lang.clone();
        std::thread::spawn(move || {
            let rt = download::download_runtime();
            let status = rt.block_on(crate::hf_api::check_token(&endpoint, &lang));
            let _ = tx.send(UiMsg::TokenChecked(status));
        });
    }

    fn list_files_async(&mut self) {
        let input = self.repo_input.trim().to_string();
        self.repo_input = input.clone();
        if input.is_empty() {
            self.status = StatusMsg::Tr("err_empty");
            return;
        }
        self.busy = true;
        self.status = StatusMsg::Tr("listing");
        let tx = self.engine.sender();
        let endpoint = self.config.endpoint.trim().to_string();
        let lang = self.lang.clone();
        std::thread::spawn(move || {
            let rt = download::download_runtime();
            match crate::hf_api::parse_repo_input(&input, &lang) {
                Ok(info) => {
                    match rt.block_on(crate::hf_api::list_files(
                        &info.repo_id,
                        &info.revision,
                        &info.subpath,
                        &info.repo_type,
                        &endpoint,
                        &lang,
                    )) {
                        Ok(entries) => {
                            let _ = tx.send(UiMsg::RepoListed {
                                info,
                                entries,
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(UiMsg::ApiError(e));
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(UiMsg::ApiError(e));
                }
            }
        });
    }

    fn login_async(&mut self) {
        let token = self.token_input.trim().to_string();
        if token.is_empty() {
            return;
        }
        let tx = self.engine.sender();
        let endpoint = self.config.endpoint.trim().to_string();
        let lang = self.lang.clone();
        std::thread::spawn(move || {
            let rt = download::download_runtime();
            let status = rt.block_on(crate::hf_api::login_token(&token, &endpoint, &lang));
            let _ = tx.send(UiMsg::TokenChecked(status));
        });
        self.token_input.clear();
        self.show_token_dialog = false;
    }

    fn start_download(&mut self) {
        let info = match &self.repo_info {
            Some(i) => i.clone(),
            None => {
                self.status = StatusMsg::Tr("no_repo");
                return;
            }
        };
        if self.selected.is_empty() {
            self.status = StatusMsg::Tr("no_select");
            return;
        }

        let target_dir = crate::hf_api::target_dir_for(&info.repo_id, self.config.download_dir.trim());
        let task_id = format!("task-{}", info.repo_id);
        let file_paths: Vec<(String, u64)> = self
            .file_entries
            .iter()
            .filter(|e| self.selected.contains(&e.path))
            .map(|e| (e.path.clone(), e.size))
            .collect();

        // Persist this download as a session so it can be resumed after a restart.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let session = Session {
            session_id: format!("session-{}", std::process::id()),
            pid: std::process::id(),
            repo_id: info.repo_id.clone(),
            repo_type: info.repo_type.clone(),
            revision: info.revision.clone(),
            subpath: info.subpath.clone(),
            endpoint: self.config.endpoint.trim().to_string(),
            download_dir: self.config.download_dir.trim().to_string(),
            target_dir: target_dir.clone(),
            files: file_paths
                .iter()
                .map(|(p, s)| session::SessionFileEntry {
                    path: p.clone(),
                    size: *s,
                })
                .collect(),
            updated_at: now,
        };
        session::write_session(&session);
        self.current_session = Some(session);

        // Pre-populate the progress map so the UI shows rows immediately.
        self.file_states.clear();
        for (p, s) in &file_paths {
            self.file_states.insert(
                p.clone(),
                FileState {
                    path: p.clone(),
                    status: FileStatus::Pending,
                    downloaded: 0,
                    total: *s,
                    speed: 0.0,
                    error: None,
                },
            );
        }
        self.active_task_id = Some(task_id.clone());
        self.status = StatusMsg::Tr("starting");
        self.engine.start(
            &task_id,
            &info.repo_id,
            &info.repo_type,
            &info.revision,
            &target_dir,
            self.config.endpoint.trim(),
            file_paths,
            self.lang.clone(),
        );
    }

    // Drain all pending messages from the worker threads.
    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UiMsg::File(f) => {
                    self.file_states.insert(f.path.clone(), f);
                }
                UiMsg::Done { task_id } => {
                    if self.active_task_id.as_deref() == Some(&task_id) {
                        // Whole task finished — nothing left to resume, so drop the
                        // session file.
                        session::delete_current_session();
                        self.current_session = None;
                        self.status = StatusMsg::Tr("done");
                    }
                }
                UiMsg::RepoListed { info, entries } => {
                    self.repo_info = Some(info);
                    self.file_entries = entries;
                    // Default to nothing selected — the user picks the files to
                    // download. The "Select All" button is still there if they want
                    // everything.
                    self.selected.clear();
                    // Clear any progress rows left over from a previously completed or
                    // abandoned download so they don't get mixed in with the freshly
                    // listed repo (e.g. an overlapping filename would otherwise show a
                    // stale 100% bar).
                    self.file_states.clear();
                    self.active_task_id = None;
                    self.busy = false;
                    self.status =
                        StatusMsg::TrCount("listed", self.file_entries.len(), "files");
                }
                UiMsg::ApiError(e) => {
                    self.status = StatusMsg::Text(format!("{}: {}", self.t("error"), e));
                    self.busy = false;
                }
                UiMsg::TokenChecked(s) => {
                    self.token_status = Some(s.clone());
                    self.config.hf_logged_in = Some(s.status == "logged_in");
                    self.save_config();
                }
            }
        }
    }

    fn status_pair(&self, st: &FileState) -> (String, egui::Color32) {
        match st.status {
            FileStatus::Pending => (self.t("pending"), egui::Color32::GRAY),
            FileStatus::Downloading => (self.t("downloading"), egui::Color32::BLUE),
            FileStatus::Done => (self.t("done"), egui::Color32::GREEN),
            FileStatus::Exists => (self.t("exists"), egui::Color32::GREEN),
            FileStatus::Failed => (self.t("failed"), egui::Color32::RED),
            FileStatus::Cancelled => (self.t("cancelled"), egui::Color32::YELLOW),
        }
    }

    /// Summary text shown next to the "Pause All" button:
    /// - `None` when there is nothing to show (no download at all, or the task is
    ///   paused/queued with nothing in flight),
    /// - `Some("总速度: X")` while files are actively downloading (sum of their speeds),
    /// - `Some("已全部完成")` once every file in the task has finished.
    fn download_summary(&self) -> Option<String> {
        if self.file_states.is_empty() {
            return None;
        }
        let mut total_speed = 0.0_f64;
        let mut downloading = 0_usize;
        let mut all_done = true;
        for f in self.file_states.values() {
            match f.status {
                FileStatus::Downloading => {
                    downloading += 1;
                    total_speed += f.speed;
                    all_done = false;
                }
                // Anything still pending or stopped short of completion means the
                // task isn't fully done yet.
                FileStatus::Pending
                | FileStatus::Failed
                | FileStatus::Cancelled => all_done = false,
                FileStatus::Done | FileStatus::Exists => {}
            }
        }
        if downloading > 0 {
            Some(format!("{}: {}", self.t("total_speed"), fmt_speed(total_speed)))
        } else if all_done {
            Some(self.t("all_done"))
        } else {
            None
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();

        // Keep repainting while anything is actively downloading.
        let active = self
            .file_states
            .values()
            .any(|f| f.status == FileStatus::Downloading);
        if active {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            // Right-click on the egui top bar opens a small context menu with "About".
            let (sec, pos) =
                ctx.input(|i| (i.pointer.secondary_pressed(), i.pointer.interact_pos()));
            if sec && pos.map(|p| ui.max_rect().contains(p)).unwrap_or(false) {
                self.about_menu_pos = pos;
            }
            ui.horizontal(|ui| {
                ui.heading(self.t("title"));
                ui.add_space(8.0);
                // Repo-input hint lives next to the title as a quiet subtitle rather
                // than crowding the input area below.
                ui.label(
                    egui::RichText::new(self.t("repo_placeholder"))
                        .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(self.t("settings")).clicked() {
                        self.show_settings = true;
                    }
                    if ui.button(self.t("about_title")).clicked() {
                        self.show_about = true;
                    }
                    let langs = i18n::available();
                    let cur_name = langs
                        .iter()
                        .find(|(c, _)| c == &self.lang)
                        .map(|(_, n)| n.clone())
                        .unwrap_or_else(|| self.lang.clone());
                    egui::ComboBox::from_label(self.t("language"))
                        .selected_text(cur_name)
                        .show_ui(ui, |ui| {
                            for (code, name) in langs {
                                if ui
                                    .selectable_label(self.lang == code, name)
                                    .clicked()
                                {
                                    self.lang = code.clone();
                                    self.config.language = Some(code);
                                    self.save_config();
                                }
                            }
                        });
                    let (txt, color) = match &self.token_status {
                        None => (self.t("status_checking"), egui::Color32::GRAY),
                        Some(s) if s.status == "logged_in" => {
                            (self.t("status_logged_in"), egui::Color32::GREEN)
                        }
                        Some(s) if s.status == "invalid" => (
                            format!(
                                "{}: {}",
                                self.t("status_invalid"),
                                s.error.clone().unwrap_or_default()
                            ),
                            egui::Color32::RED,
                        ),
                        Some(_) => (self.t("status_missing"), egui::Color32::YELLOW),
                    };
                    ui.colored_label(color, txt);
                    if ui.button(self.t("login")).clicked() {
                        self.show_token_dialog = true;
                    }
                });
            });
        });

        // ---- Bottom save-to bar ----
        // Shown BEFORE the CentralPanel so the central content reserves space for it
        // and the last file row in the list isn't obscured by the bottom panel.
        egui::TopBottomPanel::bottom("bottom_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.t("save_to"));
                let target_dir = self
                    .repo_info
                    .as_ref()
                    .map(|info| {
                        crate::hf_api::target_dir_for(&info.repo_id, self.config.download_dir.trim())
                    })
                    .unwrap_or_else(|| self.config.download_dir.trim().to_string());
                ui.label(
                    egui::RichText::new(target_dir).color(egui::Color32::RED),
                );
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // ---- 1) Repo address input (top of the single vertical column) ----
            ui.horizontal(|ui| {
                let btn_w = 90.0;
                let w = (ui.available_width() - ui.spacing().item_spacing.x - btn_w).max(160.0);
                bordered_edit(ui, &mut self.repo_input, w, &self.lang, "repo_input");
                if ui.button(self.t("list_files")).clicked() {
                    self.list_files_async();
                }
            });
            // Listing status goes on its own line directly under the List Files
            // button (and above the placeholder hint) so it's clearly feedback for
            // that action and stays fully visible.
            let status = self.status_text();
            if !status.is_empty() {
                ui.label(status);
            }

            // ---- 2) Select all / none + start download ----
            ui.horizontal(|ui| {
                if ui.button(self.t("select_all")).clicked() {
                    for e in &self.file_entries {
                        self.selected.insert(e.path.clone());
                    }
                }
                if ui.button(self.t("select_none")).clicked() {
                    self.selected.clear();
                }
                // A little gap before the Start button so the group reads clearly.
                ui.add_space(12.0);
                if ui.button(self.t("start")).clicked() {
                    self.start_download();
                }
                ui.add_space(8.0);
                if ui.button(self.t("pause_all")).clicked() {
                    if let Some(tid) = &self.active_task_id {
                        self.engine.pause_all(tid);
                    }
                }
                // Live summary to the right of Pause All: total speed while downloading,
                // "已全部完成" once everything is done, hidden otherwise.
                if let Some(summary) = self.download_summary() {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new(summary).weak());
                }
            });

            // ---- 3) File list with merged progress (scrollable) ----
            ui.separator();
            ui.heading(self.t("file_list"));
            if self.file_entries.is_empty() {
                ui.label(self.t("no_files"));
            } else {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                    // Collect paths into a local so the loop doesn't hold a borrow of
                    // `self` while the inner closures mutate `self` (selection / cancel).
                    let entries: Vec<String> =
                        self.file_entries.iter().map(|e| e.path.clone()).collect();
                    // Snapshot file sizes so the row can show "大小: X" without
                    // borrowing `self` while the inner closures mutate it.
                    let sizes: std::collections::HashMap<String, u64> = self
                        .file_entries
                        .iter()
                        .map(|e| (e.path.clone(), e.size))
                        .collect();
                    for path in &entries {
                        // Snapshot progress (if any) so nested UI closures that borrow
                        // `self` mutably (cancel / retry) don't conflict with this read.
                        let st = self.file_states.get(path).cloned();
                        ui.group(|ui| {
                            // Two-column row via egui's `columns`: column 0 holds the
                            // checkbox + filename (filename wraps within the column),
                            // column 1 holds the size, right-aligned.
                            ui.columns(2, |cols| {
                                let (left, right) = cols.split_at_mut(1);
                                let name_col = &mut left[0];
                                let size_col = &mut right[0];
                                name_col.horizontal(|ui| {
                                    let mut sel = self.selected.contains(path);
                                    let cb = ui.checkbox(&mut sel, "");
                                    if cb.clicked() {
                                        if sel {
                                            self.selected.insert(path.clone());
                                        } else {
                                            self.selected.remove(path);
                                        }
                                    }
                                    ui.add(egui::Label::new(path).wrap());
                                });
                                // Wrap in `.horizontal` so the column only takes one line
                                // of height — a bare `with_layout` would allocate the full
                                // (large) column height and leave blank space below.
                                size_col.horizontal(|ui| {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if let Some(&sz) = sizes.get(path) {
                                                ui.label(format!(
                                                    "{}: {}",
                                                    self.t("size"),
                                                    fmt_size(sz)
                                                ));
                                            }
                                        },
                                    );
                                });
                            });

                            if let Some(s) = &st {
                                let frac = if s.total > 0 {
                                    (s.downloaded as f32 / s.total as f32).clamp(0.0, 1.0)
                                } else {
                                    0.0
                                };
                                let bar_text = format!(
                                    "{} / {}  ({:.1}%)",
                                    fmt_size(s.downloaded),
                                    fmt_size(s.total),
                                    frac * 100.0
                                );
                                // Progress bar sits on its own line, directly under the
                                // filename (so long filenames push it below, as requested).
                                ui.add(egui::ProgressBar::new(frac).text(bar_text));

                                let (txt, color) = self.status_pair(s);
                                let speed_txt = fmt_speed(s.speed);
                                let eta_txt = if s.status == FileStatus::Downloading
                                    && s.speed > 0.0
                                    && s.total > s.downloaded
                                {
                                    format!(
                                        "{} {}",
                                        self.t("eta"),
                                        fmt_eta(s.total - s.downloaded, s.speed)
                                    )
                                } else {
                                    String::new()
                                };

                                ui.horizontal(|ui| {
                                    ui.colored_label(color, txt);
                                    if !speed_txt.is_empty() {
                                        ui.label(speed_txt);
                                    }
                                    if !eta_txt.is_empty() {
                                        ui.label(eta_txt);
                                    }
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let active = s.status == FileStatus::Downloading
                                                || s.status == FileStatus::Pending;
                                            if active {
                                                if ui.button(self.t("cancel")).clicked() {
                                                    if let Some(tid) = &self.active_task_id {
                                                        self.engine.pause_file(tid, path);
                                                    }
                                                }
                                            } else if s.status == FileStatus::Failed
                                                || s.status == FileStatus::Cancelled
                                            {
                                                if ui.button(self.t("retry")).clicked() {
                                                    if let Some(tid) = &self.active_task_id {
                                                        let target_dir = self
                                                            .repo_info
                                                            .as_ref()
                                                            .map(|i| {
                                                                crate::hf_api::target_dir_for(
                                                                    &i.repo_id,
                                                                    self.config.download_dir.trim(),
                                                                )
                                                            })
                                                            .unwrap_or_else(|| {
                                                                self.config
                                                                    .download_dir
                                                                    .trim()
                                                                    .to_string()
                                                            });
                                                        self.engine.retry(
                                                            tid,
                                                            path,
                                                            self.config.endpoint.trim(),
                                                            &target_dir,
                                                            self.lang.clone(),
                                                        );
                                                    }
                                                }
                                            }
                                        },
                                    );
                                });

                                if let Some(err) = &s.error {
                                    ui.colored_label(egui::Color32::RED, err);
                                }
                            }
                        });
                    }
                });
            }
        });

        if self.show_settings {
            egui::Window::new(self.t("settings_title"))
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label(self.t("download_dir"));
                    ui.horizontal(|ui| {
                        let w = ui.available_width().min(420.0);
                        bordered_edit(ui, &mut self.config.download_dir, w, &self.lang, "settings_download_dir");
                        if ui.button(self.t("browse")).clicked() {
                            if let Some(f) = rfd::FileDialog::new().pick_folder() {
                                self.config.download_dir = f.to_string_lossy().to_string();
                                self.config.download_dir_set = true;
                            }
                        }
                    });
                    ui.label(self.t("endpoint"));
                    let w = ui.available_width().min(420.0);
                    bordered_edit(ui, &mut self.config.endpoint, w, &self.lang, "settings_endpoint");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button(self.t("save")).clicked() {
                            self.config.download_dir = self.config.download_dir.trim().to_string();
                            self.config.endpoint = self.config.endpoint.trim().to_string();
                            self.save_config();
                            self.show_settings = false;
                        }
                        if ui.button(self.t("close")).clicked() {
                            self.show_settings = false;
                        }
                    });
                });
        }

        if self.show_token_dialog {
            egui::Window::new(self.t("token_dialog_title"))
                .show(ctx, |ui| {
                    ui.label(self.t("token_placeholder"));
                    bordered_edit(ui, &mut self.token_input, f32::INFINITY, &self.lang, "token_input");
                    ui.horizontal(|ui| {
                        if ui.button(self.t("login")).clicked() {
                            self.login_async();
                        }
                        if ui.button(self.t("close")).clicked() {
                            self.show_token_dialog = false;
                        }
                    });
                });
        }

        if self.show_about {
            let about = self.t("about");
            let version_line = format!("{}: {}", self.t("version"), env!("CARGO_PKG_VERSION"));
            let author_line = format!("{}: {}", self.t("author"), "lordquest@163.com");
            let title = self.t("about_title");
            egui::Window::new(title)
                .open(&mut self.show_about)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(about);
                    ui.label(version_line);
                    ui.label(author_line);
                });
        }

        // Small context menu shown at the cursor when the top bar is right-clicked.
        let menu_pos = self.about_menu_pos;
        if let Some(pos) = menu_pos {
            let inner = egui::Window::new("about_ctx")
                .fixed_pos(pos)
                .title_bar(false)
                .resizable(false)
                .show(ctx, |ui| {
                    if ui.button(self.t("about_title")).clicked() {
                        self.show_about = true;
                        self.about_menu_pos = None;
                    }
                });
            if let Some(r) = inner {
                let (pressed, ppos) =
                    ctx.input(|i| (i.pointer.any_pressed(), i.pointer.interact_pos()));
                if pressed && ppos.map(|p| !r.response.rect.contains(p)).unwrap_or(false) {
                    self.about_menu_pos = None;
                }
            }
        }

        // Recovery chooser: shown at startup when more than one previous download
        // session was found, so the user can pick which repo to resume.
        if self.show_recovery {
            let sessions = self.recovery_sessions.clone();
            egui::Window::new(self.t("recovery_title"))
                .collapsible(false)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label(self.t("recovery_prompt"));
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for s in &sessions {
                                ui.group(|ui| {
                                    ui.heading(&s.repo_id);
                                    ui.label(format!(
                                        "{}: {} · {}: {}",
                                        self.t("files"),
                                        s.files.len(),
                                        self.t("saved_at"),
                                        s.updated_at
                                    ));
                                    if ui.button(self.t("resume")).clicked() {
                                        self.restore_session(s, true);
                                        self.show_recovery = false;
                                        self.recovery_sessions.clear();
                                    }
                                });
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui.button(self.t("recovery_cancel")).clicked() {
                            self.show_recovery = false;
                            self.recovery_sessions.clear();
                        }
                    });
                });
        }
    }
}

fn fmt_size(b: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = b as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", b as u64)
    }
}

fn fmt_speed(bps: f64) -> String {
    if bps <= 0.0 {
        return String::new();
    }
    format!("{}/s", fmt_size(bps as u64))
}

/// Estimated time to finish, derived from the average download speed:
/// `remaining_bytes / speed`. Returns e.g. "1:23" or "1:02:03"; empty if unknown.
fn fmt_eta(remaining: u64, speed: f64) -> String {
    if speed <= 0.0 || remaining == 0 {
        return String::new();
    }
    let secs = (remaining as f64 / speed).ceil() as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}

/// Default UI language: follow the OS UI language (zh* -> Chinese, anything else ->
/// English). Only used when the user hasn't explicitly chosen & saved a language.
fn detect_system_lang() -> String {
    match sys_locale::get_locale() {
        Some(l) if l.to_lowercase().starts_with("zh") => "zh".to_string(),
        _ => "en".to_string(),
    }
}

/// Load a CJK font so Chinese text renders (egui's bundled font is Latin-only).
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in font_candidates() {
        if let Ok(bytes) = std::fs::read(&path) {
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
            if let Some(p) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                p.push("cjk".to_owned());
            }
            if let Some(m) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                m.push("cjk".to_owned());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

/// Candidate CJK font paths: a font bundled next to the exe (copied by build.rs
/// from `src/fonts`), followed by OS-specific system fonts.
fn font_candidates() -> Vec<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = Vec::new();

    // 1) Bundled font sitting next to the executable (`<exe_dir>/fonts/*`).
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    {
        if let Ok(entries) = std::fs::read_dir(exe_dir.join("fonts")) {
            for e in entries.flatten() {
                let path = e.path();
                if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                    if matches!(ext, "ttf" | "ttc" | "otf" | "otc") {
                        v.push(path);
                    }
                }
            }
        }
    }

    // 2) OS-specific system fonts.
    if cfg!(target_os = "windows") {
        v.push("C:/Windows/Fonts/msyh.ttc".into());
        v.push("C:/Windows/Fonts/msyh.ttf".into());
        v.push("C:/Windows/Fonts/simhei.ttf".into());
        v.push("C:/Windows/Fonts/simsun.ttc".into());
    } else if cfg!(target_os = "macos") {
        v.push("/System/Library/Fonts/PingFang.ttc".into());
        v.push("/System/Library/Fonts/STHeiti Light.ttc".into());
        v.push("/Library/Fonts/Arial Unicode.ttf".into());
    } else {
        // Linux: common locations for Noto CJK / WenQuanYi, then a directory scan.
        v.push("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc".into());
        v.push("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into());
        v.push("/usr/share/fonts/truetype/noto/NotoSansSC-Regular.otf".into());
        v.push("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc".into());
        v.push("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc".into());
        v.push("/usr/share/fonts/otf/noto/NotoSansCJK-Regular.otf".into());
        for dir in [
            "/usr/share/fonts/truetype/noto",
            "/usr/share/fonts/opentype/noto",
            "/usr/share/fonts/truetype/wqy",
            "/usr/share/fonts/otf/noto",
        ] {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let path = e.path();
                    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                        if matches!(ext, "ttf" | "ttc" | "otf" | "otc") {
                            v.push(path);
                        }
                    }
                }
            }
        }
    }
    v
}

/// A single-line text box with a black border and white background so it stands
/// out, regardless of the active theme. `desired_width` controls how wide it is
/// (use `f32::INFINITY` to fill the available space). `id_src` must be unique per
/// field so the right-click menu can track the cursor; `lang` selects the menu
/// language.
fn bordered_edit(
    ui: &mut egui::Ui,
    text: &mut String,
    desired_width: f32,
    lang: &str,
    id_src: &str,
) {
    let id = ui.make_persistent_id(id_src);
    egui::Frame::none()
        .stroke(egui::Stroke::new(1.5_f32, egui::Color32::BLACK))
        .fill(egui::Color32::WHITE)
        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
        .show(ui, |ui| {
            let out = egui::TextEdit::singleline(text)
                .id(id)
                .desired_width(desired_width)
                .frame(false)
                .text_color(egui::Color32::BLACK)
                .show(ui);
            edit_context_menu(ui, text, out.response, out.cursor_range, id, lang);
        });
}

/// Right-click menu (Cut / Copy / Paste / Select All) for a single-line text box.
/// `text` is mutated in place for cut/paste; the caret is repositioned via the
/// text-edit's stored `CCursorRange`.
fn edit_context_menu(
    _ui: &mut egui::Ui,
    text: &mut String,
    resp: egui::Response,
    cursor_range: Option<egui::text::CursorRange>,
    id: egui::Id,
    lang: &str,
) {
    resp.context_menu(|ui| {
        let (s, e) = cursor_char_range(text, cursor_range);
        if ui.button(i18n::t("cut", lang)).clicked() {
            if s < e {
                let (bs, be) = (char_to_byte(text, s), char_to_byte(text, e));
                let slice = text[bs..be].to_string();
                ui.ctx().output_mut(|o| o.copied_text = slice);
                text.replace_range(bs..be, "");
                set_ccursor(ui.ctx(), id, s, s);
            }
            ui.close_menu();
        }
        if ui.button(i18n::t("copy", lang)).clicked() {
            if s < e {
                let slice: String = text.chars().skip(s).take(e - s).collect();
                ui.ctx().output_mut(|o| o.copied_text = slice);
            }
            ui.close_menu();
        }
        if ui.button(i18n::t("paste", lang)).clicked() {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                if let Ok(pasted) = cb.get_text() {
                    let (bs, be) = (char_to_byte(text, s), char_to_byte(text, e));
                    text.replace_range(bs..be, &pasted);
                    let new_pos = s + pasted.chars().count();
                    set_ccursor(ui.ctx(), id, new_pos, new_pos);
                }
            }
            ui.close_menu();
        }
        if ui.button(i18n::t("select_all", lang)).clicked() {
            let n = text.chars().count();
            set_ccursor(ui.ctx(), id, 0, n);
            ui.close_menu();
        }
    });
}

/// Char-index (start, end) of the current selection, clamped to text length.
/// With no caret (e.g. never focused) we treat the whole buffer as selected so
/// copy/cut act on everything and paste appends at the end.
fn cursor_char_range(text: &str, cr: Option<egui::text::CursorRange>) -> (usize, usize) {
    match cr {
        Some(r) => {
            let a = r.primary.ccursor.index;
            let b = r.secondary.ccursor.index;
            let (mut s, mut e) = if a <= b { (a, b) } else { (b, a) };
            let n = text.chars().count();
            s = s.min(n);
            e = e.min(n);
            (s, e)
        }
        None => {
            let n = text.chars().count();
            (n, n)
        }
    }
}

/// Map a char index to a byte index for `String` slicing.
fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

/// Reposition the text-edit caret by setting its stored `CCursorRange`.
fn set_ccursor(ctx: &egui::Context, id: egui::Id, s: usize, e: usize) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let range = egui::text::CCursorRange {
            primary: egui::text::CCursor {
                index: s,
                prefer_next_row: true,
            },
            secondary: egui::text::CCursor {
                index: e,
                prefer_next_row: true,
            },
        };
        state.cursor.set_char_range(Some(range));
        state.store(ctx, id);
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    i18n::load();
    let (tx, rx) = std::sync::mpsc::channel::<UiMsg>();
    let engine = DownloadEngine::new(tx);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "HF Downloader",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(rx, engine)))
        }),
    )
}

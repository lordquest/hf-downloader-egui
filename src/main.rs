#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod download;
mod engine;
mod hf_api;
mod i18n;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;

use eframe::egui;

use crate::config::AppConfig;
use crate::download::{FileState, FileStatus};
use crate::engine::{DownloadEngine, UiMsg};
use crate::hf_api::{FileEntry, RepoInfo, TokenStatus};

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
        };
        app.check_token_async();
        app
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
        let endpoint = self.config.endpoint.clone();
        let lang = self.lang.clone();
        std::thread::spawn(move || {
            let rt = download::download_runtime();
            let status = rt.block_on(crate::hf_api::check_token(&endpoint, &lang));
            let _ = tx.send(UiMsg::TokenChecked(status));
        });
    }

    fn list_files_async(&mut self) {
        let input = self.repo_input.clone();
        if input.trim().is_empty() {
            self.status = StatusMsg::Tr("err_empty");
            return;
        }
        self.busy = true;
        self.status = StatusMsg::Tr("listing");
        let tx = self.engine.sender();
        let endpoint = self.config.endpoint.clone();
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
        let token = self.token_input.clone();
        if token.trim().is_empty() {
            return;
        }
        let tx = self.engine.sender();
        let endpoint = self.config.endpoint.clone();
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

        let target_dir = crate::hf_api::target_dir_for(&info.repo_id, &self.config.download_dir);
        let task_id = format!("task-{}", info.repo_id);
        let file_paths: Vec<(String, u64)> = self
            .file_entries
            .iter()
            .filter(|e| self.selected.contains(&e.path))
            .map(|e| (e.path.clone(), e.size))
            .collect();

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
            &self.config.endpoint,
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
                        self.status = StatusMsg::Tr("done");
                    }
                }
                UiMsg::RepoListed { info, entries } => {
                    self.repo_info = Some(info);
                    self.file_entries = entries;
                    self.selected = self
                        .file_entries
                        .iter()
                        .map(|e| e.path.clone())
                        .collect();
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

        egui::SidePanel::left("left")
            .resizable(true)
            .default_width(400.0)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.label(self.t("repo_placeholder"));
                    egui::Frame::none()
                        .stroke(egui::Stroke::new(1.5_f32, egui::Color32::BLACK))
                        .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.repo_input)
                                    .desired_width(f32::INFINITY)
                                    .frame(false),
                            );
                        });
                    ui.horizontal(|ui| {
                        if ui.button(self.t("list_files")).clicked() && !self.busy {
                            self.list_files_async();
                        }
                        if self.busy {
                            ui.spinner();
                        }
                    });

                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button(self.t("select_all")).clicked() {
                            self.selected =
                                self.file_entries.iter().map(|e| e.path.clone()).collect();
                        }
                        if ui.button(self.t("select_none")).clicked() {
                            self.selected.clear();
                        }
                        ui.label(format!(
                            "{}: {}/{}",
                            self.t("file_list"),
                            self.selected.len(),
                            self.file_entries.len()
                        ));
                    });

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for entry in &self.file_entries {
                            let mut sel = self.selected.contains(&entry.path);
                            if ui.checkbox(&mut sel, &entry.path).clicked() {
                                if sel {
                                    self.selected.insert(entry.path.clone());
                                } else {
                                    self.selected.remove(&entry.path);
                                }
                            }
                        }
                    });

                    ui.separator();
                    if let Some(info) = &self.repo_info {
                        let target =
                            crate::hf_api::target_dir_for(&info.repo_id, &self.config.download_dir);
                        ui.label(format!("{}: {}", self.t("save_to"), target));
                    }
                    ui.separator();
                    if ui.button(self.t("start")).clicked() {
                        self.start_download();
                    }
                    ui.label(self.status_text());
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(self.t("progress"));
            if self.file_states.is_empty() {
                ui.label(self.t("no_files"));
            } else {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (path, st) in self.file_states.iter() {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(path);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let active = st.status == FileStatus::Downloading
                                            || st.status == FileStatus::Pending;
                                        if active {
                                            if ui.button(self.t("cancel")).clicked() {
                                                if let Some(tid) = &self.active_task_id {
                                                    self.engine.cancel(tid);
                                                }
                                            }
                                        } else if st.status == FileStatus::Failed
                                            || st.status == FileStatus::Cancelled
                                        {
                                            if ui.button(self.t("retry")).clicked() {
                                                if let Some(tid) = &self.active_task_id {
                                                    self.engine.retry(tid, path, self.lang.clone());
                                                }
                                            }
                                        }
                                        let (txt, color) = self.status_pair(st);
                                        ui.colored_label(color, txt);
                                        // Reserve a fixed width so the speed text changing
                                        // length doesn't shift the status/buttons around.
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(86.0, 18.0),
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                                ui.label(fmt_speed(st.speed));
                                            },
                                        );
                                    },
                                );
                            });

                            let frac = if st.total > 0 {
                                (st.downloaded as f32 / st.total as f32).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            let bar_text = format!(
                                "{} / {}  ({:.1}%)",
                                fmt_size(st.downloaded),
                                fmt_size(st.total),
                                frac * 100.0
                            );
                            ui.add(egui::ProgressBar::new(frac).text(bar_text));
                            if let Some(err) = &st.error {
                                ui.colored_label(egui::Color32::RED, err);
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
                        ui.text_edit_singleline(&mut self.config.download_dir);
                        if ui.button(self.t("browse")).clicked() {
                            if let Some(f) = rfd::FileDialog::new().pick_folder() {
                                self.config.download_dir = f.to_string_lossy().to_string();
                                self.config.download_dir_set = true;
                            }
                        }
                    });
                    ui.label(self.t("endpoint"));
                    ui.text_edit_singleline(&mut self.config.endpoint);
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button(self.t("save")).clicked() {
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
                    ui.text_edit_singleline(&mut self.token_input);
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

/// Default UI language: follow the OS UI language (zh* -> Chinese, anything else ->
/// English). Only used when the user hasn't explicitly chosen & saved a language.
fn detect_system_lang() -> String {
    match sys_locale::get_locale() {
        Some(l) if l.to_lowercase().starts_with("zh") => "zh".to_string(),
        _ => "en".to_string(),
    }
}

/// Load a system CJK font so Chinese text renders (egui's bundled font is Latin-only).
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/msyh.ttf",
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/simsun.ttc",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
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

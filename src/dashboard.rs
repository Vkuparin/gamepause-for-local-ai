//! Rust-native dashboard. Engine work stays on the existing worker channel.
use crate::dashboard_theme::{self as design, Checkbox, CheckboxUi, Icon, Palette};
use crate::{
    app::{Action, Shared, SharedState},
    commands::Outcome,
    config::{Config, ExtraGame},
    control::CoreCommand,
    discovery::canonical,
    tray, wide,
};
use eframe::egui::{self, *};
use egui_extras::{Column, TableBuilder};
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    time::{Duration, Instant},
};
use std::{path::PathBuf, ptr::null_mut};
use windows_sys::Win32::{Foundation::HWND, UI::Controls::Dialogs::*};
use winit::platform::windows::EventLoopBuilderExtWindows;

static RUNNING_REQUESTED: AtomicBool = AtomicBool::new(false);
static UI_VISIBLE: AtomicBool = AtomicBool::new(false);
static UI_STOP: AtomicBool = AtomicBool::new(false);
static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);
struct Bridge {
    tx: Sender<UiRequest>,
    ctx: Option<Context>,
    shared: SharedState,
    fingerprint: String,
    thread: Option<std::thread::JoinHandle<()>>,
}
enum UiRequest {
    Show,
    Resume,
    Verify,
    Theme,
    Stop,
}

pub fn needs_running_apps() -> bool {
    RUNNING_REQUESTED.load(Ordering::Relaxed)
}
pub fn scale(value: i32, dpi: i32) -> i32 {
    value * dpi / 96
}
pub fn is_dialog_message(_: &windows_sys::Win32::UI::WindowsAndMessaging::MSG) -> bool {
    false
}
pub fn theme_changed() {
    dispatch(UiRequest::Theme);
}
fn dispatch(request: UiRequest) {
    let ctx = BRIDGE
        .lock()
        .ok()
        .and_then(|bridge| {
            bridge.as_ref().map(|b| {
                let _ = b.tx.send(request);
                b.ctx.clone()
            })
        })
        .flatten();
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}
pub fn close() {
    UI_STOP.store(true, Ordering::Relaxed);
    let bridge = BRIDGE.lock().ok().and_then(|mut b| b.take());
    if let Some(mut bridge) = bridge {
        let _ = bridge.tx.send(UiRequest::Stop);
        if let Some(ctx) = bridge.ctx {
            ctx.request_repaint();
        }
        if let Some(thread) = bridge.thread.take() {
            let _ = thread.join();
        }
    }
    RUNNING_REQUESTED.store(false, Ordering::Relaxed);
    UI_VISIBLE.store(false, Ordering::Relaxed);
}
/// Called by the existing tray timer. Unchanged state does not redraw the GPU window.
pub fn refresh() {
    if !UI_VISIBLE.load(Ordering::Relaxed) {
        return;
    }
    let shared = BRIDGE
        .lock()
        .ok()
        .and_then(|b| b.as_ref().map(|b| b.shared.clone()));
    let Some(shared) = shared else { return };
    let Ok(s) = shared.lock().map(|s| s.clone()) else {
        return;
    };
    let summary = crate::presentation::summarize(&s);
    let fingerprint = format!(
        "{}|{}|{}|{:?}|{:?}|{}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}|{}|{:?}",
        summary.games,
        summary.ai_text(),
        s.revision,
        s.commands.latest,
        s.running_apps,
        s.settings_error,
        s.restore_offer,
        s.verify_report,
        s.doctor_report,
        s.provider_statuses,
        s.commands.settings_pending,
        s.doctor_pending,
        s.verifying,
        s.discovery_ready,
        s.games
    );
    let ctx = BRIDGE.lock().ok().and_then(|mut b| {
        b.as_mut().and_then(|b| {
            if b.fingerprint == fingerprint {
                return None;
            }
            b.fingerprint = fingerprint;
            b.ctx.clone()
        })
    });
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}
pub fn show(shared: SharedState, tx: Sender<Action>, folder: PathBuf) {
    if BRIDGE.lock().is_ok_and(|b| {
        b.as_ref()
            .is_some_and(|b| b.thread.as_ref().is_some_and(|t| t.is_finished()))
    }) {
        close();
    }
    if BRIDGE.lock().is_ok_and(|b| b.is_some()) {
        dispatch(UiRequest::Show);
        return;
    }
    let (ui_tx, rx) = mpsc::channel();
    UI_STOP.store(false, Ordering::Relaxed);
    let rx = Arc::new(Mutex::new(rx));
    let Ok(mut bridge) = BRIDGE.lock() else {
        return;
    };
    *bridge = Some(Bridge {
        tx: ui_tx,
        ctx: None,
        shared: shared.clone(),
        fingerprint: String::new(),
        thread: None,
    });
    let thread = std::thread::Builder::new()
        .name("gamepause-ui".into())
        .spawn(move || {
            let mut initial = Some(UiRequest::Show);
            loop {
                let request = initial
                    .take()
                    .or_else(|| rx.lock().ok().and_then(|rx| rx.recv().ok()));
                let Some(request) = request else { break };
                if matches!(request, UiRequest::Stop) {
                    break;
                }
                if matches!(request, UiRequest::Theme) {
                    continue;
                }
                let error_state = shared.clone();
                let app_shared = shared.clone();
                let app_tx = tx.clone();
                let app_folder = folder.clone();
                let app_rx = rx.clone();
                let result = eframe::run_native(
                    "GamePause for LM Studio",
                    native_options(),
                    Box::new(move |cc| {
                        design::fonts(&cc.egui_ctx);
                        if let Ok(mut b) = BRIDGE.lock()
                            && let Some(b) = b.as_mut()
                        {
                            b.ctx = Some(cc.egui_ctx.clone());
                        }
                        let mut app = Dashboard::new(app_shared, app_tx, app_folder, app_rx);
                        let s = app.shared.lock().map(|s| s.clone()).unwrap_or_default();
                        match request {
                            UiRequest::Resume => app.resume(&s),
                            UiRequest::Verify if crate::ui_commands::verify_available(&s) => {
                                app.modal = Some(Modal::Verify)
                            }
                            _ => (),
                        }
                        Ok(Box::new(app))
                    }),
                );
                if let Ok(mut b) = BRIDGE.lock()
                    && let Some(b) = b.as_mut()
                {
                    b.ctx = None;
                }
                UI_VISIBLE.store(false, Ordering::Relaxed);
                RUNNING_REQUESTED.store(false, Ordering::Relaxed);
                if let Err(error) = result {
                    crate::app::local_result(
                        &error_state,
                        Outcome::Failed,
                        format!("Could not open dashboard: {error}"),
                    );
                    crate::app::log(&folder, &format!("Dashboard renderer failed: {error}"));
                }
                if UI_STOP.load(Ordering::Relaxed) {
                    break;
                }
                // The same thread reuses eframe's thread-local Windows event loop.
                // A closed dashboard has no window or renderer and blocks on the mailbox.
            }
        });
    match thread {
        Ok(thread) => {
            if let Some(b) = bridge.as_mut() {
                b.thread = Some(thread);
            }
        }
        Err(error) => {
            *bridge = None;
            drop(bridge);
            tray::error(&format!("Could not start dashboard: {error}"));
        }
    }
}
fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([1120.0, 920.0])
            .with_min_inner_size([620.0, 580.0])
            .with_icon(app_icon()),
        renderer: eframe::Renderer::Glow,
        event_loop_builder: Some(Box::new(|builder| {
            builder.with_any_thread(true);
        })),
        ..Default::default()
    }
}
fn app_icon() -> IconData {
    let mut rgba = vec![0; 32 * 32 * 4];
    for y in 4..28 {
        for x in (7..13).chain(19..25) {
            let i = (y * 32 + x) * 4;
            rgba[i..i + 4].copy_from_slice(&[255, 133, 48, 255]);
        }
    }
    IconData {
        rgba,
        width: 32,
        height: 32,
    }
}
/// The native tray routes gameplay confirmation into the same themed modal.
pub fn request_resume(shared: SharedState, tx: Sender<Action>) {
    let folder = crate::config::data_directory();
    show(shared, tx, folder);
    dispatch(UiRequest::Resume);
}
pub fn request_verify_modal(shared: SharedState, tx: Sender<Action>, folder: PathBuf) {
    show(shared, tx, folder);
    dispatch(UiRequest::Verify);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Games,
    Running,
    Ignored,
    Activity,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    General,
    Detection,
    LMStudio,
    Ollama,
    Recovery,
    Diagnostics,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    name: String,
    path: String,
    platform: String,
    custom: bool,
    ignored: bool,
    running: bool,
    pid: Option<u32>,
}
fn ignored(config: &Config, path: &str) -> bool {
    config
        .ignored_games
        .iter()
        .chain(&config.excluded_paths)
        .any(|p| canonical(p) == canonical(path))
}
fn rows(shared: &Shared, page: Page, query: &str) -> Vec<Row> {
    let status = |path: &str| {
        shared.active_games.iter().find(|g| {
            canonical(&g.path) == canonical(path) || canonical(&g.executable) == canonical(path)
        })
    };
    let mut rows: Vec<Row> = match page {
        Page::Games => shared
            .games
            .iter()
            .map(|g| Row {
                name: g.name.clone(),
                path: g.path.clone(),
                platform: g.launcher.clone(),
                custom: g.launcher == "Custom",
                ignored: ignored(&shared.config, &g.path),
                running: status(&g.path).is_some(),
                pid: status(&g.path).map(|g| g.pid),
            })
            .collect(),
        Page::Running => shared
            .running_apps
            .iter()
            .map(|a| {
                let recognized = shared
                    .games
                    .iter()
                    .find(|g| crate::discovery::inside(&a.path, &g.path));
                Row {
                    name: a.name.clone(),
                    path: a.path.clone(),
                    platform: recognized
                        .map_or("Unrecognized", |g| g.launcher.as_str())
                        .into(),
                    custom: false,
                    ignored: ignored(&shared.config, &a.path),
                    running: true,
                    pid: status(&a.path).map(|g| g.pid),
                }
            })
            .collect(),
        Page::Ignored => shared
            .config
            .ignored_games
            .iter()
            .chain(&shared.config.excluded_paths)
            .map(|path| {
                let game = shared
                    .games
                    .iter()
                    .find(|g| canonical(&g.path) == canonical(path));
                Row {
                    name: game.map_or_else(
                        || path.rsplit(['\\', '/']).next().unwrap_or(path).into(),
                        |g| g.name.clone(),
                    ),
                    path: path.clone(),
                    platform: game
                        .map_or("Custom exclusion", |g| g.launcher.as_str())
                        .into(),
                    custom: game.is_some_and(|g| g.launcher == "Custom"),
                    ignored: true,
                    running: status(path).is_some(),
                    pid: status(path).map(|g| g.pid),
                }
            })
            .collect(),
        Page::Activity => vec![],
    };
    let query = query.to_lowercase();
    let mut seen = HashSet::new();
    rows.retain(|r| {
        (format!("{} {} {}", r.name, r.path, r.platform)
            .to_lowercase()
            .contains(&query))
            && seen.insert(canonical(&r.path))
    });
    rows.sort_by_cached_key(|r| r.name.to_lowercase());
    rows
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Neutral,
    Paused,
    Success,
    Error,
}
struct Hero {
    title: &'static str,
    reason: String,
    hint: &'static str,
    tone: Tone,
    paused: bool,
}
fn hero(s: &Shared) -> Hero {
    use crate::control::Activity::*;
    let activity = if s.verifying { Verifying } else { s.activity };
    let (title, hint, tone, paused) = match activity {
        Unknown => (
            "CHECKING GAMES",
            "Waiting for reliable detection before controlling AI.",
            Tone::Neutral,
            false,
        ),
        Watching => (
            if s.config.automation_enabled {
                "READY FOR GAMING"
            } else {
                "AUTO PAUSE OFF"
            },
            "AI is unchanged. Current model residency is not polled.",
            Tone::Neutral,
            false,
        ),
        Observation => (
            "OBSERVATION MODE",
            "Detection only. AI and saved recovery are unchanged.",
            Tone::Neutral,
            false,
        ),
        Unavailable => (
            "AI UNAVAILABLE",
            "Open your AI provider or check its connection in Advanced.",
            Tone::Error,
            false,
        ),
        DetectionUnavailable => (
            "DETECTION UNAVAILABLE",
            "AI control is held until game detection succeeds.",
            Tone::Error,
            false,
        ),
        Capturing => (
            "PAUSING AI",
            "Saving original model settings before unloading.",
            Tone::Paused,
            false,
        ),
        WaitingForInference => (
            "WAITING TO PAUSE",
            "Waiting for inference to finish. Pause is not complete.",
            Tone::Paused,
            false,
        ),
        Unloading => (
            "UNLOADING AI",
            "Removing captured models. Waiting for verification.",
            Tone::Paused,
            false,
        ),
        Paused => (
            "AI PAUSED",
            "AI paused for gaming. Resume to restore captured models.",
            Tone::Paused,
            true,
        ),
        ManualHold => (
            "AI PAUSED",
            "You paused AI manually. Resume to release the hold.",
            Tone::Paused,
            true,
        ),
        Countdown => (
            "RESUME SCHEDULED",
            "Waiting for the configured delay before restoring AI.",
            Tone::Paused,
            true,
        ),
        Restoring => (
            "RESTORING AI",
            "Reloading captured models and verifying restoration.",
            Tone::Paused,
            false,
        ),
        Recovery => (
            "RECOVERY PENDING",
            "Saved recovery is retained. Resume to retry when available.",
            Tone::Error,
            false,
        ),
        PartialFailure => (
            "AI NEEDS ATTENTION",
            "An operation failed or is incomplete. Open Activity for details.",
            Tone::Error,
            false,
        ),
        Verifying => (
            "TESTING RECOVERY",
            "Testing capture, unload and restoration. Please wait.",
            Tone::Paused,
            false,
        ),
        Coexistence => (
            "AI RESUMED",
            "Restoration verified during gameplay. Pause AI to end approval.",
            Tone::Success,
            false,
        ),
    };
    let reliable =
        s.discovery_ready && s.detection_ok && !s.disabled && s.discovery_errors.is_empty();
    let reason = if !reliable {
        "Running games are not yet confirmed".into()
    } else if let Some(first) = s.active_games.first() {
        if s.active_games.len() == 1 {
            format!("{} is running", first.game)
        } else {
            format!(
                "{} and {} other games are running",
                first.game,
                s.active_games.len() - 1
            )
        }
    } else if s.manual_pause {
        "Manual pause is in effect".into()
    } else {
        "No recognized game is running".into()
    };
    Hero {
        title,
        reason,
        hint,
        tone,
        paused,
    }
}
fn provider_status(s: &Shared) -> Vec<(String, Tone)> {
    use crate::coordinator::State;
    s.config
        .providers
        .iter()
        .filter(|p| p.enabled())
        .map(|p| {
            let report = s
                .provider_statuses
                .iter()
                .find(|r| r.id == p.id() && r.kind == p.kind());
            let (text, tone) = match report.map(|r| r.state) {
                Some(State::Paused) => ("Captured-model unload verified", Tone::Success),
                Some(State::Restored) => (
                    "Restoration verified; current residency unpolled",
                    Tone::Success,
                ),
                Some(State::Pausing) => ("Unloading captured models", Tone::Paused),
                Some(State::Restoring) => ("Restoring captured models", Tone::Paused),
                Some(State::Failed) => ("Operation failed; check Activity", Tone::Error),
                Some(State::Deferred) => ("Waiting; operation incomplete", Tone::Paused),
                _ if s.activity == crate::control::Activity::Unavailable => {
                    ("Unavailable or connection refused", Tone::Error)
                }
                _ => ("Current model state unpolled", Tone::Neutral),
            };
            (
                format!(
                    "{}: {text}{}",
                    p.kind().name(),
                    if p.kind() == crate::provider::Kind::Ollama {
                        " (experimental)"
                    } else {
                        ""
                    }
                ),
                tone,
            )
        })
        .collect()
}

struct ActivityLog {
    entries: VecDeque<String>,
    last: String,
}
impl ActivityLog {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            last: String::new(),
        }
    }
    fn record(&mut self, detail: String) {
        if self.last == detail {
            return;
        }
        self.last = detail.clone();
        let mut local: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe {
            windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut local);
        }
        let detail = detail.chars().take(8000).collect::<String>();
        self.entries.push_front(format!(
            "{:02}:{:02}:{:02}  {}",
            local.wHour, local.wMinute, local.wSecond, detail
        ));
        self.entries.truncate(160);
    }
}
struct Toast {
    key: Option<(u64, Outcome, String)>,
    since: Instant,
}
impl Toast {
    fn new() -> Self {
        Self {
            key: None,
            since: Instant::now(),
        }
    }
    fn message<'a>(&mut self, s: &'a Shared, now: Instant) -> Option<(&'a str, bool)> {
        if !s.settings_error.is_empty() {
            return Some((&s.settings_error, true));
        }
        let result = s.commands.latest.as_ref()?;
        let key = (result.id, result.outcome, result.message.clone());
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.since = now;
        }
        let persistent = matches!(
            result.outcome,
            Outcome::Failed | Outcome::Requested | Outcome::Working
        );
        (persistent || now.duration_since(self.since) < Duration::from_secs(5))
            .then_some((&result.message, result.outcome == Outcome::Failed))
    }
}

enum Modal {
    Add {
        name: String,
        path: String,
        auto: bool,
    },
    Rename(Row, String),
    Remove(Row),
    Resume(crate::gameplay::RestoreOffer, Vec<bool>),
    Verify,
    Ollama(crate::config::Provider),
    Help,
}
struct Dashboard {
    shared: SharedState,
    tx: Sender<Action>,
    folder: PathBuf,
    rx: Arc<Mutex<mpsc::Receiver<UiRequest>>>,
    page: Page,
    settings_page: SettingsPage,
    query: String,
    selected: Option<String>,
    modal: Option<Modal>,
    modal_active: bool,
    owner: HWND,
    edit_config: Config,
    edit_revision: u64,
    dirty: bool,
    validation: String,
    log: ActivityLog,
    toast: Toast,
    visible: bool,
    stopping: bool,
    theme: Option<(bool, bool)>,
    worker_log: Option<String>,
}
impl Dashboard {
    fn new(
        shared: SharedState,
        tx: Sender<Action>,
        folder: PathBuf,
        rx: Arc<Mutex<mpsc::Receiver<UiRequest>>>,
    ) -> Self {
        let s = shared.lock().map(|s| s.clone()).unwrap_or_default();
        Self {
            shared,
            tx,
            folder,
            rx,
            page: Page::Games,
            settings_page: SettingsPage::General,
            query: String::new(),
            selected: None,
            modal: None,
            modal_active: false,
            owner: null_mut(),
            edit_config: s.config,
            edit_revision: s.revision,
            dirty: false,
            validation: String::new(),
            log: ActivityLog::new(),
            toast: Toast::new(),
            visible: true,
            stopping: false,
            theme: None,
            worker_log: None,
        }
    }
    fn action(&self, action: Action, label: &str) {
        crate::app::request_action(&self.shared, &self.tx, action, label);
    }
    fn save(&mut self, config: Config, advanced: bool) {
        match config.validate() {
            Ok(()) => {
                self.validation.clear();
                self.action(
                    if advanced {
                        Action::AdvancedSettings(Box::new(config))
                    } else {
                        Action::Settings(Box::new(config))
                    },
                    "Save settings",
                );
            }
            Err(e) => self.validation = format!("Check these settings: {e:#}"),
        }
    }
    fn resume(&mut self, s: &Shared) {
        let a = s.controls().availability();
        if !s.pending && a.resume {
            crate::app::request_core(&self.shared, &self.tx, CoreCommand::Resume);
        } else if !a.restore {
            crate::app::local_result(&self.shared, Outcome::Failed, a.reason);
        } else if s.coexistence {
            self.action(Action::RetryGameplayRestore, "Resume AI retry");
        } else if s.active_games.is_empty() {
            crate::app::request_core(&self.shared, &self.tx, CoreCommand::Restore);
        } else if let Some(offer) = s.restore_offer.clone() {
            let count = offer.games.len();
            self.modal = Some(Modal::Resume(offer, vec![false; count]));
        }
    }
    fn set_page(&mut self, page: Page) {
        self.page = page;
        self.query.clear();
        self.selected = None;
        RUNNING_REQUESTED.store(self.visible && page == Page::Running, Ordering::Relaxed);
    }
    fn tone(p: Palette, t: Tone) -> Color32 {
        match t {
            Tone::Neutral => p.muted,
            Tone::Paused => p.accent,
            Tone::Success => p.success,
            Tone::Error => p.error,
        }
    }
    fn hero(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        let h = hero(s);
        let accent = Self::tone(p, h.tone);
        p.card().inner_margin(18).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.set_min_width(ui.available_width());
            let narrow = ui.available_width() < 780.0;
            ui.horizontal(|ui| {
                if ui.available_width() > 650.0 {
                    let (rect, _) = ui.allocate_exact_size(vec2(108.0, 108.0), Sense::hover());
                    ui.painter()
                        .circle_stroke(rect.center(), 47.0, Stroke::new(5.0_f32, accent));
                    design::icon(
                        ui.painter(),
                        rect.shrink(23.0),
                        if h.paused { Icon::Pause } else { Icon::Play },
                        accent,
                    );
                    ui.add_space(10.0);
                }
                let right = if narrow { 0.0 } else { 190.0 };
                let width = (ui.available_width() - right - design::GAP).max(220.0);
                ui.allocate_ui_with_layout(
                    vec2(width, 110.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_min_width(width);
                        ui.label(
                            RichText::new(h.title)
                                .size(design::HERO_FONT)
                                .family(FontFamily::Name("heading".into()))
                                .color(accent),
                        );
                        ui.add(Label::new(RichText::new(&h.reason).size(23.0)).truncate())
                            .on_hover_text(&h.reason);
                        for (text, tone) in provider_status(s) {
                            ui.horizontal(|ui| {
                                let (dot, _) =
                                    ui.allocate_exact_size(vec2(12.0, 18.0), Sense::hover());
                                ui.painter()
                                    .circle_filled(dot.center(), 4.0, Self::tone(p, tone));
                                ui.add(Label::new(text).truncate());
                            });
                        }
                        if !s.config.any_provider_enabled() {
                            ui.colored_label(p.muted, "No AI provider is enabled");
                        }
                        ui.add(Label::new(RichText::new(h.hint).size(15.0).color(p.muted)).wrap());
                    },
                );
                if !narrow {
                    self.primary(ui, s, p);
                }
            });
            if narrow {
                ui.add_space(10.0);
                self.primary(ui, s, p);
            }
        });
    }
    fn primary(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        let a = s.controls().availability();
        let resume = s.pending || s.manual_pause;
        let enabled = if resume {
            a.restore || a.resume
        } else {
            a.pause
        };
        ui.add_enabled_ui(enabled, |ui| {
            let label = if resume {
                crate::restore_dialog::resume_label(
                    !s.active_games.is_empty(),
                    s.coexistence,
                    s.pending,
                )
            } else {
                "Pause AI"
            };
            let response = ui.add(
                Button::new(
                    RichText::new(format!("     {label}"))
                        .color(if crate::theme::high_contrast() {
                            p.selected_text
                        } else {
                            p.text
                        })
                        .size(20.0)
                        .family(FontFamily::Name("heading".into())),
                )
                .min_size(design::HERO_ACTION)
                .fill(if ui.visuals().dark_mode {
                    design::PRIMARY_FILL
                } else {
                    p.selected
                })
                .stroke(Stroke::new(1.0_f32, p.accent)),
            );
            design::icon(
                ui.painter(),
                Rect::from_center_size(
                    pos2(response.rect.left() + 24.0, response.rect.center().y),
                    vec2(23.0, 23.0),
                ),
                if resume { Icon::Play } else { Icon::Pause },
                p.text,
            );
            if response.on_hover_text(a.reason).clicked() {
                if resume {
                    self.resume(s);
                } else {
                    crate::app::request_core(&self.shared, &self.tx, CoreCommand::Pause);
                }
            }
        });
    }
    fn settings_strip(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        p.card().inner_margin(12).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                design::icon(ui.painter(), rect, Icon::Settings, p.muted);
                ui.strong("Settings");
                ui.separator();
                let mut auto = s.config.automation_enabled;
                if ui
                    .add_enabled(
                        !s.commands.settings_pending,
                        Checkbox::new(&mut auto, "Automatically pause AI while gaming"),
                    )
                    .changed()
                {
                    let mut c = s.config.clone();
                    c.automation_enabled = auto;
                    self.save(c, false);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !s.commands.settings_pending,
                            Button::new(if s.config.advanced_settings_visible {
                                "Back to games"
                            } else {
                                "Advanced  >"
                            }),
                        )
                        .clicked()
                    {
                        self.action(
                            Action::AdvancedVisibility(!s.config.advanced_settings_visible),
                            "Advanced visibility",
                        );
                    }
                });
            });
        });
    }
    fn navigation(&mut self, ui: &mut Ui, p: Palette) {
        ui.horizontal(|ui| {
            for (page, label, icon) in [
                (Page::Games, "Games", Icon::Game),
                (Page::Running, "Running apps", Icon::Apps),
                (Page::Ignored, "Ignored", Icon::Ignore),
            ] {
                if design::button(ui, label, icon, self.page == page, p).clicked() {
                    self.set_page(page);
                }
            }
        });
    }
    fn games(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        self.navigation(ui, p);
        ui.add_space(6.0);
        p.card().inner_margin(12).show(ui,|ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect,_)=ui.allocate_exact_size(vec2(20.0,20.0),Sense::hover());design::icon(ui.painter(),rect,Icon::Search,p.muted);
                let width=(ui.available_width()-260.0).max(130.0);
                ui.add_sized([width,design::CONTROL],TextEdit::singleline(&mut self.query).hint_text(match self.page {Page::Games=>"Search games...",Page::Running=>"Search applications...",_=>"Search ignored..."}));
                if design::button(ui,"Refresh",Icon::Apps,false,p).clicked(){self.action(Action::Refresh,"Discovery refresh");}
                if design::button(ui,"Add game...",Icon::Add,true,p).clicked(){self.modal=Some(Modal::Add{name:String::new(),path:String::new(),auto:true});}
            });
            ui.add_space(8.0);
            let entries=rows(s,self.page,&self.query);
            let height=(ui.available_height()-162.0).clamp(140.0,470.0);
            let table_top=ui.cursor().top();
            let table_right=ui.max_rect().right();
            let row_painter=ui.painter().with_clip_rect(Rect::from_min_max(pos2(ui.max_rect().left(),table_top+34.0),pos2(table_right,table_top+34.0+height)));
            let original_spacing=ui.spacing().item_spacing.y;
            ui.spacing_mut().item_spacing.y=0.0;
            let wide=ui.available_width()>740.0;
            let page=self.page;
            let mut selected=self.selected.clone();
            let mut toggled=None;
            let mut table=TableBuilder::new(ui).id_salt(("game-table",format!("{page:?}"))).striped(true).resizable(true).sense(Sense::click()).cell_layout(Layout::left_to_right(Align::Center)).min_scrolled_height(height.min(entries.len() as f32*design::ROW).max(96.0)).max_scroll_height(height)
                .column(Column::remainder().at_least(180.0).resizable(false).clip(true));
            if wide {table=table.column(Column::initial(145.0).at_least(100.0));}
            table=table.column(Column::initial(if page==Page::Running {90.0}else{110.0}).at_least(80.0)).column(Column::initial(160.0).at_least(110.0));
            table.header(34.0,|mut header| {
                header.col(|ui| {ui.strong(if page==Page::Running {"Application"}else{"Game"});});
                if wide {header.col(|ui|{ui.strong(if page==Page::Running {"Recognition"}else{"Platform"});});}
                header.col(|ui|{ui.strong(if page==Page::Running {"PID"}else{"Auto pause"});});
                header.col(|ui|{ui.strong(if page==Page::Ignored {"Source"}else{"Status"});});
            }).body(|body| {
                body.rows(design::ROW,entries.len(),|mut row| {
                    let index=row.index();
                    let entry=&entries[index];
                    row.set_selected(selected.as_ref().is_some_and(|path|canonical(path)==canonical(&entry.path)));
                    row.col(|ui| {
                        let (rect,_)=ui.allocate_exact_size(vec2(30.0,30.0),Sense::hover());
                        ui.painter().rect_filled(rect,5,p.elevated);design::icon(ui.painter(),rect.shrink(4.0),if page==Page::Running {Icon::Apps}else{Icon::Game},p.accent);
                        ui.add(Label::new(&entry.name).truncate()).on_hover_text(format!("{}\n{}",entry.name,entry.path));
                    });
                    if wide {row.col(|ui| {ui.add(Label::new(RichText::new(&entry.platform).color(p.muted)).truncate());});}
                    row.col(|ui| {
                        if page==Page::Running {ui.label(entry.pid.map_or_else(||"—".into(),|pid|pid.to_string())).on_hover_text("PID is available for recognized games. Other process IDs are not collected by this inventory.");}
                        else {let mut on=!entry.ignored;if ui.add_enabled(!s.commands.settings_pending,Checkbox::new(&mut on,if entry.ignored {"Off"}else{"On"})).changed(){toggled=Some((entry.path.clone(),on));}}
                    });
                    row.col(|ui| {
                        if page==Page::Ignored {ui.add(Label::new("Saved exclusion").truncate());}
                        else {let reliable=s.discovery_ready&&s.detection_ok&&s.discovery_errors.is_empty();let (label,color)=if !reliable {("Unknown",p.muted)} else if entry.running {("Running",p.success)}else{("Not running",p.muted)};
                            let (dot,_)=ui.allocate_exact_size(vec2(10.0,16.0),Sense::hover());ui.painter().circle_filled(dot.center(),4.0,color);ui.colored_label(color,label);}
                    });
                    if row.response().clicked() {selected=Some(entry.path.clone());}
                    let response=row.response();
                    let selected_row=selected.as_ref().is_some_and(|path|canonical(path)==canonical(&entry.path));
                    if selected_row {let mut rect=response.rect.shrink(1.0);rect.max.x=rect.max.x.min(table_right-1.0);row_painter.rect_stroke(rect,4,Stroke::new(1.0_f32,p.accent),StrokeKind::Inside);}
                    if response.has_focus() {
                        let step=response.ctx.input(|i|if i.key_pressed(Key::ArrowDown){1}else if i.key_pressed(Key::ArrowUp){-1}else{0});
                        if step!=0 {let current=selected.as_ref().and_then(|path|entries.iter().position(|r|canonical(&r.path)==canonical(path))).unwrap_or(index);let next=(current as isize+step).clamp(0,entries.len().saturating_sub(1) as isize) as usize;selected=Some(entries[next].path.clone());}
                        if ui_input_arrow(&response) {selected=Some(entry.path.clone());}
                    }
                });
            });
            ui.spacing_mut().item_spacing.y=original_spacing;
            self.selected=selected;
            if let Some((path,on))=toggled {let mut c=s.config.clone();set_ignored(&mut c,&path,!on);self.save(c,false);}
            if entries.is_empty() {
                ui.colored_label(p.muted,if !self.query.is_empty(){"No matching entries. Try a different search."}else{match page {Page::Games=>"No games found. Refresh discovery or add a game executable.",Page::Running=>"No relevant applications are running. This inventory refreshes while the tab is open.",_=>"No ignored applications. Games you ignore will appear here."}});
            }
            ui.add_space(10.0);
            let entry=self.selected.as_ref().and_then(|path|entries.iter().find(|r|canonical(&r.path)==canonical(path)));
            self.details(ui,s,entry,p);
        });
    }
    fn details(&mut self, ui: &mut Ui, s: &Shared, row: Option<&Row>, p: Palette) {
        p.card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if let Some(row) = row {
                let wide = ui.available_width() > 740.0;
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
                    ui.painter().rect_filled(rect, 7, p.elevated);
                    design::icon(ui.painter(), rect.shrink(14.0), Icon::Game, p.accent);
                    let text_width =
                        (ui.available_width() - if wide { 220.0 } else { 0.0 }).max(180.0);
                    ui.allocate_ui_with_layout(
                        vec2(text_width, 80.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_min_width(text_width);
                            ui.add(
                                Label::new(
                                    RichText::new(&row.name)
                                        .size(21.0)
                                        .family(FontFamily::Name("heading".into())),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&row.name);
                            ui.add(Label::new(RichText::new(&row.path).color(p.muted)).truncate())
                                .on_hover_text(&row.path);
                            ui.colored_label(p.muted, format!("Recognized from {}", row.platform));
                        },
                    );
                    if wide {
                        self.row_actions(ui, s, row, p);
                    }
                });
                if !wide {
                    self.row_actions(ui, s, row, p);
                }
            } else {
                ui.colored_label(
                    p.muted,
                    "Select an entry to see its path, source and available actions.",
                );
            }
        });
    }
    fn row_actions(&mut self, ui: &mut Ui, s: &Shared, row: &Row, p: Palette) {
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!s.commands.settings_pending, |ui| {
                if design::button(
                    ui,
                    if self.page == Page::Running {
                        "Add as game"
                    } else if row.ignored {
                        "Unignore"
                    } else {
                        "Ignore"
                    },
                    if self.page == Page::Running {
                        Icon::Add
                    } else {
                        Icon::Ignore
                    },
                    false,
                    p,
                )
                .clicked()
                {
                    if self.page == Page::Running {
                        self.modal = Some(Modal::Add {
                            name: row.name.trim_end_matches(".exe").into(),
                            path: row.path.clone(),
                            auto: true,
                        });
                    } else {
                        let mut c = s.config.clone();
                        set_ignored(&mut c, &row.path, !row.ignored);
                        self.save(c, false);
                    }
                }
                ui.menu_button("More...", |ui| {
                    if self.page == Page::Running && ui.button("Ignore executable").clicked() {
                        let mut c = s.config.clone();
                        set_ignored(&mut c, &row.path, true);
                        self.save(c, false);
                        ui.close();
                    }
                    if row.custom {
                        if ui.button("Rename...").clicked() {
                            self.modal = Some(Modal::Rename(row.clone(), row.name.clone()));
                            ui.close();
                        }
                        if ui.button("Remove...").clicked() {
                            self.modal = Some(Modal::Remove(row.clone()));
                            ui.close();
                        }
                    }
                    if ui.button("Copy path").clicked() {
                        ui.ctx().copy_text(row.path.clone());
                        ui.close();
                    }
                });
            });
        });
    }
}

fn ui_input_arrow(response: &Response) -> bool {
    response
        .ctx
        .input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space))
}
fn set_ignored(config: &mut Config, path: &str, off: bool) {
    config
        .ignored_games
        .retain(|p| canonical(p) != canonical(path));
    config
        .excluded_paths
        .retain(|p| canonical(p) != canonical(path));
    if off {
        config.ignored_games.push(path.into());
    }
}
fn confirmed_resume(
    s: &Shared,
    offer: &crate::gameplay::RestoreOffer,
    checked: &[bool],
) -> Result<Action, String> {
    if !s.controls().availability().restore
        || s.commands.settings_pending
        || checked.len() != offer.games.len()
        || s.restore_offer
            .as_ref()
            .is_none_or(|current| current.id != offer.id)
    {
        return Err(
            "Running games or control availability changed. Cancel and request Resume again."
                .into(),
        );
    }
    let mut seen = HashSet::new();
    let ignored = offer
        .games
        .iter()
        .zip(checked)
        .filter(|(_, on)| **on)
        .map(|(game, _)| game.executable.clone())
        .filter(|path| seen.insert(canonical(path)))
        .collect();
    Ok(Action::ConfirmedRestore {
        offer_id: offer.id,
        ignored,
    })
}

impl Dashboard {
    fn advanced(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        ui.heading("Advanced settings");
        ui.colored_label(
            p.muted,
            "Settings are saved atomically. Recovery keeps its original provider routes.",
        );
        ui.add_space(design::GAP);
        p.card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                for (page, label) in [
                    (SettingsPage::General, "General"),
                    (SettingsPage::Detection, "Detection"),
                    (SettingsPage::LMStudio, "LM Studio"),
                    (SettingsPage::Ollama, "Ollama"),
                    (SettingsPage::Recovery, "Recovery"),
                    (SettingsPage::Diagnostics, "Diagnostics"),
                ] {
                    if ui
                        .selectable_label(self.settings_page == page, label)
                        .clicked()
                    {
                        self.settings_page = page;
                        self.validation.clear();
                    }
                }
            });
        });
        ui.add_space(design::GAP);
        p.card().show(ui,|ui| {
            ui.set_min_width(ui.available_width());
            ui.add_enabled_ui(!s.commands.settings_pending,|ui| {
                match self.settings_page {
                    SettingsPage::General=> {
                        ui.heading("General");
                        let mut startup=tray::startup_enabled();
                        if ui.styled_checkbox(&mut startup,"Start when I sign in to Windows").changed(){tray::request_startup(&self.shared,startup,&self.folder);}
                        let mut visual=s.config.notifications_enabled;let mut sound=s.config.sound_enabled;
                        let changed=ui.styled_checkbox(&mut visual,"Windows notifications").changed() | ui.styled_checkbox(&mut sound,"Sound for notifications").changed();
                        if changed {self.action(Action::NotificationPreferences{visual,sound},"Notification preferences");}
                        ui.add_space(8.0);ui.label("Appearance");
                        let mut appearance=s.config.appearance;
                        ComboBox::from_id_salt("appearance").selected_text(match appearance {crate::config::Appearance::System=>"Follow Windows",crate::config::Appearance::Light=>"Light",crate::config::Appearance::Dark=>"Dark"}).show_ui(ui,|ui| {
                            for (value,label) in [(crate::config::Appearance::System,"Follow Windows"),(crate::config::Appearance::Light,"Light"),(crate::config::Appearance::Dark,"Dark")] {ui.selectable_value(&mut appearance,value,label);}
                        });
                        if appearance!=s.config.appearance {self.action(Action::Appearance(appearance),"Appearance");}
                        ui.colored_label(p.muted,"High contrast follows Windows colors. Closing the window keeps GamePause in the tray.");
                        if ui.button("Keyboard help").clicked(){self.modal=Some(Modal::Help);}
                    },
                    SettingsPage::Detection=> {
                        ui.heading("Game detection");
                        ui.colored_label(p.muted,"Launcher metadata and registered paths identify games. Running apps helps you add missed executables.");
                        numeric(ui,"Process poll interval (seconds)",&mut self.edit_config.poll_seconds,&mut self.dirty);
                        numeric(ui,"Discovery interval (seconds)",&mut self.edit_config.discovery_seconds,&mut self.dirty);
                        string_list(ui,"Additional game folders",&mut self.edit_config.game_roots,&mut self.dirty);
                        string_list(ui,"Steam roots",&mut self.edit_config.steam_roots,&mut self.dirty);
                        string_list(ui,"Epic manifest folders",&mut self.edit_config.epic_manifest_dirs,&mut self.dirty);
                        string_list(ui,"Excluded executable names",&mut self.edit_config.excluded_executables,&mut self.dirty);
                        self.save_bar(ui,s,p);
                    },
                    SettingsPage::LMStudio=> {
                        ui.heading("LM Studio");
                        let editable=!s.provider_pending(crate::provider::Kind::LMStudio);
                        if !editable {ui.colored_label(p.accent,"Restore pending LM Studio recovery before editing its connection.");}
                        ui.add_enabled_ui(editable,|ui| {
                            if let Some(crate::config::Provider::LMStudio{enabled,connection,..})=self.edit_config.providers.iter_mut().find(|p|p.kind()==crate::provider::Kind::LMStudio) {
                                self.dirty |= ui.styled_checkbox(enabled,"Enable LM Studio control").changed();
                                ui.label("Loopback API address");self.dirty |= ui.text_edit_singleline(&mut connection.endpoint).changed();
                                ui.label("lms executable");
                                ui.horizontal(|ui| {self.dirty|=ui.text_edit_singleline(&mut connection.lms_path).changed();if ui.button("Browse...").clicked(){match browse(self.owner){Ok(Some(path))=>{connection.lms_path=path;self.dirty=true;},Ok(None)=>(),Err(e)=>self.validation=format!("File picker: {e:#}")}}});
                                self.dirty |= ui.styled_checkbox(&mut connection.stop_server_during_gaming,"Stop the captured server during gaming").changed();
                            } else {ui.label("No LM Studio entry is configured.");}
                        });
                        ui.colored_label(p.muted,"Restores captured load settings and verifies the original server state. No automatic downloads.");
                        self.save_bar(ui,s,p);
                    },
                    SettingsPage::Ollama=> {
                        ui.heading("Experimental Ollama");
                        ui.colored_label(p.accent,"Not tested with a live Ollama installation.");
                        ui.label("Supports local GGUF completion models with verified identity, supported context and finite observed expiry. Full load options, parallelism, conversations and KV cache are not preserved. Embedding/cloud models and unknown settings are refused.");
                        ui.label("The user-owned service stays running. GamePause does not download models or fight later client reloads.");
                        let saved=s.config.providers.iter().find(|p|p.kind()==crate::provider::Kind::Ollama);
                        let editable=!s.provider_pending(crate::provider::Kind::Ollama);
                        ui.add_enabled_ui(editable,|ui| {
                            if let Some(saved)=saved {
                                let mut enabled=saved.enabled();
                                if ui.styled_checkbox(&mut enabled,"Enable experimental Ollama control").changed(){
                                    if enabled {self.modal=Some(Modal::Ollama(saved.clone()));}
                                    else {let mut c=s.config.clone();if let Some(crate::config::Provider::Ollama{enabled,..})=c.providers.iter_mut().find(|p|p.kind()==crate::provider::Kind::Ollama){*enabled=false;}self.save(c,true);}
                                }
                                if let Some(crate::config::Provider::Ollama{endpoint,..})=self.edit_config.providers.iter_mut().find(|p|p.kind()==crate::provider::Kind::Ollama) {
                                    ui.label("Loopback endpoint");self.dirty |= ui.text_edit_singleline(endpoint).changed();
                                }
                                if ui.button("Save Ollama endpoint").clicked(){
                                    let mut c=s.config.clone();
                                    let value=self.edit_config.providers.iter().find(|p|p.kind()==crate::provider::Kind::Ollama).map(|p|p.endpoint().to_owned());
                                    if let (Some(value),Some(crate::config::Provider::Ollama{endpoint,..}))=(value,c.providers.iter_mut().find(|p|p.kind()==crate::provider::Kind::Ollama)){*endpoint=value;self.save(c,true);self.dirty=false;}
                                }
                            } else {ui.label("No Ollama entry is configured.");}
                        });
                        if !editable {ui.colored_label(p.accent,"Finish pending Ollama recovery before changing its enrollment or endpoint.");}
                        ui.hyperlink_to("Contribute Ollama fixes or live evidence",concat!(env!("CARGO_PKG_REPOSITORY"),"/blob/main/CONTRIBUTING.md"));
                    },
                    SettingsPage::Recovery=> {
                        ui.heading("Pause and recovery");
                        numeric(ui,"Restore after games exit (seconds)",&mut self.edit_config.restore_delay_seconds,&mut self.dirty);
                        numeric(ui,"Retry interval (seconds)",&mut self.edit_config.retry_seconds,&mut self.dirty);
                        ui.label("Original model settings are saved before unloading. Partial failures retain recovery until verification succeeds. Ignoring a game does not erase pending recovery checks.");
                        ui.colored_label(if s.pending {p.accent}else{p.muted},if s.pending {"Recovery is pending"}else{"No saved recovery is pending"});
                        self.save_bar(ui,s,p);
                        ui.add_space(design::GAP);
                        if ui.add_enabled(crate::ui_commands::verify_available(s),Button::new("Test LM Studio round-trip...")).clicked(){self.modal=Some(Modal::Verify);}
                        ui.colored_label(p.muted,"The test captures, unloads and reloads your live LM Studio models. It requires no running games or pending recovery.");
                    },
                    SettingsPage::Diagnostics=> {
                        ui.heading("Read-only diagnostics");
                        if ui.add_enabled(!s.doctor_pending,Button::new(if s.doctor_pending {"Checking..."}else{"Check enabled providers"})).clicked(){self.action(Action::Doctor,"Read-only diagnostics");}
                        ui.colored_label(p.muted,"Uses saved connections. Does not start services or load/unload models.");
                        let report=crate::diagnostics::render(s);
                        ui.add(Label::new(report).wrap());
                        if design::button(ui,"Open logs and status folder",Icon::Folder,false,p).clicked(){tray::request_folder(&self.shared,&self.folder);}
                    },
                }
            });
            if !self.validation.is_empty(){ui.colored_label(p.error,&self.validation);}
            if !s.settings_error.is_empty(){ui.colored_label(p.error,&s.settings_error);}
        });
    }
    fn save_bar(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        ui.add_space(design::GAP);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.dirty && !s.commands.settings_pending,
                    Button::new("Save settings").fill(p.selected),
                )
                .clicked()
            {
                self.save(self.edit_config.clone(), true);
                if self.validation.is_empty() {
                    self.dirty = false;
                }
            }
            if ui
                .add_enabled(self.dirty, Button::new("Discard edits"))
                .clicked()
            {
                self.edit_config = s.config.clone();
                self.dirty = false;
                self.validation.clear();
            }
            if self.dirty {
                ui.colored_label(p.muted, "Unsaved changes");
            }
        });
    }
    fn activity(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        ui.horizontal(|ui| {
            ui.heading("Activity");
            if ui.button("Back to games").clicked() {
                self.set_page(Page::Games);
            }
            if ui.button("Open logs folder").clicked() {
                tray::request_folder(&self.shared, &self.folder);
            }
            if ui.button("Load recent worker log").clicked() {
                self.worker_log = Some(
                    read_worker_log(&self.folder)
                        .unwrap_or_else(|error| format!("Could not read worker log: {error}")),
                );
            }
        });
        ui.colored_label(p.muted,"Recent observed status changes while this dashboard is open. Detailed worker logs remain in the data folder.");
        p.card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Current state");
            let summary = crate::presentation::summarize(s);
            ui.label(&summary.games);
            ui.label(summary.ai_text());
            if let Some(result) = &s.commands.latest {
                ui.label(format!(
                    "Command #{}: {:?}\n{}",
                    result.id, result.outcome, result.message
                ));
            }
            if let Some(feedback) = &s.restore_feedback {
                ui.label(feedback.text());
            }
            if let Some(report) = &s.verify_report {
                ui.label(render_verify_report(report));
            }
            for (source, error) in &s.discovery_errors {
                ui.colored_label(p.error, format!("{source}: {error}"));
            }
        });
        ui.add_space(design::GAP);
        p.card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Recent events");
            for entry in &self.log.entries {
                ui.separator();
                ui.add(Label::new(entry).wrap());
            }
        });
        if let Some(text) = self.worker_log.as_mut() {
            ui.add_space(design::GAP);
            p.card().show(ui, |ui| {
                ui.strong("Recent worker log (timestamps are Unix seconds)");
                ui.colored_label(
                    p.muted,
                    "Loaded on request. Up to 64 KiB from the current local log.",
                );
                ui.add(
                    TextEdit::multiline(text)
                        .desired_width(f32::INFINITY)
                        .desired_rows(12)
                        .interactive(false),
                );
            });
        }
    }
    fn modal(&mut self, ctx: &Context, s: &Shared, p: Palette) {
        let Some(mut modal) = self.modal.take() else {
            self.modal_active = false;
            return;
        };
        let first = !self.modal_active;
        self.modal_active = true;
        let mut cancel = false;
        let mut accepted = false;
        let title = match &modal {
            Modal::Add { .. } => "Add game",
            Modal::Rename(..) => "Rename game",
            Modal::Remove(..) => "Remove custom game?",
            Modal::Resume(..) => "Resume AI while a game is running?",
            Modal::Verify => "Test live LM Studio recovery?",
            Modal::Ollama(..) => "Enable experimental Ollama?",
            Modal::Help => "Keyboard and controls",
        };
        let response=egui::Modal::new(Id::new("gamepause-modal")).frame(p.card().inner_margin(20)).show(ctx,|ui| {
            ui.set_width(510.0_f32.min(ctx.content_rect().width()-64.0));
            ui.heading(title);ui.add_space(design::GAP);
            match &mut modal {
                Modal::Add{name,path,auto}=> {
                    ui.label("Choose the game executable. Its name is filled from the filename.");
                    ui.label("Game name");ui.text_edit_singleline(name);
                    ui.label("Executable");ui.horizontal(|ui| {
                        ui.add_sized([ui.available_width()-112.0,design::CONTROL],TextEdit::singleline(path));
                        if ui.button("Browse...").clicked(){match browse(self.owner){Ok(Some(value))=>{*path=value;if name.trim().is_empty(){*name=std::path::Path::new(path).file_stem().unwrap_or_default().to_string_lossy().into_owned();}},Ok(None)=>(),Err(e)=>self.validation=format!("Could not choose executable: {e:#}")}}
                    });
                    ui.label("Source: Custom registration");ui.styled_checkbox(auto,"Automatically pause AI for this game");
                },
                Modal::Rename(row,name)=> {ui.label(&row.path);ui.label("Game name");ui.text_edit_singleline(name);},
                Modal::Remove(row)=> {ui.label(&row.name);ui.add(Label::new(&row.path).wrap());ui.label("Removes only this custom registration. Pending recovery keeps its original game checks.");},
                Modal::Resume(offer,checked)=> {
                    ui.colored_label(p.accent,"Resuming AI may consume GPU memory and affect game performance.");
                    ui.label("This approval ends when a listed game exits/restarts, another nonignored game starts, you pause AI, or GamePause restarts.");
                    ui.add_space(8.0);
                    ScrollArea::vertical().max_height(220.0).show(ui,|ui| {
                        for (game,ignore) in offer.games.iter().zip(checked.iter_mut()) {
                            ui.strong(format!("{} (PID {})",game.game,game.pid));
                            ui.add(Label::new(RichText::new(&game.executable).small().color(p.muted)).wrap());
                            ui.styled_checkbox(ignore,"Also ignore this executable for future pauses");ui.separator();
                        }
                    });
                    ui.colored_label(p.muted,"Ignore selections are optional. Resume works without selecting any games.");
                },
                Modal::Verify=> {ui.label("This live test captures settings, unloads models and restores them. It can interrupt current inference. Recovery safeguards and fresh game checks remain in force.");},
                Modal::Ollama(provider)=> {
                    ui.label(format!("Saved endpoint: {}",provider.endpoint()));
                    ui.colored_label(p.accent,"Experimental. Not tested with a live Ollama installation.");
                    ui.label("Only supported local GGUF completion models with finite expiry are eligible. Restoration verifies identity, context and remaining observed residency deadline. Full load settings, parallelism, conversations and KV cache are not preserved. Embedding/cloud models and unknown settings are refused.");
                    ui.label("The user-owned service stays running. No model downloads or repeated unloading of later client reloads.");
                },
                Modal::Help=> {ui.label("Tab / Shift+Tab moves focus. Enter / Space activates controls. Arrow keys select table rows. Escape closes this dialog or returns to Games. F1 opens this help. Closing the dashboard keeps the tray watcher running. Quit uses the existing safe shutdown path.");},
            }
            if !self.validation.is_empty(){ui.colored_label(p.error,&self.validation);}
            ui.add_space(design::GAP);
            ui.horizontal(|ui| {
                let cancel_button=ui.button(if matches!(modal,Modal::Help){"Close"}else{"Cancel"});
                // Cancel is first in keyboard order. Dangerous actions require explicit activation.
                if first {cancel_button.request_focus();}
                cancel=cancel_button.clicked();
                let label=match modal {Modal::Add{..}=>"Add game",Modal::Rename(..)=>"Rename",Modal::Remove(..)=>"Remove",Modal::Resume(..)=>"Resume AI",Modal::Verify=>"Test round-trip",Modal::Ollama(..)=>"Enable Ollama",Modal::Help=>""};
                if !label.is_empty(){accepted=ui.add_enabled(!s.commands.settings_pending,Button::new(label).fill(p.selected).stroke(Stroke::new(1.0_f32,p.accent))).clicked();}
            });
        });
        cancel |= response.should_close();
        if cancel {
            self.modal_active = false;
            self.validation.clear();
            crate::app::local_result(
                &self.shared,
                Outcome::Cancelled,
                "Dialog cancelled; AI and preferences unchanged.",
            );
            return;
        }
        if accepted {
            self.modal_active = false;
            match &modal {
                Modal::Add { name, path, auto } => {
                    if name.trim().is_empty() {
                        self.validation = "Enter a game name.".into();
                    } else if !std::path::Path::new(path).is_absolute()
                        || !path.to_lowercase().ends_with(".exe")
                        || !std::path::Path::new(path).is_file()
                    {
                        self.validation =
                            "Choose an existing executable with an absolute path.".into();
                    } else {
                        let mut c = s.config.clone();
                        add_game(&mut c, path.clone(), name.trim().into());
                        set_ignored(&mut c, path, !auto);
                        self.save(c, false);
                        if self.validation.is_empty() {
                            return;
                        }
                    }
                }
                Modal::Rename(row, name) => match apply_rename(&s.config, &row.path, name) {
                    Ok(c) => {
                        self.save(c, false);
                        if self.validation.is_empty() {
                            return;
                        }
                    }
                    Err(e) => self.validation = e,
                },
                Modal::Remove(row) => {
                    // The worker validates exact custom path/name again before persisting.
                    self.action(
                        Action::RemoveCustom {
                            path: row.path.clone(),
                            name: row.name.clone(),
                        },
                        "Remove custom game",
                    );
                    return;
                }
                Modal::Resume(offer, checked) => match confirmed_resume(s, offer, checked) {
                    Ok(action) => {
                        self.action(action, "Resume AI during gameplay");
                        return;
                    }
                    Err(error) => self.validation = error,
                },
                Modal::Verify => {
                    if crate::ui_commands::verify_available(s) {
                        crate::app::request_verify(&self.shared, &self.tx);
                        return;
                    }
                    self.validation="The test is no longer available. Wait for games, recovery or current work to finish.".into();
                }
                Modal::Ollama(provider) => {
                    if !s.config.advanced_settings_visible
                        || s.provider_pending(crate::provider::Kind::Ollama)
                        || s.config
                            .providers
                            .iter()
                            .find(|p| p.kind() == crate::provider::Kind::Ollama)
                            != Some(provider)
                    {
                        self.validation =
                            "Ollama settings changed while confirming. Cancel and try again."
                                .into();
                    } else {
                        let mut c = s.config.clone();
                        if let Some(crate::config::Provider::Ollama { enabled, .. }) = c
                            .providers
                            .iter_mut()
                            .find(|p| p.kind() == crate::provider::Kind::Ollama)
                        {
                            *enabled = true;
                        }
                        self.save(c, true);
                        if self.validation.is_empty() {
                            return;
                        }
                    }
                }
                Modal::Help => return,
            }
        }
        self.modal = Some(modal);
    }
    fn draw(&mut self, ctx: &Context, s: &Shared) {
        let contrast = crate::theme::high_contrast();
        let dark = crate::theme::effective_dark(s.config.appearance);
        let palette = Palette::for_mode(dark, contrast);
        if self.theme != Some((dark, contrast)) {
            palette.install(ctx, dark);
            self.theme = Some((dark, contrast));
        }
        if !self.dirty && s.revision != self.edit_revision {
            self.edit_config = s.config.clone();
            self.edit_revision = s.revision;
        }
        if self.dirty && s.revision != self.edit_revision && !s.commands.settings_pending {
            // Preserve edits only while their base settings have not changed.
            self.edit_config = s.config.clone();
            self.edit_revision = s.revision;
            self.dirty = false;
            self.validation = "Saved settings changed. Review them before editing again.".into();
        }
        let summary = crate::presentation::summarize(s);
        let command = s
            .commands
            .latest
            .as_ref()
            .map_or_else(String::new, |result| {
                format!(
                    "\nCommand #{}: {:?}  {}",
                    result.id, result.outcome, result.message
                )
            });
        self.log.record(format!(
            "{}\n{}{}",
            summary.games,
            summary.ai_text(),
            command
        ));
        if ctx.input(|i| i.key_pressed(Key::F1)) {
            self.modal = Some(Modal::Help);
        }
        if self.modal.is_none() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            if s.config.advanced_settings_visible {
                self.action(Action::AdvancedVisibility(false), "Close Advanced");
            } else {
                self.set_page(Page::Games);
            }
        }
        TopBottomPanel::bottom("footer")
            .frame(
                Frame::new()
                    .fill(palette.background)
                    .inner_margin(Margin::symmetric(22, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        palette.muted,
                        format!("GamePause {}", env!("CARGO_PKG_VERSION")),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if design::button(ui, "Quit", Icon::Power, false, palette).clicked() {
                            crate::app::request_quit(&self.shared, &self.tx);
                        }
                        if design::button(
                            ui,
                            "Activity",
                            Icon::Activity,
                            self.page == Page::Activity,
                            palette,
                        )
                        .clicked()
                        {
                            self.set_page(Page::Activity);
                        }
                    });
                });
            });
        CentralPanel::default()
            .frame(Frame::new().fill(palette.background).inner_margin(18))
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.hero(ui, s, palette);
                        ui.add_space(design::GAP);
                        self.settings_strip(ui, s, palette);
                        ui.add_space(design::GAP);
                        if self.page == Page::Activity {
                            self.activity(ui, s, palette);
                        } else if s.config.advanced_settings_visible {
                            self.advanced(ui, s, palette);
                        } else {
                            self.games(ui, s, palette);
                        }
                    });
            });
        if let Some((message, error)) = self.toast.message(s, Instant::now()) {
            let message = message.to_owned();
            Area::new(Id::new("toast"))
                .anchor(Align2::CENTER_BOTTOM, [0.0, -76.0])
                .order(Order::Foreground)
                .show(ctx, |ui| {
                    palette
                        .card()
                        .stroke(Stroke::new(
                            1.0_f32,
                            if error { palette.error } else { palette.border },
                        ))
                        .show(ui, |ui| {
                            ui.set_max_width((ctx.content_rect().width() - 60.0).min(700.0));
                            ui.add(Label::new(message).wrap());
                        });
                });
            if !error
                && s.commands
                    .latest
                    .as_ref()
                    .is_some_and(|r| !matches!(r.outcome, Outcome::Working | Outcome::Requested))
            {
                ctx.request_repaint_after(
                    Duration::from_secs(5).saturating_sub(self.toast.since.elapsed()),
                );
            }
        }
        self.modal(ctx, s, palette);
    }
}
impl eframe::App for Dashboard {
    fn update(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = frame.window_handle()
            && let RawWindowHandle::Win32(handle) = handle.as_raw()
        {
            self.owner = handle.hwnd.get() as HWND;
        }
        let Ok(s) = self.shared.lock().map(|s| s.clone()) else {
            return;
        };
        while let Some(request) = self.rx.lock().ok().and_then(|rx| rx.try_recv().ok()) {
            match request {
                UiRequest::Stop => {
                    self.stopping = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                UiRequest::Show => {
                    self.visible = true;
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                }
                UiRequest::Resume => {
                    self.visible = true;
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.resume(&s);
                }
                UiRequest::Verify => {
                    self.visible = true;
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    if crate::ui_commands::verify_available(&s) {
                        self.modal = Some(Modal::Verify);
                    }
                }
                UiRequest::Theme => self.theme = None,
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.stopping {
            self.visible = false;
        }
        RUNNING_REQUESTED.store(
            self.visible && self.page == Page::Running && !s.config.advanced_settings_visible,
            Ordering::Relaxed,
        );
        UI_VISIBLE.store(self.visible, Ordering::Relaxed);
        if self.visible {
            self.draw(ctx, &s);
        }
    }
}

fn numeric(ui: &mut Ui, label: &str, value: &mut f64, dirty: &mut bool) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        *dirty |= ui.add(DragValue::new(value).speed(0.5)).changed();
    });
}
fn string_list(ui: &mut Ui, label: &str, list: &mut Vec<String>, dirty: &mut bool) {
    ui.collapsing(label, |ui| {
        let mut remove = None;
        for (index, value) in list.iter_mut().enumerate() {
            ui.push_id((label, index), |ui| {
                ui.horizontal(|ui| {
                    *dirty |= ui.text_edit_singleline(value).changed();
                    if ui.small_button("Remove").clicked() {
                        remove = Some(index);
                    }
                });
            });
        }
        if let Some(index) = remove {
            list.remove(index);
            *dirty = true;
        }
        if ui.button("Add entry").clicked() {
            list.push(String::new());
            *dirty = true;
        }
    });
}
fn read_worker_log(folder: &std::path::Path) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(folder.join("gamepause.log"))?;
    let len = file.metadata()?.len();
    let skipped = len.saturating_sub(65536);
    file.seek(SeekFrom::Start(skipped))?;
    let mut bytes = Vec::new();
    file.take(65536).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(if skipped > 0 {
        text.split_once('\n').map_or("", |(_, tail)| tail).into()
    } else {
        text.into_owned()
    })
}
#[cfg(test)]
pub(crate) fn shared_command_ids(advanced: bool) -> Vec<i32> {
    crate::ui_commands::Command::ALL
        .into_iter()
        .filter(|c| *c != crate::ui_commands::Command::OpenDashboard && c.visible(advanced))
        .map(|c| c as i32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        control::Activity,
        coordinator::{Report, State},
        discovery::Game,
        provider::{Guarantee, Kind},
    };
    use std::sync::Arc;
    fn fixture() -> Shared {
        let mut s = Shared {
            activity: Activity::Paused,
            active_mode: true,
            pending: true,
            discovery_ready: true,
            detection_ok: true,
            ..Default::default()
        };
        s.config.appearance = crate::config::Appearance::Dark;
        for (name, platform) in [
            ("Stardew Valley", "Steam"),
            ("The Last of Us Part II", "Epic"),
            ("The Witcher 3: Wild Hunt — Remastered", "Steam"),
            ("Trine 4: The Nightmare Prince", "Steam"),
        ] {
            s.games.push(Game {
                identity: name.into(),
                name: name.into(),
                launcher: platform.into(),
                path: format!(r"D:\Fixture Games\{name}"),
            });
        }
        let mut active = crate::gameplay::fixtures::game(10416, 11);
        active.game = s.games[2].name.clone();
        active.path = s.games[2].path.clone();
        active.executable = format!(r"{}\game.exe", active.path);
        active.launcher = "Steam".into();
        s.active_games.push(active.clone());
        s.restore_offer = Some(crate::gameplay::RestoreOffer {
            id: 42,
            games: vec![active],
        });
        s.provider_statuses = vec![Report {
            id: s.config.providers[0].id().into(),
            kind: Kind::LMStudio,
            guarantee: Guarantee::CapturedConfiguration,
            state: State::Paused,
            pending: true,
            error: String::new(),
            retry_seconds: None,
        }];
        s.running_apps = vec![
            crate::processes::RunningApp {
                name: "game.exe".into(),
                path: format!(r"{}\game.exe", s.games[2].path),
            },
            crate::processes::RunningApp {
                name: "missing-game.exe".into(),
                path: r"D:\Fixture Games\Unknown\missing-game.exe".into(),
            },
        ];
        s
    }
    fn dashboard(s: Shared) -> Dashboard {
        let (tx, _) = mpsc::channel();
        let (_, rx) = mpsc::channel();
        Dashboard::new(
            Arc::new(Mutex::new(s)),
            tx,
            PathBuf::from("scratch/ui-fixture"),
            Arc::new(Mutex::new(rx)),
        )
    }
    #[test]
    fn pending_recovery_and_idle_do_not_claim_verified_pause_or_current_residency() {
        let mut s = fixture();
        s.activity = Activity::Recovery;
        assert!(!hero(&s).paused);
        assert_eq!(hero(&s).tone, Tone::Error);
        s.activity = Activity::Watching;
        s.pending = false;
        s.provider_statuses.clear();
        assert!(hero(&s).hint.contains("not polled"));
        assert!(!hero(&s).paused);
        assert!(provider_status(&s)[0].0.contains("unpolled"));
        s.provider_statuses = fixture().provider_statuses;
        s.provider_statuses[0].state = State::Restored;
        assert!(
            provider_status(&s)[0]
                .0
                .contains("current residency unpolled")
        );
        s.config.automation_enabled = false;
        assert_eq!(hero(&s).title, "AUTO PAUSE OFF");
        s.detection_ok = false;
        assert!(hero(&s).reason.contains("not yet confirmed"));
    }
    #[test]
    fn game_tables_filter_dedupe_and_preserve_exclusions() {
        let mut s = fixture();
        s.games.push(s.games[0].clone());
        assert_eq!(rows(&s, Page::Games, "").len(), 4);
        assert_eq!(rows(&s, Page::Games, "witcher")[0].pid, Some(10416));
        let path = s.games[0].path.clone();
        set_ignored(&mut s.config, &path, true);
        assert!(rows(&s, Page::Games, "stardew")[0].ignored);
        assert_eq!(rows(&s, Page::Ignored, "").len(), 1);
        set_ignored(&mut s.config, &path, false);
        assert!(rows(&s, Page::Ignored, "").is_empty());
        assert_eq!(
            rows(&s, Page::Running, "missing")[0].platform,
            "Unrecognized"
        );
    }
    #[test]
    fn success_toasts_expire_unchanged_results_and_errors_persist() {
        let start = Instant::now();
        let mut toast = Toast::new();
        let mut s = Shared::default();
        s.commands.local(Outcome::Completed, "Settings saved");
        assert!(toast.message(&s, start).is_some());
        assert!(toast.message(&s, start + Duration::from_secs(6)).is_none());
        s.commands.local(Outcome::Failed, "Save refused");
        assert!(
            toast
                .message(&s, start + Duration::from_secs(60))
                .unwrap()
                .1
        );
        assert!(
            toast
                .message(&s, start + Duration::from_secs(120))
                .is_some()
        );
    }
    #[test]
    fn gameplay_resume_opens_unchecked_modal_and_does_not_send_restore() {
        let s = fixture();
        let (tx, rx) = mpsc::channel();
        let (_, ui_rx) = mpsc::channel();
        let mut d = Dashboard::new(
            Arc::new(Mutex::new(s.clone())),
            tx,
            PathBuf::from("scratch"),
            Arc::new(Mutex::new(ui_rx)),
        );
        d.resume(&s);
        assert!(
            matches!(d.modal,Some(Modal::Resume(_,ref choices)) if choices.iter().all(|on|!*on))
        );
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn gameplay_confirmation_refuses_stale_offers_busy_control_and_unselected_exclusions() {
        let mut s = fixture();
        let offer = s.restore_offer.clone().unwrap();
        assert!(
            matches!(confirmed_resume(&s,&offer,&[false]),Ok(Action::ConfirmedRestore{ignored,..}) if ignored.is_empty())
        );
        assert!(
            matches!(confirmed_resume(&s,&offer,&[true]),Ok(Action::ConfirmedRestore{ignored,..}) if ignored==vec![offer.games[0].executable.clone()])
        );
        s.restore_offer.as_mut().unwrap().id += 1;
        assert!(confirmed_resume(&s, &offer, &[true]).is_err());
        s.restore_offer = Some(offer.clone());
        s.activity = Activity::Restoring;
        assert!(confirmed_resume(&s, &offer, &[true]).is_err());
        s.activity = Activity::Paused;
        s.commands.settings_pending = true;
        assert!(confirmed_resume(&s, &offer, &[true]).is_err());
    }
    #[test]
    fn recent_worker_log_has_a_read_cap_and_discards_partial_first_line() {
        let folder = PathBuf::from(format!("scratch/log-fixture-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut text = "x".repeat(70000);
        text.push_str("\nlast complete event\n");
        std::fs::write(folder.join("gamepause.log"), text).unwrap();
        assert_eq!(read_worker_log(&folder).unwrap(), "last complete event\n");
        std::fs::remove_file(folder.join("gamepause.log")).unwrap();
        std::fs::remove_dir(folder).unwrap();
    }
    #[test]
    #[ignore = "opens only a fictional-state dashboard to exercise UI-thread lifecycle"]
    fn ui_bridge_hides_reopens_and_stops_without_backend() {
        fn until(label: &str, mut ready: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready() {
                assert!(Instant::now() < deadline, "UI lifecycle timed out: {label}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                close();
            }
        }
        let _cleanup = Cleanup;
        let s = Arc::new(Mutex::new(fixture()));
        let (tx, rx) = mpsc::channel();
        show(s, tx, PathBuf::from("scratch/ui-fixture"));
        until("window open", || UI_VISIBLE.load(Ordering::Relaxed));
        let ctx = BRIDGE
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .ctx
            .clone()
            .unwrap();
        ctx.send_viewport_cmd(ViewportCommand::Close);
        ctx.request_repaint();
        until("window close", || !UI_VISIBLE.load(Ordering::Relaxed));
        assert!(!needs_running_apps());
        theme_changed();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !UI_VISIBLE.load(Ordering::Relaxed),
            "A theme change must not reopen a closed dashboard"
        );
        let before = BRIDGE.lock().unwrap().as_ref().unwrap().fingerprint.clone();
        refresh();
        assert_eq!(before, BRIDGE.lock().unwrap().as_ref().unwrap().fingerprint);
        dispatch(UiRequest::Show);
        until("window open", || UI_VISIBLE.load(Ordering::Relaxed));
        dispatch(UiRequest::Resume);
        ctx.request_repaint();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            rx.try_recv().is_err(),
            "Opening a warning must not send a restore action"
        );
        close();
        assert!(BRIDGE.lock().unwrap().is_none());
    }
    #[test]
    fn activity_is_bounded_and_unchanged_frames_add_no_events() {
        let mut log = ActivityLog::new();
        log.record("same".into());
        log.record("same".into());
        assert_eq!(log.entries.len(), 1);
        for index in 0..300 {
            log.record(format!("event {index}"));
        }
        assert_eq!(log.entries.len(), 160);
    }
    #[test]
    fn all_pages_and_states_render_at_supported_sizes_and_dpi() {
        for dpi in [1.0, 1.25, 1.5, 1.75, 2.0] {
            for size in [vec2(620.0, 580.0), vec2(1120.0, 920.0), vec2(1500.0, 960.0)] {
                let mut d = dashboard(fixture());
                let ctx = Context::default();
                design::fonts(&ctx);
                for activity in [
                    Activity::Unknown,
                    Activity::Watching,
                    Activity::Observation,
                    Activity::Unavailable,
                    Activity::DetectionUnavailable,
                    Activity::Capturing,
                    Activity::WaitingForInference,
                    Activity::Unloading,
                    Activity::Paused,
                    Activity::ManualHold,
                    Activity::Countdown,
                    Activity::Restoring,
                    Activity::Recovery,
                    Activity::PartialFailure,
                    Activity::Verifying,
                    Activity::Coexistence,
                ] {
                    let mut s = fixture();
                    s.activity = activity;
                    let input = RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                        ..Default::default()
                    };
                    ctx.set_pixels_per_point(dpi);
                    let output = ctx.run(input, |ctx| d.draw(ctx, &s));
                    assert!(!output.shapes.is_empty());
                }
                for page in [Page::Games, Page::Running, Page::Ignored, Page::Activity] {
                    d.page = page;
                    let output = ctx.run(
                        RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                            ..Default::default()
                        },
                        |ctx| d.draw(ctx, &fixture()),
                    );
                    assert!(!output.shapes.is_empty());
                }
                for page in [
                    SettingsPage::General,
                    SettingsPage::Detection,
                    SettingsPage::LMStudio,
                    SettingsPage::Ollama,
                    SettingsPage::Recovery,
                    SettingsPage::Diagnostics,
                ] {
                    d.page = Page::Games;
                    d.settings_page = page;
                    let mut s = fixture();
                    s.config.advanced_settings_visible = true;
                    let output = ctx.run(
                        RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                            ..Default::default()
                        },
                        |ctx| d.draw(ctx, &s),
                    );
                    assert!(!output.shapes.is_empty());
                }
            }
        }
    }
    /// Render only fictional state. No watcher, provider, startup or config writes.
    #[test]
    #[ignore = "opens an isolated renderer window for visual review"]
    fn ui_design_review_snapshots() {
        struct Review {
            d: Dashboard,
            stage: usize,
            frames: u32,
            start: Instant,
        }
        impl eframe::App for Review {
            fn update(&mut self, ctx: &Context, _: &mut eframe::Frame) {
                assert!(
                    self.start.elapsed() < Duration::from_secs(60),
                    "Screenshot renderer timed out"
                );
                let mut image = None;
                ctx.input(|i| {
                    for e in &i.events {
                        if let Event::Screenshot { image: shot, .. } = e {
                            image = Some(shot.clone());
                        }
                    }
                });
                if let Some(image) = image {
                    let bytes = image
                        .pixels
                        .iter()
                        .flat_map(|pixel| pixel.to_array())
                        .collect::<Vec<_>>();
                    let path = format!("scratch/ui-review/{:02}.png", self.stage);
                    image::save_buffer(
                        &path,
                        &bytes,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                    self.stage += 1;
                    self.frames = 0;
                    if self.stage == 15 {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                        return;
                    }
                }
                if self.stage >= 15 {
                    return;
                }
                let mut s = fixture();
                self.d.page = Page::Games;
                self.d.modal = None;
                let size = match self.stage {
                    1 => vec2(620.0, 580.0),
                    2 => vec2(1500.0, 960.0),
                    _ => vec2(1120.0, 920.0),
                };
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                match self.stage {
                    3 => self.d.page = Page::Running,
                    4 => {
                        self.d.page = Page::Ignored;
                        let path = s.games[0].path.clone();
                        set_ignored(&mut s.config, &path, true);
                    }
                    5..=10 => {
                        s.config.advanced_settings_visible = true;
                        self.d.settings_page = [
                            SettingsPage::General,
                            SettingsPage::Detection,
                            SettingsPage::LMStudio,
                            SettingsPage::Ollama,
                            SettingsPage::Recovery,
                            SettingsPage::Diagnostics,
                        ][self.stage - 5];
                    }
                    11 => {
                        self.d.modal =
                            Some(Modal::Resume(s.restore_offer.clone().unwrap(), vec![false]))
                    }
                    12 => {
                        self.d.modal = Some(Modal::Add {
                            name: "Fixture game".into(),
                            path: r"D:\Fixture Games\play.exe".into(),
                            auto: true,
                        })
                    }
                    13 => self.d.page = Page::Activity,
                    14 => {
                        s.activity = Activity::PartialFailure;
                        s.config.appearance = crate::config::Appearance::Light;
                    }
                    _ => (),
                }
                self.d.draw(ctx, &s);
                self.frames += 1;
                if self.frames == 6 {
                    ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
                }
                ctx.request_repaint_after(Duration::from_millis(40));
            }
        }
        std::fs::create_dir_all("scratch/ui-review").unwrap();
        eframe::run_native(
            "GamePause isolated UI review",
            native_options(),
            Box::new(|cc| {
                design::fonts(&cc.egui_ctx);
                let mut d = dashboard(fixture());
                d.selected = Some(fixture().games[3].path.clone());
                Ok(Box::new(Review {
                    d,
                    stage: 0,
                    frames: 0,
                    start: Instant::now(),
                }))
            }),
        )
        .unwrap();
        assert!(std::path::Path::new("scratch/ui-review/14.png").exists());
    }
}

pub fn render_verify_report(report: &crate::engine::VerifyReport) -> String {
    let mut lines = report
        .steps
        .iter()
        .map(|s| {
            format!(
                "{}: {} ({})",
                s.name,
                s.detail,
                if s.ok { "ok" } else { "FAIL" }
            )
        })
        .collect::<Vec<_>>()
        .join("  |  ");
    lines.push_str("  —  ");
    lines.push_str(&report.summary);
    lines
}

pub fn apply_save(config: &Config, delay_text: &str, host_text: &str) -> Result<Config, String> {
    let mut out = config.clone();
    let delay = delay_text
        .trim()
        .parse::<f64>()
        .map_err(|_| "Restore delay must be a number of seconds.".to_string())?;
    if !(0.0..=3600.0).contains(&delay) {
        return Err("Restore delay must be between 0 and 3600 seconds.".to_string());
    }
    out.restore_delay_seconds = delay;
    let host = host_text.trim();
    if host.is_empty() {
        return Err("Local API address cannot be empty.".to_string());
    }
    out.lm_mut().map_err(|e| e.to_string())?.endpoint = host.into();
    out.validate()
        .map_err(|e| format!("Check these settings: {e:#}"))?;
    Ok(out)
}

/// Pure: apply the RENAME action to a config, returning the new config on
/// success or a human-readable error string for the FEEDBACK line.
/// Only rows that are custom (user-added) are renameable; discovered rows
/// keep their launcher-provided name.
pub fn apply_rename(config: &Config, row_path: &str, new_name: &str) -> Result<Config, String> {
    let name = new_name.trim();
    if name.is_empty() {
        return Err("Game name cannot be empty.".to_string());
    }
    let mut out = config.clone();
    let target = canonical(row_path);
    if let Some(game) = out
        .extra_games
        .iter_mut()
        .find(|g| canonical(&g.path) == target)
    {
        game.name = name.into();
        Ok(out)
    } else {
        Err("This game is not a custom entry — only games you added can be renamed.".to_string())
    }
}

fn browse(hwnd: HWND) -> anyhow::Result<Option<String>> {
    unsafe {
        let mut buffer = vec![0u16; 32768];
        let filter = wide("Windows executable (*.exe)\0*.exe\0\0");
        let mut dialog: OPENFILENAMEW = std::mem::zeroed();
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.hwndOwner = hwnd;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
        if GetOpenFileNameW(&mut dialog) == 0 {
            let error = CommDlgExtendedError();
            if error != 0 {
                anyhow::bail!("File picker failed (code {error})");
            }
            return Ok(None);
        }
        let n = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        Ok(Some(String::from_utf16_lossy(&buffer[..n])))
    }
}

fn add_game(config: &mut Config, path: String, name: String) {
    config
        .excluded_paths
        .retain(|p| canonical(p) != canonical(&path));
    config
        .ignored_games
        .retain(|p| canonical(p) != canonical(&path));
    if !config
        .extra_games
        .iter()
        .any(|g| canonical(&g.path) == canonical(&path))
    {
        config.extra_games.push(ExtraGame { name, path });
    }
}

#[cfg(test)]
mod preserved_behavior_tests {
    use super::*;
    #[test]
    fn adding_executable_deduplicates_and_clears_ignore() {
        let mut config = Config::default();
        config.ignored_games.push(r"D:\Games\game.exe".into());
        add_game(&mut config, r"d:\games\GAME.exe".into(), "Game".into());
        add_game(&mut config, r"D:\Games\game.exe".into(), "Game".into());
        assert_eq!(config.extra_games.len(), 1);
        assert!(config.ignored_games.is_empty());
    }
    #[test]
    fn scale_follows_dpi_at_100_150_200() {
        assert_eq!(scale(100, 96), 100);
        assert_eq!(scale(100, 144), 150);
        assert_eq!(scale(100, 192), 200);
        assert_eq!(scale(960, 96), 960);
        assert_eq!(scale(960, 144), 1440);
        assert_eq!(scale(960, 192), 1920);
    }
    #[test]
    fn render_verify_report_shows_every_step_and_the_failing_one() {
        // A failed report must name each step, mark the failing one FAIL, and
        // carry the summary so the FEEDBACK line explains the outcome inline.
        let report = crate::engine::VerifyReport {
            steps: vec![
                crate::engine::VerifyStep {
                    name: "capture".into(),
                    ok: true,
                    detail: "1 model(s) captured".into(),
                },
                crate::engine::VerifyStep {
                    name: "unload".into(),
                    ok: true,
                    detail: "1 model(s) unloaded".into(),
                },
                crate::engine::VerifyStep {
                    name: "verify-unloaded".into(),
                    ok: false,
                    detail: "models still loaded after unload".into(),
                },
            ],
            ok: false,
            summary: "Round-trip verify failed at: verify-unloaded".into(),
        };
        let rendered = super::render_verify_report(&report);
        for needle in [
            "capture: 1 model(s) captured (ok)",
            "unload: 1 model(s) unloaded (ok)",
            "verify-unloaded: models still loaded after unload (FAIL)",
            "Round-trip verify failed at: verify-unloaded",
        ] {
            assert!(rendered.contains(needle), "missing {needle} in: {rendered}");
        }

        // The success path renders every step as ok and the passing summary.
        let pass = crate::engine::VerifyReport {
            steps: vec![crate::engine::VerifyStep {
                name: "capture".into(),
                ok: true,
                detail: "0 model(s) captured".into(),
            }],
            ok: true,
            summary: "Round-trip verify passed: capture, unload, restore, and field-compare all succeeded".into(),
        };
        let rendered = super::render_verify_report(&pass);
        assert!(rendered.contains("capture: 0 model(s) captured (ok)"));
        assert!(rendered.contains("Round-trip verify passed"));
        assert!(
            !rendered.contains("FAIL"),
            "a passing report must not say FAIL"
        );
    }
    #[test]
    fn save_validation_routes_to_feedback_not_modal() {
        // A bad value yields the Err(String) variant — the exact value dispatch
        // routes to the FEEDBACK line (set_feedback), never to a modal.
        let cfg = crate::config::Config::default();
        let bad = super::apply_save(&cfg, "not-a-number", "127.0.0.1:1234");
        let err = bad.expect_err("a non-numeric delay must fail validation");
        assert!(
            err.contains("number of seconds"),
            "the feedback message should name the field; got {err}"
        );

        let cfg = crate::config::Config::default();
        assert!(
            super::apply_save(&cfg, "10", "   ").is_err(),
            "an empty API host must fail validation"
        );

        // a valid save succeeds and applies the edited fields
        let cfg = crate::config::Config::default();
        let ok = super::apply_save(&cfg, "45", " 127.0.0.1:8080 ")
            .expect("a valid delay + host should pass");
        assert_eq!(ok.restore_delay_seconds, 45.0);
        assert_eq!(ok.lm_endpoint(), "127.0.0.1:8080");
    }
    #[test]
    fn rename_updates_the_matching_custom_entry() {
        let mut cfg = crate::config::Config::default();
        cfg.extra_games.push(crate::config::ExtraGame {
            name: "Old Name".into(),
            path: "C:\\Games\\App\\game.exe".into(),
        });
        let ok = super::apply_rename(&cfg, "c:/games/app/game.exe", "New Name")
            .expect("renaming an existing custom entry should succeed");
        assert_eq!(ok.extra_games[0].name, "New Name");
        assert_eq!(ok.extra_games[0].path, "C:\\Games\\App\\game.exe");

        let err = super::apply_rename(&cfg, "C:\\Games\\Nope\\missing.exe", "X")
            .expect_err("a non-custom path must be rejected");
        assert!(err.contains("not a custom entry"), "got: {err}");

        let err = super::apply_rename(&cfg, "C:\\Games\\App\\game.exe", "   ")
            .expect_err("a blank name must be rejected");
        assert!(err.contains("empty"), "got: {err}");
    }
}

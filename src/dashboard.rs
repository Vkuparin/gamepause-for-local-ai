//! Rust-native dashboard. Engine work stays on the existing worker channel.
use crate::dashboard_theme::{
    self as design, Checkbox, CheckboxUi, Emphasis, Icon, Look, Palette, Push,
};
use crate::{
    app::{Action, Shared, SharedState},
    commands::Outcome,
    config::Config,
    control::CoreCommand,
    discovery::canonical,
    tray, wide,
};
mod game_rules;
mod view_data;
use game_rules::{add_game, answer_ask, asking_paths, exclude_executable, set_ask, set_ignored};
pub use game_rules::{apply_rename, apply_save, render_verify_report};
use view_data::{Hero, Row, Tone, hero, provider_status, rows, running_executable, sort_rows};

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
    /// The open dashboard window, or 0. Used only by the tray UI thread to
    /// bring a minimized window back; never handed to a worker.
    window: isize,
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
/// A minimized window draws no frames, and requests are read while drawing:
/// without this, Show, Resume and Stop would wait until someone restored it.
/// A fullscreen game minimizes the dashboard, so that is the usual case.
fn wake_window(command: i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, ShowWindowAsync};
    let window = BRIDGE
        .lock()
        .ok()
        .and_then(|bridge| bridge.as_ref().map(|b| b.window))
        .unwrap_or(0) as HWND;
    // No lock is held here, and the call does not wait for the UI thread.
    if !window.is_null() && unsafe { IsIconic(window) } != 0 {
        unsafe {
            ShowWindowAsync(window, command);
        }
    }
}
fn dispatch(request: UiRequest) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_RESTORE, SW_SHOWNOACTIVATE};
    let wake = match request {
        UiRequest::Show | UiRequest::Resume | UiRequest::Verify => Some(SW_RESTORE),
        UiRequest::Stop => Some(SW_SHOWNOACTIVATE),
        UiRequest::Theme => None,
    };
    if let Some(command) = wake {
        wake_window(command);
    }
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
    wake_window(windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE);
    let bridge = BRIDGE.lock().ok().and_then(|mut b| b.take());
    if let Some(mut bridge) = bridge {
        let _ = bridge.tx.send(UiRequest::Stop);
        if let Some(ctx) = bridge.ctx {
            ctx.request_repaint();
        }
        if let Some(thread) = bridge.thread.take() {
            // Quit must not hang on a renderer that draws no frame. The
            // process is exiting; a thread that has not stopped is left to it.
            let deadline = Instant::now() + Duration::from_secs(3);
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
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
        window: 0,
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
                    "GamePause for Local AI",
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
                    b.window = 0;
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
            .with_inner_size([1114.0, 848.0])
            .with_min_inner_size([620.0, 580.0])
            .with_icon(app_icon(
                Palette::for_mode(true, false, Look::default()).accent,
            )),
        renderer: eframe::Renderer::Glow,
        event_loop_builder: Some(Box::new(|builder| {
            builder.with_any_thread(true);
        })),
        ..Default::default()
    }
}
/// The window icon is the pause mark in the current state color.
fn app_icon(color: Color32) -> IconData {
    let mut rgba = vec![0; 32 * 32 * 4];
    for y in 4..28 {
        for x in (7..13).chain(19..25) {
            let i = (y * 32 + x) * 4;
            rgba[i..i + 4].copy_from_slice(&[color.r(), color.g(), color.b(), 255]);
        }
    }
    IconData {
        rgba,
        width: 32,
        height: 32,
    }
}
/// Tint the native caption. Windows 10 ignores the color attributes and keeps its own bar.
fn caption(hwnd: HWND, palette: Palette, dark: bool) {
    use windows_sys::Win32::Graphics::Dwm::*;
    const SYSTEM: u32 = 0xFFFF_FFFF;
    let set = |attribute: DWMWINDOWATTRIBUTE, value: u32| unsafe {
        DwmSetWindowAttribute(hwnd, attribute as u32, (&raw const value).cast(), 4);
    };
    let (bar, text) = palette.caption.map_or((SYSTEM, SYSTEM), |c| {
        (
            c.r() as u32 | (c.g() as u32) << 8 | (c.b() as u32) << 16,
            0x00FF_FFFF,
        )
    });
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark as u32);
    set(DWMWA_CAPTION_COLOR, bar);
    set(DWMWA_TEXT_COLOR, text);
}
/// The native tray routes gameplay confirmation into the same themed modal.
pub fn request_resume(shared: SharedState, tx: Sender<Action>, folder: PathBuf) {
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
    Apps,
    Recovery,
    Diagnostics,
}

fn game_tile(ui: &Ui, rect: Rect, texture: Option<TextureHandle>, fallback: Icon, p: Palette) {
    match texture {
        Some(texture) => egui::Image::new(&texture)
            .corner_radius(5)
            .paint_at(ui, rect),
        None => {
            ui.painter().rect_filled(rect, 5, p.elevated);
            design::icon(
                ui.painter(),
                rect.shrink(rect.width() * 0.16),
                fallback,
                p.accent,
            );
        }
    }
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
    Help,
    /// Escape was pressed in Advanced with unsaved edits.
    Discard,
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
    sort: (usize, bool),
    modal: Option<Modal>,
    modal_active: bool,
    owner: HWND,
    edit_config: Config,
    /// Saved settings the draft was last based on.
    edit_base: Config,
    edit_revision: u64,
    dirty: bool,
    validation: String,
    log: ActivityLog,
    toast: Toast,
    visible: bool,
    stopping: bool,
    theme: Option<(bool, bool, Look)>,
    chrome: Option<(bool, bool)>,
    window_icon: Option<Color32>,
    icons: crate::game_icons::Cache,
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
            sort: (0, true),
            modal: None,
            modal_active: false,
            owner: null_mut(),
            edit_base: s.config.clone(),
            edit_config: s.config,
            edit_revision: s.revision,
            dirty: false,
            validation: String::new(),
            log: ActivityLog::new(),
            toast: Toast::new(),
            visible: true,
            stopping: false,
            theme: None,
            chrome: None,
            window_icon: None,
            icons: Default::default(),
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
        self.sort = (0, true);
        RUNNING_REQUESTED.store(self.visible && page == Page::Running, Ordering::Relaxed);
    }
    fn tone(p: Palette, t: Tone) -> Color32 {
        match t {
            Tone::Neutral => p.muted,
            Tone::Busy => p.busy,
            Tone::Success => p.success,
            Tone::Error => p.error,
        }
    }
    fn hero(&mut self, ui: &mut Ui, s: &Shared, h: &Hero, p: Palette) {
        p.card()
            .inner_margin(Margin::symmetric(24, 20))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.set_min_width(ui.available_width());
                let narrow = ui.available_width() < 780.0;
                ui.horizontal(|ui| {
                    if ui.available_width() > 650.0 {
                        let (rect, _) = ui.allocate_exact_size(vec2(112.0, 112.0), Sense::hover());
                        design::ring(ui.painter(), rect, h.glyph, p);
                        ui.add_space(14.0);
                    }
                    let right = if narrow {
                        0.0
                    } else {
                        design::HERO_ACTION.x + design::GAP
                    };
                    let width = (ui.available_width() - right - design::GAP).max(220.0);
                    ui.allocate_ui_with_layout(
                        vec2(width, 112.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_min_width(width);
                            // Text rows keep their own height instead of control height.
                            ui.spacing_mut().interact_size.y = 0.0;
                            ui.label(
                                RichText::new(h.title)
                                    .size(design::HERO_FONT)
                                    .line_height(Some(design::HERO_FONT + 6.0))
                                    .family(design::hero())
                                    .color(p.accent),
                            );
                            if let Some(game) = &h.game {
                                ui.add(Label::new(RichText::new(game).size(23.0)).truncate())
                                    .on_hover_text(game);
                            }
                            for (name, text, tone) in provider_status(s) {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 7.0;
                                    let (dot, _) =
                                        ui.allocate_exact_size(vec2(20.0, 26.0), Sense::hover());
                                    ui.painter().circle_filled(
                                        dot.center(),
                                        6.5,
                                        Self::tone(p, tone),
                                    );
                                    ui.label(
                                        RichText::new(name).size(19.0).family(design::heading()),
                                    );
                                    ui.add(Label::new(RichText::new(text).size(19.0)).truncate());
                                });
                            }
                            if !s.config.any_provider_enabled() {
                                ui.colored_label(p.muted, "No AI provider is enabled");
                            } else if provider_status(s).is_empty() {
                                ui.colored_label(p.muted, "No AI app found to pause");
                            }
                            // Shown while AI is held paused, not once it is coming back.
                            if s.freed_bytes > 0 && s.pending && h.look == Look::Paused {
                                // Lines up with the hint below, under the provider names.
                                ui.horizontal(|ui| {
                                    ui.add_space(27.0);
                                    ui.colored_label(
                                        p.muted,
                                        format!(
                                            "About {} freed for your game",
                                            crate::presentation::size(s.freed_bytes)
                                        ),
                                    );
                                });
                            }
                            ui.horizontal(|ui| {
                                ui.add_space(27.0);
                                ui.add(
                                    Label::new(RichText::new(h.hint).size(17.5).color(p.muted))
                                        .wrap(),
                                );
                            });
                        },
                    );
                    if !narrow {
                        ui.vertical(|ui| {
                            ui.add_space(24.0);
                            self.primary(ui, s, h, p);
                        });
                    }
                });
                if narrow {
                    ui.add_space(10.0);
                    self.primary(ui, s, h, p);
                }
            });
    }
    fn primary(&mut self, ui: &mut Ui, s: &Shared, h: &Hero, p: Palette) {
        let a = s.controls().availability();
        let resume = s.pending || s.manual_pause;
        let enabled = if resume {
            a.restore || a.resume
        } else {
            a.pause
        };
        // Work in progress keeps the state color and takes no input.
        let busy = h.look == Look::Loading && !enabled;
        let (label, glyph) = if busy {
            ("Loading...", Icon::Dots)
        } else if resume {
            (
                crate::restore_dialog::resume_label(
                    !s.active_games.is_empty(),
                    s.coexistence,
                    s.pending,
                ),
                Icon::Play,
            )
        } else {
            ("Pause AI", Icon::Pause)
        };
        ui.add_enabled_ui(enabled || busy, |ui| {
            let response = Push::new(label)
                .icon(glyph)
                .emphasis(if busy {
                    Emphasis::Busy
                } else {
                    Emphasis::Solid
                })
                .min(design::HERO_ACTION)
                .size(21.0)
                .show(ui, p)
                .on_hover_text(a.reason);
            if !busy && response.clicked() {
                if resume {
                    self.resume(s);
                } else {
                    crate::app::request_core(&self.shared, &self.tx, CoreCommand::Pause);
                }
            }
        });
    }
    fn settings_strip(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        p.card()
            .inner_margin(Margin::symmetric(22, 8))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("Settings")
                            .size(18.0)
                            .family(design::heading()),
                    );
                    ui.separator();
                    let mut auto = s.config.automation_enabled;
                    let label = if ui.ctx().content_rect().width() < 700.0 {
                        "Pause AI while gaming"
                    } else {
                        "Automatically pause AI while gaming"
                    };
                    if ui
                        .add_enabled(
                            !s.commands.settings_pending,
                            Checkbox::new(&mut auto, label),
                        )
                        .changed()
                    {
                        let mut c = s.config.clone();
                        c.automation_enabled = auto;
                        self.save(c, false);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let visible = s.config.advanced_settings_visible;
                        let push = if visible {
                            Push::new("Back to games")
                        } else {
                            Push::new("Advanced").trailing(Icon::Caret)
                        };
                        let clicked = ui
                            .add_enabled_ui(!s.commands.settings_pending, |ui| {
                                push.min(vec2(118.0, 38.0)).show(ui, p).clicked()
                            })
                            .inner;
                        if clicked {
                            self.action(
                                Action::AdvancedVisibility(!visible),
                                "Advanced visibility",
                            );
                        }
                    });
                });
            });
    }
    fn navigation(&mut self, ui: &mut Ui, p: Palette) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (page, label, icon) in [
                (Page::Games, "Games", Icon::Game),
                (Page::Running, "Running apps", Icon::Apps),
                (Page::Ignored, "Ignored", Icon::Ignore),
            ] {
                if design::tab(ui, label, icon, self.page == page, p).clicked() {
                    self.set_page(page);
                }
            }
        });
    }
    fn games(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        const HEADER: f32 = 36.0;
        let spacing = ui.spacing().item_spacing.y;
        // Tabs sit directly on the panel they switch.
        ui.spacing_mut().item_spacing.y = 0.0;
        self.navigation(ui, p);
        let panel = p.card().inner_margin(12).corner_radius(CornerRadius {
            nw: 0,
            ne: design::RADIUS,
            sw: design::RADIUS,
            se: design::RADIUS,
        });
        panel.show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = spacing;
            ui.set_min_width(ui.available_width());
            let page = self.page;
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 162.0 - design::GAP).max(130.0);
                Frame::new()
                    .fill(p.background)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(design::RADIUS)
                    .inner_margin(Margin::symmetric(12, 0))
                    .show(ui, |ui| {
                        ui.set_width(width - 24.0);
                        ui.set_min_height(design::CONTROL);
                        let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                        design::icon(ui.painter(), rect, Icon::Search, p.muted);
                        ui.add(
                            TextEdit::singleline(&mut self.query)
                                .frame(false)
                                .desired_width(ui.available_width())
                                .hint_text(match page {
                                    Page::Games => "Search games...",
                                    Page::Running => "Search applications...",
                                    _ => "Search ignored...",
                                }),
                        );
                    });
                if Push::new("Add game...")
                    .icon(Icon::Add)
                    .emphasis(Emphasis::Accent)
                    .min(vec2(150.0, design::CONTROL + 2.0))
                    .show(ui, p)
                    .clicked()
                {
                    self.modal = Some(Modal::Add {
                        name: String::new(),
                        path: String::new(),
                        auto: true,
                    });
                }
            });
            ui.add_space(2.0);
            let mut entries = rows(s, page, &self.query);
            sort_rows(&mut entries, self.sort, page == Page::Running);
            // The details card is one line until a row is selected.
            let details = if self.selected.is_some() {
                178.0
            } else {
                108.0
            };
            let height = (ui.available_height() - details).clamp(140.0, 520.0);
            let wide = ui.available_width() > 740.0;
            let mut selected = self.selected.clone();
            let mut sort = self.sort;
            let mut toggled = None;
            // The row closures borrow the cache; details reuse it afterwards.
            let mut icons = std::mem::take(&mut self.icons);
            Frame::new()
                .stroke(Stroke::new(1.0_f32, p.border))
                .corner_radius(design::RADIUS)
                .inner_margin(1)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    // Row selection fills must stay inside the rounded table border.
                    let mut clip = ui.clip_rect();
                    clip.min.x = clip.min.x.max(ui.max_rect().left());
                    clip.max.x = clip.max.x.min(ui.max_rect().right());
                    ui.set_clip_rect(clip);
                    let top = ui.cursor().top();
                    let right = ui.max_rect().right();
                    let rows_clip = Rect::from_min_max(
                        pos2(ui.max_rect().left(), top + HEADER),
                        pos2(right, top + HEADER + height),
                    );
                    let row_painter = ui.painter().with_clip_rect(rows_clip);
                    ui.painter().hline(
                        ui.max_rect().x_range(),
                        top + HEADER,
                        Stroke::new(1.0_f32, p.border),
                    );
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let lines = ui.painter().clone();
                    let mut table = TableBuilder::new(ui)
                        .id_salt(("game-table", format!("{page:?}")))
                        .sense(Sense::click())
                        .cell_layout(Layout::left_to_right(Align::Center))
                        .min_scrolled_height(
                            height.min(entries.len() as f32 * design::ROW).max(96.0),
                        )
                        .max_scroll_height(height)
                        .column(Column::remainder().at_least(180.0).clip(true));
                    if wide {
                        table = table.column(Column::exact(210.0));
                    }
                    table = table.column(Column::exact(if page == Page::Running {
                        130.0
                    } else {
                        190.0
                    }));
                    let mut edges = Vec::new();
                    let mut heading = |ui: &mut Ui, column: usize, label: &str| {
                        if column > 0 {
                            edges.push(ui.max_rect().left() - ui.spacing().item_spacing.x / 2.0);
                        }
                        ui.add_space(10.0);
                        let response = ui
                            .add(
                                Label::new(
                                    RichText::new(label).size(16.0).family(design::heading()),
                                )
                                .selectable(false)
                                .sense(Sense::click()),
                            )
                            .on_hover_cursor(CursorIcon::PointingHand)
                            .on_hover_text("Sort by this column");
                        if sort.0 == column {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                            design::icon(
                                ui.painter(),
                                rect,
                                if sort.1 { Icon::Up } else { Icon::Down },
                                p.muted,
                            );
                        }
                        if response.clicked() {
                            sort = (column, sort.0 != column || !sort.1);
                        }
                    };
                    table
                        .header(HEADER, |mut header| {
                            header.col(|ui| {
                                heading(
                                    ui,
                                    0,
                                    if page == Page::Running {
                                        "Application"
                                    } else {
                                        "Game"
                                    },
                                )
                            });
                            if wide {
                                header.col(|ui| {
                                    heading(
                                        ui,
                                        1,
                                        if page == Page::Running {
                                            "Recognition"
                                        } else {
                                            "Platform"
                                        },
                                    )
                                });
                            }
                            header.col(|ui| {
                                heading(
                                    ui,
                                    2,
                                    if page == Page::Running {
                                        "PID"
                                    } else {
                                        "Auto pause"
                                    },
                                )
                            });
                        })
                        .body(|body| {
                            body.rows(design::ROW, entries.len(), |mut row| {
                                let index = row.index();
                                let entry = &entries[index];
                                let chosen = selected
                                    .as_ref()
                                    .is_some_and(|path| canonical(path) == canonical(&entry.path));
                                row.set_selected(chosen);
                                row.col(|ui| {
                                    ui.add_space(8.0);
                                    let (rect, _) =
                                        ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                                    let texture = icons.get(
                                        ui.ctx(),
                                        &entry.path,
                                        running_executable(s, &entry.path),
                                        64,
                                    );
                                    game_tile(
                                        ui,
                                        rect,
                                        texture,
                                        if page == Page::Running {
                                            Icon::Apps
                                        } else {
                                            Icon::Game
                                        },
                                        p,
                                    );
                                    ui.add(Label::new(&entry.name).truncate())
                                        .on_hover_text(format!("{}\n{}", entry.name, entry.path));
                                });
                                if wide {
                                    row.col(|ui| {
                                        ui.add_space(10.0);
                                        let (rect, _) = ui
                                            .allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                                        design::platform(ui.painter(), rect, &entry.platform, p);
                                        ui.add(
                                            Label::new(
                                                RichText::new(&entry.platform).color(p.muted),
                                            )
                                            .truncate(),
                                        );
                                    });
                                }
                                row.col(|ui| {
                                    ui.add_space(10.0);
                                    if page == Page::Running {
                                        ui.label(
                                            entry
                                                .pid
                                                .map_or_else(|| "—".into(), |pid| pid.to_string()),
                                        )
                                        .on_hover_text(
                                            "The process ID is shown for recognized games only.",
                                        );
                                    } else {
                                        let mut on = !entry.ignored;
                                        if ui
                                            .add_enabled(
                                                !s.commands.settings_pending,
                                                Checkbox::new(
                                                    &mut on,
                                                    if entry.ignored {
                                                        "Off"
                                                    } else if entry.ask {
                                                        "Ask"
                                                    } else {
                                                        "On"
                                                    },
                                                ),
                                            )
                                            .changed()
                                        {
                                            toggled = Some((entry.path.clone(), on));
                                        }
                                    }
                                });
                                if row.response().clicked() {
                                    selected = Some(entry.path.clone());
                                }
                                let response = row.response();
                                let mut rect = response.rect;
                                rect.max.x = rect.max.x.min(right - 1.0);
                                if selected
                                    .as_ref()
                                    .is_some_and(|path| canonical(path) == canonical(&entry.path))
                                {
                                    row_painter.rect_stroke(
                                        rect.shrink(1.0),
                                        4,
                                        Stroke::new(1.5_f32, p.accent),
                                        StrokeKind::Inside,
                                    );
                                } else if index + 1 < entries.len() {
                                    row_painter.hline(
                                        rect.x_range(),
                                        rect.bottom(),
                                        Stroke::new(1.0_f32, p.border.gamma_multiply(0.55)),
                                    );
                                }
                                if response.has_focus() {
                                    let step = response.ctx.input(|i| {
                                        if i.key_pressed(Key::ArrowDown) {
                                            1
                                        } else if i.key_pressed(Key::ArrowUp) {
                                            -1
                                        } else {
                                            0
                                        }
                                    });
                                    if step != 0 {
                                        let current = selected
                                            .as_ref()
                                            .and_then(|path| {
                                                entries.iter().position(|r| {
                                                    canonical(&r.path) == canonical(path)
                                                })
                                            })
                                            .unwrap_or(index);
                                        let next = (current as isize + step)
                                            .clamp(0, entries.len().saturating_sub(1) as isize)
                                            as usize;
                                        selected = Some(entries[next].path.clone());
                                    }
                                    if ui_input_arrow(&response) {
                                        selected = Some(entry.path.clone());
                                    }
                                }
                            });
                        });
                    for x in edges {
                        lines.vline(
                            x,
                            top..=ui.min_rect().bottom(),
                            Stroke::new(1.0_f32, p.border),
                        );
                    }
                });
            self.icons = icons;
            self.selected = selected;
            self.sort = sort;
            if let Some((path, on)) = toggled {
                let mut c = s.config.clone();
                set_ignored(&mut c, &path, !on);
                self.save(c, false);
            }
            if entries.is_empty() {
                ui.colored_label(
                    p.muted,
                    if !self.query.is_empty() {
                        "No matching entries. Try a different search."
                    } else {
                        match page {
                            Page::Games => {
                                "No games found yet. Add a game, or refresh the list in Advanced."
                            }
                            Page::Running => "No relevant applications are running right now.",
                            _ => "Nothing is ignored. Games you ignore appear here.",
                        }
                    },
                );
            }
            ui.add_space(4.0);
            let entry = self.selected.as_ref().and_then(|path| {
                entries
                    .iter()
                    .find(|r| canonical(&r.path) == canonical(path))
            });
            self.details(ui, s, entry, p);
        });
    }
    fn details(&mut self, ui: &mut Ui, s: &Shared, row: Option<&Row>, p: Palette) {
        p.card().inner_margin(14).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let Some(row) = row else {
                ui.colored_label(p.muted, "Select an entry to see its location and actions.");
                return;
            };
            let wide = ui.available_width() > 740.0;
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(92.0, 92.0), Sense::hover());
                let texture =
                    self.icons
                        .get(ui.ctx(), &row.path, running_executable(s, &row.path), 128);
                game_tile(
                    ui,
                    rect,
                    texture,
                    if self.page == Page::Running {
                        Icon::Apps
                    } else {
                        Icon::Game
                    },
                    p,
                );
                ui.add_space(6.0);
                let text_width = (ui.available_width() - if wide { 290.0 } else { 0.0 }).max(180.0);
                ui.allocate_ui_with_layout(
                    vec2(text_width, 92.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_min_width(text_width);
                        ui.spacing_mut().interact_size.y = 0.0;
                        ui.add(
                            Label::new(
                                RichText::new(&row.name)
                                    .size(23.0)
                                    .family(design::heading()),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&row.name);
                        ui.horizontal(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                            design::icon(ui.painter(), rect, Icon::Folder, p.muted);
                            ui.add(Label::new(RichText::new(&row.path).color(p.muted)).truncate())
                                .on_hover_text(&row.path);
                        });
                        ui.horizontal(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                            design::platform(ui.painter(), rect, &row.platform, p);
                            ui.colored_label(
                                p.muted,
                                match row.platform.as_str() {
                                    "Custom" => "Added by you".into(),
                                    "Custom exclusion" => "Ignored by you".into(),
                                    "Unrecognized" => "Not a recognized game".into(),
                                    platform => format!("Recognized from {platform}"),
                                },
                            );
                        });
                    },
                );
                if wide {
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        self.row_actions(ui, s, row, p);
                    });
                }
            });
            if !wide {
                self.row_actions(ui, s, row, p);
            }
        });
    }
    fn row_actions(&mut self, ui: &mut Ui, s: &Shared, row: &Row, p: Palette) {
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!s.commands.settings_pending, |ui| {
                let (label, icon) = if self.page == Page::Running {
                    ("Add as game", Icon::Add)
                } else if row.ignored {
                    ("Unignore", Icon::Ignore)
                } else {
                    ("Ignore", Icon::Ignore)
                };
                if Push::new(label)
                    .icon(icon)
                    .min(vec2(150.0, 44.0))
                    .show(ui, p)
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
                ui.spacing_mut().interact_size.y = 44.0;
                ui.spacing_mut().button_padding.x = 18.0;
                ui.menu_button("More...", |ui| {
                    if self.page == Page::Games
                        && ui
                            .button(if row.ask {
                                "Pause automatically"
                            } else {
                                "Ask before pausing"
                            })
                            .clicked()
                    {
                        let mut c = s.config.clone();
                        set_ask(&mut c, &row.path, !row.ask);
                        self.save(c, false);
                        ui.close();
                    }
                    if self.page == Page::Running && ui.button("Ignore executable").clicked() {
                        let mut c = s.config.clone();
                        exclude_executable(&mut c, &row.path);
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

/// Carries unsaved Advanced edits over a change to the saved settings: a value
/// the draft did not touch follows the saved settings, and one it did touch
/// stays as typed. `None` when the result is not a valid configuration.
fn rebase_draft(base: &Config, draft: &Config, saved: &Config) -> Option<Config> {
    use serde_json::Value;
    fn merge(base: &Value, draft: &Value, saved: &Value) -> Value {
        if draft == base {
            return saved.clone();
        }
        if saved == base {
            return draft.clone();
        }
        match (base, draft, saved) {
            (Value::Object(base), Value::Object(draft), Value::Object(saved)) => Value::Object(
                draft
                    .iter()
                    .map(|(key, value)| {
                        let other = |map: &serde_json::Map<String, Value>| {
                            map.get(key).cloned().unwrap_or(Value::Null)
                        };
                        (key.clone(), merge(&other(base), value, &other(saved)))
                    })
                    .collect(),
            ),
            (Value::Array(base), Value::Array(draft), Value::Array(saved))
                if base.len() == draft.len() && draft.len() == saved.len() =>
            {
                Value::Array(
                    base.iter()
                        .zip(draft)
                        .zip(saved)
                        .map(|((base, draft), saved)| merge(base, draft, saved))
                        .collect(),
                )
            }
            // Both changed the same value: the edit on screen wins.
            _ => draft.clone(),
        }
    }
    let value = |config: &Config| serde_json::to_value(config).ok();
    let merged: Config =
        serde_json::from_value(merge(&value(base)?, &value(draft)?, &value(saved)?)).ok()?;
    merged.validate().ok()?;
    Some(merged)
}
fn ui_input_arrow(response: &Response) -> bool {
    response
        .ctx
        .input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space))
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
                    (SettingsPage::Apps, "Other apps"),
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
                        ui.add_space(8.0);ui.label("Pause AI / Resume AI shortcut");
                        self.dirty |= ui.add(TextEdit::singleline(&mut self.edit_config.pause_hotkey).hint_text("None, for example Ctrl+Alt+P")).changed();
                        ui.colored_label(p.muted,"Works everywhere, including in games. Needs Ctrl, Alt or Win plus one key. Leave empty for no shortcut.");
                        self.save_bar(ui,s,p);
                        if ui.button("Keyboard help").clicked(){self.modal=Some(Modal::Help);}
                    },
                    SettingsPage::Detection=> {
                        ui.heading("Game detection");
                        ui.colored_label(p.muted,"Launcher metadata and registered paths identify games. Running apps helps you add missed executables.");
                        numeric(ui,"Process poll interval (seconds)",&mut self.edit_config.poll_seconds,&mut self.dirty);
                        self.dirty |= ui.styled_checkbox(&mut self.edit_config.suggest_unknown_games,"Point out fullscreen programs that look like games").changed();
                        ui.colored_label(p.muted,"A suggestion only: AI is never paused for a program until you add it.");
                        numeric(ui,"Discovery interval (seconds)",&mut self.edit_config.discovery_seconds,&mut self.dirty);
                        string_list(ui,"Additional game folders",&mut self.edit_config.game_roots,&mut self.dirty);
                        string_list(ui,"Steam roots",&mut self.edit_config.steam_roots,&mut self.dirty);
                        string_list(ui,"Epic manifest folders",&mut self.edit_config.epic_manifest_dirs,&mut self.dirty);
                        string_list(ui,"Excluded executable names",&mut self.edit_config.excluded_executables,&mut self.dirty);
                        if ui.button("Refresh game list").on_hover_text("Look for newly installed games now.").clicked(){self.action(Action::Refresh,"Discovery refresh");}
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
                        ui.heading("Ollama");
                        ui.label("Works when Ollama is running; nothing happens when it is not. Local models are unloaded for gaming. GGUF completion models come back with their context and the keep-alive time they had left; other local models, such as embedding models, stay unloaded. Full load options, parallelism, conversations and KV cache are not preserved.");
                        ui.label("Ollama itself keeps running. GamePause does not download models or fight later client reloads. Tested with Ollama 0.35.1 and 0.40.0.");
                        let editable=!s.provider_pending(crate::provider::Kind::Ollama);
                        ui.add_enabled_ui(editable,|ui| {
                            if let Some(crate::config::Provider::Ollama{enabled,endpoint,..})=self.edit_config.providers.iter_mut().find(|p|p.kind()==crate::provider::Kind::Ollama) {
                                self.dirty |= ui.styled_checkbox(enabled,"Pause Ollama models while gaming").changed();
                                ui.label("Loopback endpoint");self.dirty |= ui.text_edit_singleline(endpoint).changed();
                            } else {ui.label("No Ollama entry is configured.");}
                        });
                        if !editable {ui.colored_label(p.accent,"Finish pending Ollama recovery before turning it off or changing its endpoint.");}
                        self.save_bar(ui,s,p);
                        ui.hyperlink_to("Report Ollama problems or contribute fixes",concat!(env!("CARGO_PKG_REPOSITORY"),"/blob/main/CONTRIBUTING.md"));
                    },
                    SettingsPage::Apps=> {
                        ui.heading("Other AI apps");
                        ui.colored_label(p.accent,"Not yet tested with real AI tools such as llama.cpp or KoboldCpp.");
                        ui.label("Choose the program file of a local AI server, for example llama-server.exe or koboldcpp.exe. GamePause stops that exact file when a game starts and starts it again afterwards with the same command line and folder.");
                        ui.label("Work in progress is interrupted. Environment variables set by a launcher are not kept. The saved start command is encrypted for your Windows account and never shown.");
                        let editable=!s.provider_pending(crate::provider::Kind::Process);
                        if !editable {ui.colored_label(p.accent,"Finish the pending restart before changing these apps.");}
                        ui.add_enabled_ui(editable,|ui| {
                            if let Some(crate::config::Provider::Process{enabled,apps,..})=self.edit_config.providers.iter_mut().find(|p|p.kind()==crate::provider::Kind::Process) {
                                self.dirty |= ui.styled_checkbox(enabled,"Stop these apps while gaming").changed();
                                let mut remove=None;
                                for (index,app) in apps.iter_mut().enumerate() {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(RichText::new(&app.name).strong());
                                        ui.colored_label(p.muted,&app.path);
                                        self.dirty |= ui.styled_checkbox(&mut app.relaunch,"Start again after gaming").changed();
                                        if ui.button("Remove").clicked(){remove=Some(index);}
                                    });
                                }
                                if let Some(index)=remove {apps.remove(index);self.dirty=true;}
                                if apps.is_empty(){ui.colored_label(p.muted,"No app chosen. Nothing is stopped.");}
                                if ui.button("Add app...").clicked(){
                                    match browse(self.owner){
                                        Ok(Some(path))=>{
                                            let name=crate::process_session::file_name(&path).trim_end_matches(".exe").trim_end_matches(".EXE").to_owned();
                                            apps.push(crate::config::ProcessApp{name,path,relaunch:true});
                                            self.dirty=true;
                                        },
                                        Ok(None)=>(),
                                        Err(e)=>self.validation=format!("File picker: {e:#}"),
                                    }
                                }
                            } else {ui.label("No entry for other apps is configured.");}
                        });
                        self.save_bar(ui,s,p);
                    },
                    SettingsPage::Recovery=> {
                        ui.heading("Pause and recovery");
                        numeric(ui,"Restore after games exit (seconds)",&mut self.edit_config.restore_delay_seconds,&mut self.dirty);
                        numeric(ui,"Retry interval (seconds)",&mut self.edit_config.retry_seconds,&mut self.dirty);
                        ui.label("Original model settings are saved before unloading. Partial failures retain recovery until verification succeeds. Ignoring a game does not erase pending recovery checks.");
                        ui.colored_label(if s.pending {p.accent}else{p.muted},if s.pending {"Recovery is pending"}else{"No saved recovery is pending"});
                        self.save_bar(ui,s,p);
                        ui.add_space(design::GAP);
                        if ui.add_enabled(crate::ui_commands::verify_available(s),Button::new("Test round-trip...")).clicked(){self.modal=Some(Modal::Verify);}
                        ui.colored_label(p.muted,"The test captures, unloads and reloads the models LM Studio and Ollama have loaded. It requires no running games or pending recovery.");
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
    fn discard_draft(&mut self, s: &Shared) {
        self.edit_config = s.config.clone();
        self.edit_base = s.config.clone();
        self.edit_revision = s.revision;
        self.dirty = false;
        self.validation.clear();
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
                self.discard_draft(s);
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
                ui.strong("Recent worker log");
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
            Modal::Verify => "Test live pause and restore?",
            Modal::Help => "Keyboard and controls",
            Modal::Discard => "Discard unsaved settings?",
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
                Modal::Discard=> {ui.label("Advanced has edits that were not saved. Discard them and go back to games, or keep editing.");},
                Modal::Help=> {ui.label("Tab / Shift+Tab moves focus. Enter / Space activates controls. Arrow keys select table rows. Escape closes this dialog or returns to Games. F1 opens this help. Closing the dashboard keeps the tray watcher running. Quit uses the existing safe shutdown path.");},
            }
            if !self.validation.is_empty(){ui.colored_label(p.error,&self.validation);}
            ui.add_space(design::GAP);
            ui.horizontal(|ui| {
                let cancel_button=ui.button(match modal {Modal::Help=>"Close",Modal::Discard=>"Keep editing",_=>"Cancel"});
                // Cancel is first in keyboard order. Dangerous actions require explicit activation.
                if first {cancel_button.request_focus();}
                cancel=cancel_button.clicked();
                let label=match modal {Modal::Add{..}=>"Add game",Modal::Rename(..)=>"Rename",Modal::Remove(..)=>"Remove",Modal::Resume(..)=>"Resume AI",Modal::Verify=>"Test round-trip",Modal::Discard=>"Discard",Modal::Help=>""};
                if !label.is_empty(){accepted=ui.add_enabled(!s.commands.settings_pending,Button::new(label).fill(p.selected).stroke(Stroke::new(1.0_f32,p.accent))).clicked();}
            });
        });
        cancel |= response.should_close();
        if cancel {
            self.modal_active = false;
            // Closing help or returning to the edits cancels nothing.
            if !matches!(modal, Modal::Help | Modal::Discard) {
                self.validation.clear();
                crate::app::local_result(
                    &self.shared,
                    Outcome::Cancelled,
                    "Dialog cancelled; AI and preferences unchanged.",
                );
            }
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
                Modal::Discard => {
                    self.discard_draft(s);
                    self.action(Action::AdvancedVisibility(false), "Close Advanced");
                    return;
                }
                Modal::Help => return,
            }
        }
        self.modal = Some(modal);
    }
    fn draw(&mut self, ctx: &Context, s: &Shared) {
        let contrast = crate::theme::high_contrast();
        let dark = crate::theme::effective_dark(s.config.appearance);
        let hero = hero(s);
        let palette = Palette::for_mode(dark, contrast, hero.look);
        if self.theme != Some((dark, contrast, hero.look)) {
            palette.install(ctx, dark);
            self.theme = Some((dark, contrast, hero.look));
        }
        if !self.owner.is_null() && self.chrome != Some((dark, contrast)) {
            caption(self.owner, palette, dark);
            self.chrome = Some((dark, contrast));
        }
        if self.window_icon != Some(palette.accent) {
            ctx.send_viewport_cmd(ViewportCommand::Icon(Some(Arc::new(app_icon(
                palette.accent,
            )))));
            self.window_icon = Some(palette.accent);
        }
        self.icons.poll(ctx);
        if s.revision != self.edit_revision && !(self.dirty && s.commands.settings_pending) {
            if self.dirty {
                // Another save or a refresh happened: keep what was typed and
                // take everything else from the saved settings.
                match rebase_draft(&self.edit_base, &self.edit_config, &s.config) {
                    Some(merged) => {
                        self.dirty = merged != s.config;
                        self.edit_config = merged;
                    }
                    None => {
                        self.edit_config = s.config.clone();
                        self.dirty = false;
                        self.validation =
                            "Saved settings changed. Review them before editing again.".into();
                    }
                }
            } else {
                self.edit_config = s.config.clone();
            }
            self.edit_base = s.config.clone();
            self.edit_revision = s.revision;
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
            if self.page == Page::Activity || !s.config.advanced_settings_visible {
                self.set_page(Page::Games);
            } else if self.dirty {
                self.modal = Some(Modal::Discard);
            } else {
                self.action(Action::AdvancedVisibility(false), "Close Advanced");
            }
        }
        TopBottomPanel::bottom("footer")
            .frame(
                Frame::new()
                    .fill(palette.background)
                    .inner_margin(Margin::symmetric(22, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        palette.muted,
                        format!("GamePause for Local AI {}", env!("CARGO_PKG_VERSION")),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let size = vec2(126.0, 42.0);
                        if Push::new("Quit")
                            .icon(Icon::Power)
                            .min(size)
                            .show(ui, palette)
                            .clicked()
                        {
                            crate::app::request_quit(&self.shared, &self.tx);
                        }
                        if Push::new("Activity")
                            .icon(Icon::Activity)
                            .emphasis(if self.page == Page::Activity {
                                Emphasis::Accent
                            } else {
                                Emphasis::Plain
                            })
                            .min(size)
                            .show(ui, palette)
                            .clicked()
                        {
                            // A second click closes the log, like its Back button.
                            self.set_page(if self.page == Page::Activity {
                                Page::Games
                            } else {
                                Page::Activity
                            });
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
                        self.hero(ui, s, &hero, palette);
                        if !s.ask_prompt.is_empty() {
                            palette.card().inner_margin(16).show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(format!(
                                        "{} is running and set to ask. AI is still running.",
                                        s.ask_prompt.join(", ")
                                    ));
                                    if Push::new("Pause AI for this game")
                                        .icon(Icon::Pause)
                                        .min(vec2(230.0, 44.0))
                                        .show(ui, palette)
                                        .clicked()
                                    {
                                        self.action(Action::PauseForGame, "Pause AI for this game");
                                    }
                                    // The remembered answers are the game's ordinary rule.
                                    let asking = asking_paths(s);
                                    ui.add_enabled_ui(
                                        !asking.is_empty() && !s.commands.settings_pending,
                                        |ui| {
                                            if ui
                                                .button("Always pause")
                                                .on_hover_text("Pause AI automatically whenever this game runs.")
                                                .clicked()
                                            {
                                                self.save(answer_ask(&s.config, &asking, true), false);
                                            }
                                            if ui
                                                .button("Never pause")
                                                .on_hover_text("Ignore this game. Change it later under Ignored.")
                                                .clicked()
                                            {
                                                self.save(answer_ask(&s.config, &asking, false), false);
                                            }
                                        },
                                    );
                                });
                            });
                        }
                        if let Some(path) = &s.suggestion {
                            let name = crate::process_session::file_name(path);
                            palette.card().inner_margin(16).show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(format!(
                                        "{name} looks like a game GamePause does not know. Add it so AI pauses when it runs?"
                                    ));
                                    ui.add_enabled_ui(!s.commands.settings_pending, |ui| {
                                        if ui.button("Add as game").clicked() {
                                            self.modal = Some(Modal::Add {
                                                name: name
                                                    .trim_end_matches(".exe")
                                                    .trim_end_matches(".EXE")
                                                    .into(),
                                                path: path.clone(),
                                                auto: true,
                                            });
                                        }
                                        if ui.button("Not a game").clicked() {
                                            let mut c = s.config.clone();
                                            c.dismissed_suggestions.push(path.clone());
                                            self.save(c, false);
                                        }
                                    });
                                });
                            });
                        }
                        ui.add_space(4.0);
                        self.settings_strip(ui, s, palette);
                        ui.add_space(4.0);
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
fn native_window(frame: &eframe::Frame) -> Option<HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}
impl eframe::App for Dashboard {
    fn update(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        if let Some(owner) = native_window(frame)
            && owner != self.owner
        {
            self.owner = owner;
            if let Ok(mut bridge) = BRIDGE.lock()
                && let Some(bridge) = bridge.as_mut()
            {
                bridge.window = owner as isize;
            }
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
                UiRequest::Theme => {
                    self.theme = None;
                    self.chrome = None;
                }
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod preserved_behavior_tests;

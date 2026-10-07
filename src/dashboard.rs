//! Rust-native dashboard. Engine work stays on the existing worker channel.
//!
//! This file owns the `Dashboard` state, its frame loop and the page layout.
//! The children split the rest by responsibility: `bridge` runs the UI thread,
//! `view_data` and `game_rules` are pure, and `chrome`, `games`, `settings`,
//! `dialogs` and `activity` each draw one part of the window.

mod activity;
mod bridge;
mod chrome;
mod dialogs;
mod game_rules;
mod games;
mod native;
mod settings;
mod view_data;

pub use bridge::{
    close, needs_running_apps, refresh, request_resume, request_verify_modal, show, theme_changed,
};
pub use game_rules::{apply_rename, apply_save, render_verify_report};

use crate::{
    app::{Action, Shared, SharedState},
    commands::Outcome,
    config::Config,
    control::CoreCommand,
    dashboard_theme::{Emphasis, Icon, Look, Palette, Push},
};
use activity::{ActivityLog, Toast};
use bridge::{BRIDGE, RUNNING_REQUESTED, UI_VISIBLE, UiRequest};
use eframe::egui::*;
use game_rules::{answer_ask, asking_paths};
use native::{app_icon, caption, native_window};
use settings::rebase_draft;
use std::{
    path::PathBuf,
    ptr::null_mut,
    sync::{
        Arc, Mutex,
        atomic::Ordering,
        mpsc::{self, Sender},
    },
    time::{Duration, Instant},
};
use view_data::{Row, hero};
use windows_sys::Win32::Foundation::HWND;

pub fn scale(value: i32, dpi: i32) -> i32 {
    value * dpi / 96
}
pub fn is_dialog_message(_: &windows_sys::Win32::UI::WindowsAndMessaging::MSG) -> bool {
    false
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
}

impl Dashboard {
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
                                        self.action(Action::PauseForGame {
                                            games: s.active_games.iter().filter(|game| s.config.asks(&game.path)).cloned().collect(),
                                        }, "Pause AI for this game");
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

#[cfg(test)]
pub(crate) fn shared_command_ids(advanced: bool) -> Vec<i32> {
    crate::ui_commands::Command::ALL
        .into_iter()
        .filter(|c| *c != crate::ui_commands::Command::OpenDashboard && c.visible(advanced))
        .map(|c| c as i32)
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod preserved_behavior_tests;

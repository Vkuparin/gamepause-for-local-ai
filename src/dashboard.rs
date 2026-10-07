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
    tray,
};
mod activity;
mod bridge;
mod dialogs;
use activity::{ActivityLog, Toast};

mod game_rules;
mod native;
use bridge::{BRIDGE, RUNNING_REQUESTED, UI_VISIBLE, UiRequest};
pub use bridge::{
    close, needs_running_apps, refresh, request_resume, request_verify_modal, show, theme_changed,
};
use native::{app_icon, browse, caption, native_window};
mod view_data;
use game_rules::{answer_ask, asking_paths, exclude_executable, set_ask, set_ignored};
pub use game_rules::{apply_rename, apply_save, render_verify_report};
use view_data::{Hero, Row, Tone, hero, provider_status, rows, running_executable, sort_rows};

use eframe::egui::{self, *};
use egui_extras::{Column, TableBuilder};
use std::{path::PathBuf, ptr::null_mut};
use std::{
    sync::{
        Arc, Mutex,
        atomic::Ordering,
        mpsc::{self, Sender},
    },
    time::{Duration, Instant},
};
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

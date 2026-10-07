//! What frames every page: the hero card with its primary action, the settings
//! strip and the page tabs.

use super::{
    Dashboard, Page,
    view_data::{Hero, Tone, provider_status},
};
use crate::{
    app::{Action, Shared},
    control::CoreCommand,
    dashboard_theme::{self as design, Emphasis, Icon, Look, Palette, Push},
};
use eframe::egui::*;
impl Dashboard {
    pub(super) fn tone(p: Palette, t: Tone) -> Color32 {
        match t {
            Tone::Neutral => p.muted,
            Tone::Busy => p.busy,
            Tone::Success => p.success,
            Tone::Error => p.error,
        }
    }
    pub(super) fn hero(&mut self, ui: &mut Ui, s: &Shared, h: &Hero, p: Palette) {
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
    pub(super) fn primary(&mut self, ui: &mut Ui, s: &Shared, h: &Hero, p: Palette) {
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
    pub(super) fn settings_strip(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
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
                            design::Checkbox::new(&mut auto, label),
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
    pub(super) fn navigation(&mut self, ui: &mut Ui, p: Palette) {
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
}

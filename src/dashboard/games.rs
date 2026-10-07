//! The game table for the Games, Running apps and Ignored pages, the details
//! card for the selected row and the actions offered for it.

use super::{
    Dashboard, Modal, Page,
    game_rules::{exclude_executable, set_ask, set_ignored},
    view_data::{Row, rows, running_executable, sort_rows},
};
use crate::{
    app::Shared,
    dashboard_theme::{self as design, Emphasis, Icon, Palette, Push},
    discovery::canonical,
};
use eframe::egui::{self, *};
use egui_extras::{Column, TableBuilder};
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
fn ui_input_arrow(response: &Response) -> bool {
    response
        .ctx
        .input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space))
}
impl Dashboard {
    pub(super) fn games(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
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
    pub(super) fn details(&mut self, ui: &mut Ui, s: &Shared, row: Option<&Row>, p: Palette) {
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
    pub(super) fn row_actions(&mut self, ui: &mut Ui, s: &Shared, row: &Row, p: Palette) {
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

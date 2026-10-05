//! Shared design tokens and vector icons for the Rust-native dashboard.
use eframe::egui::{self, *};

pub const ORANGE: Color32 = Color32::from_rgb(255, 133, 48);
pub const GREEN: Color32 = Color32::from_rgb(61, 195, 113);
pub const RED: Color32 = Color32::from_rgb(239, 101, 101);
pub const GAP: f32 = 12.0;
pub const CONTROL: f32 = 38.0;
pub const ROW: f32 = 46.0;
pub const RADIUS: u8 = 7;
pub const HERO_FONT: f32 = 36.0;
pub const HERO_ACTION: Vec2 = Vec2::new(176.0, 62.0);
pub const PRIMARY_FILL: Color32 = Color32::from_rgb(174, 67, 11);

#[derive(Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub surface: Color32,
    pub elevated: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub selected: Color32,
    pub selected_text: Color32,
    pub accent: Color32,
    pub success: Color32,
    pub error: Color32,
}
impl Palette {
    pub fn for_mode(dark: bool, contrast: bool) -> Self {
        if contrast {
            // Follow actual Windows high-contrast colors, including custom themes.
            let system = |index| unsafe {
                let c = windows_sys::Win32::Graphics::Gdi::GetSysColor(index);
                Color32::from_rgb(c as u8, (c >> 8) as u8, (c >> 16) as u8)
            };
            use windows_sys::Win32::Graphics::Gdi::*;
            let text = system(COLOR_WINDOWTEXT);
            Self {
                background: system(COLOR_WINDOW),
                surface: system(COLOR_WINDOW),
                elevated: system(COLOR_BTNFACE),
                border: text,
                text,
                muted: text,
                selected: system(COLOR_HIGHLIGHT),
                selected_text: system(COLOR_HIGHLIGHTTEXT),
                accent: text,
                success: text,
                error: text,
            }
        } else if dark {
            Self {
                background: Color32::from_rgb(14, 18, 20),
                surface: Color32::from_rgb(20, 24, 27),
                elevated: Color32::from_rgb(29, 33, 36),
                border: Color32::from_rgb(54, 60, 64),
                text: Color32::from_rgb(239, 241, 244),
                muted: Color32::from_rgb(169, 179, 189),
                selected: Color32::from_rgb(57, 36, 25),
                selected_text: Color32::from_rgb(239, 241, 244),
                accent: ORANGE,
                success: GREEN,
                error: RED,
            }
        } else {
            Self {
                background: Color32::from_rgb(242, 244, 247),
                surface: Color32::WHITE,
                elevated: Color32::from_rgb(232, 236, 240),
                border: Color32::from_rgb(184, 193, 202),
                text: Color32::from_rgb(27, 32, 38),
                muted: Color32::from_rgb(84, 94, 107),
                selected: Color32::from_rgb(255, 230, 211),
                selected_text: Color32::from_rgb(27, 32, 38),
                accent: Color32::from_rgb(172, 71, 9),
                success: Color32::from_rgb(22, 119, 60),
                error: Color32::from_rgb(180, 37, 37),
            }
        }
    }
    pub fn card(self) -> Frame {
        Frame::new()
            .fill(self.surface)
            .stroke(Stroke::new(1.0_f32, self.border))
            .corner_radius(RADIUS)
            .inner_margin(16)
    }
    pub fn install(self, ctx: &Context, dark: bool) {
        let mut style = Style {
            visuals: if dark {
                Visuals::dark()
            } else {
                Visuals::light()
            },
            ..Default::default()
        };
        style.visuals.panel_fill = self.background;
        style.visuals.window_fill = self.surface;
        style.visuals.extreme_bg_color = self.background;
        style.visuals.faint_bg_color = self.elevated;
        style.visuals.override_text_color = Some(self.text);
        style.visuals.selection.bg_fill = self.selected;
        style.visuals.selection.stroke = Stroke::new(1.0_f32, self.selected_text);
        style.visuals.hyperlink_color = self.accent;
        style.visuals.warn_fg_color = self.accent;
        style.visuals.error_fg_color = self.error;
        style.visuals.window_corner_radius = RADIUS.into();
        style.visuals.window_stroke = Stroke::new(1.0_f32, self.border);
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.noninteractive,
        ] {
            widget.bg_fill = self.elevated;
            widget.weak_bg_fill = self.surface;
            widget.bg_stroke = Stroke::new(1.0_f32, self.border);
            widget.fg_stroke = Stroke::new(1.0_f32, self.text);
            widget.corner_radius = RADIUS.into();
        }
        for widget in [
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            widget.bg_fill = self.selected;
            widget.weak_bg_fill = self.selected;
            widget.bg_stroke = Stroke::new(1.0_f32, self.accent);
            widget.fg_stroke = Stroke::new(1.0_f32, self.text);
            widget.corner_radius = RADIUS.into();
        }
        style.spacing.item_spacing = vec2(GAP, 8.0);
        style.spacing.button_padding = vec2(14.0, 9.0);
        style.spacing.interact_size = vec2(CONTROL, CONTROL);
        style.spacing.scroll.bar_width = 8.0;
        style.animation_time = 0.0;
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(17.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(17.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(23.0));
        ctx.set_style(style);
    }
}

#[derive(Clone, Copy)]
pub enum Icon {
    Pause,
    Play,
    Game,
    Apps,
    Ignore,
    Search,
    Add,
    Folder,
    Activity,
    Power,
    Settings,
}

/// Small stroke icons use the same geometry in navigation, buttons and cards.
pub fn icon(painter: &Painter, rect: Rect, kind: Icon, color: Color32) {
    let rect = rect.shrink(rect.width() * 0.12);
    let p = |x: f32, y: f32| {
        pos2(
            rect.left() + x * rect.width(),
            rect.top() + y * rect.height(),
        )
    };
    let stroke = Stroke::new((rect.width() / 12.0).clamp(1.5, 4.0), color);
    let line = |a, b| {
        painter.line_segment([a, b], stroke);
    };
    match kind {
        Icon::Pause => {
            for x in [0.32, 0.68] {
                line(p(x, 0.18), p(x, 0.82));
            }
        }
        Icon::Play => {
            painter.add(Shape::convex_polygon(
                vec![p(0.22, 0.1), p(0.85, 0.5), p(0.22, 0.9)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Ignore => {
            painter.circle_stroke(rect.center(), rect.width() * 0.44, stroke);
            line(p(0.2, 0.8), p(0.8, 0.2));
        }
        Icon::Search => {
            painter.circle_stroke(p(0.4, 0.4), rect.width() * 0.3, stroke);
            line(p(0.63, 0.63), p(0.93, 0.93));
        }
        Icon::Add => {
            line(p(0.5, 0.1), p(0.5, 0.9));
            line(p(0.1, 0.5), p(0.9, 0.5));
        }
        Icon::Apps => {
            painter.rect_stroke(rect.shrink(1.0), 2, stroke, StrokeKind::Inside);
            line(p(0.0, 0.25), p(1.0, 0.25));
        }
        Icon::Folder => {
            painter.add(Shape::closed_line(
                vec![
                    p(0.05, 0.2),
                    p(0.4, 0.2),
                    p(0.5, 0.35),
                    p(0.95, 0.35),
                    p(0.95, 0.85),
                    p(0.05, 0.85),
                ],
                stroke,
            ));
        }
        Icon::Game => {
            painter.add(Shape::closed_line(
                vec![
                    p(0.18, 0.25),
                    p(0.82, 0.25),
                    p(1.0, 0.8),
                    p(0.75, 0.8),
                    p(0.63, 0.62),
                    p(0.37, 0.62),
                    p(0.25, 0.8),
                    p(0.0, 0.8),
                ],
                stroke,
            ));
            line(p(0.18, 0.45), p(0.4, 0.45));
            line(p(0.29, 0.34), p(0.29, 0.56));
            painter.circle_filled(p(0.72, 0.44), rect.width() * 0.05, color);
        }
        Icon::Activity => {
            for y in [0.2, 0.5, 0.8] {
                painter.circle_filled(p(0.05, y), rect.width() * 0.04, color);
                line(p(0.25, y), p(0.95, y));
            }
        }
        Icon::Power => {
            painter.circle_stroke(p(0.5, 0.55), rect.width() * 0.4, stroke);
            painter.rect_filled(
                Rect::from_min_max(p(0.35, 0.0), p(0.65, 0.4)),
                0,
                Color32::TRANSPARENT,
            );
            line(p(0.5, 0.0), p(0.5, 0.5));
        }
        Icon::Settings => {
            painter.circle_stroke(rect.center(), rect.width() * 0.32, stroke);
            painter.circle_stroke(rect.center(), rect.width() * 0.1, stroke);
            for (a, b) in [
                (p(0.5, 0.0), p(0.5, 0.18)),
                (p(0.5, 0.82), p(0.5, 1.0)),
                (p(0.0, 0.5), p(0.18, 0.5)),
                (p(0.82, 0.5), p(1.0, 0.5)),
            ] {
                line(a, b);
            }
        }
    }
}

pub fn button(ui: &mut Ui, text: &str, kind: Icon, primary: bool, palette: Palette) -> Response {
    let text = RichText::new(format!("     {text}")).color(palette.text);
    let mut button = Button::new(text).min_size(vec2(0.0, CONTROL));
    if primary {
        button = button
            .fill(palette.selected)
            .stroke(Stroke::new(1.0_f32, palette.accent));
    }
    let response = ui.add(button);
    icon(
        ui.painter(),
        Rect::from_center_size(
            pos2(response.rect.left() + 21.0, response.rect.center().y),
            vec2(20.0, 20.0),
        ),
        kind,
        if primary {
            palette.accent
        } else {
            palette.muted
        },
    );
    response
}

pub fn fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    // Use the installed Windows UI face, with bundled fonts as the fallback.
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts
            .font_data
            .insert("Segoe UI".into(), FontData::from_owned(bytes).into());
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "Segoe UI".into());
    }
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeuib.ttf") {
        fonts
            .font_data
            .insert("Segoe UI bold".into(), FontData::from_owned(bytes).into());
        fonts.families.insert(
            FontFamily::Name("heading".into()),
            vec!["Segoe UI bold".into()],
        );
    } else {
        fonts.families.insert(
            FontFamily::Name("heading".into()),
            fonts.families[&FontFamily::Proportional].clone(),
        );
    }
    ctx.set_fonts(fonts);
}

/// Keep egui's keyboard, labels and AccessKit metadata, with square accent boxes.
pub struct Checkbox<'a> {
    checked: &'a mut bool,
    label: &'a str,
}
impl<'a> Checkbox<'a> {
    pub fn new(checked: &'a mut bool, label: &'a str) -> Self {
        Self { checked, label }
    }
}
impl Widget for Checkbox<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let dark = ui.visuals().dark_mode;
        let contrast = crate::theme::high_contrast();
        let palette = Palette::for_mode(dark, contrast);
        ui.scope(|ui| {
            ui.spacing_mut().icon_width = 22.0;
            let style = ui.style_mut();
            for widget in [
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.active,
                &mut style.visuals.widgets.hovered,
            ] {
                widget.corner_radius = 3.into();
                if *self.checked {
                    widget.bg_fill = palette.accent;
                    widget.weak_bg_fill = palette.accent;
                    widget.fg_stroke = Stroke::new(2.0_f32, palette.background);
                }
            }
            ui.add(egui::Checkbox::new(self.checked, self.label))
        })
        .inner
    }
}
pub trait CheckboxUi {
    fn styled_checkbox(&mut self, value: &mut bool, label: &str) -> Response;
}
impl CheckboxUi for Ui {
    fn styled_checkbox(&mut self, value: &mut bool, label: &str) -> Response {
        self.add(Checkbox::new(value, label))
    }
}

//! Shared design tokens and vector icons for the Rust-native dashboard.
use eframe::egui::{self, epaint::Shadow, *};

pub const GREEN: Color32 = Color32::from_rgb(61, 195, 113);
pub const YELLOW: Color32 = Color32::from_rgb(255, 206, 48);
pub const RED: Color32 = Color32::from_rgb(244, 96, 96);
pub const GAP: f32 = 12.0;
pub const CONTROL: f32 = 40.0;
pub const ROW: f32 = 43.0;
pub const RADIUS: u8 = 7;
pub const HERO_FONT: f32 = 46.0;
pub const HERO_ACTION: Vec2 = Vec2::new(220.0, 64.0);
const PALETTE: &str = "gamepause-palette";

pub use crate::presentation::Look;

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
    pub accent_fill: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub busy: Color32,
    pub error: Color32,
    /// Native caption tint. High contrast keeps the Windows caption.
    pub caption: Option<Color32>,
    pub glow: bool,
}
impl Palette {
    pub fn for_mode(dark: bool, contrast: bool, look: Look) -> Self {
        let rgb = Color32::from_rgb;
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
                accent_fill: system(COLOR_HIGHLIGHT),
                on_accent: system(COLOR_HIGHLIGHTTEXT),
                success: text,
                busy: text,
                error: text,
                caption: None,
                glow: false,
            }
        } else if dark {
            let (accent, accent_fill, on_accent, selected) = match look {
                Look::Running => (
                    rgb(78, 160, 255),
                    rgb(38, 104, 214),
                    Color32::WHITE,
                    rgb(17, 38, 70),
                ),
                Look::Paused => (
                    rgb(255, 128, 32),
                    rgb(212, 88, 18),
                    Color32::WHITE,
                    rgb(60, 33, 16),
                ),
                Look::Loading => (YELLOW, rgb(242, 196, 32), rgb(26, 21, 4), rgb(56, 45, 12)),
                Look::Attention => (RED, rgb(196, 48, 48), Color32::WHITE, rgb(64, 23, 23)),
            };
            // The running state carries a slight navy cast, as in the mockups.
            let (background, surface, elevated, border) = if look == Look::Running {
                (
                    rgb(9, 14, 22),
                    rgb(13, 19, 28),
                    rgb(23, 30, 42),
                    rgb(42, 52, 66),
                )
            } else {
                (
                    rgb(14, 14, 15),
                    rgb(19, 20, 22),
                    rgb(30, 31, 34),
                    rgb(52, 54, 58),
                )
            };
            Self {
                background,
                surface,
                elevated,
                border,
                text: rgb(239, 241, 244),
                muted: rgb(169, 179, 189),
                selected,
                selected_text: rgb(239, 241, 244),
                accent,
                accent_fill,
                on_accent,
                success: GREEN,
                busy: YELLOW,
                error: RED,
                caption: Some(rgb(10, 48, 104)),
                glow: true,
            }
        } else {
            let (accent, accent_fill, on_accent, selected) = match look {
                Look::Running => (
                    rgb(20, 92, 200),
                    rgb(28, 100, 214),
                    Color32::WHITE,
                    rgb(220, 234, 255),
                ),
                Look::Paused => (
                    rgb(184, 74, 8),
                    rgb(206, 84, 14),
                    Color32::WHITE,
                    rgb(255, 230, 211),
                ),
                Look::Loading => (
                    rgb(130, 96, 0),
                    rgb(245, 200, 40),
                    rgb(40, 30, 0),
                    rgb(255, 244, 200),
                ),
                Look::Attention => (
                    rgb(180, 37, 37),
                    rgb(196, 48, 48),
                    Color32::WHITE,
                    rgb(255, 224, 224),
                ),
            };
            Self {
                background: rgb(242, 244, 247),
                surface: Color32::WHITE,
                elevated: rgb(232, 236, 240),
                border: rgb(184, 193, 202),
                text: rgb(27, 32, 38),
                muted: rgb(84, 94, 107),
                selected,
                selected_text: rgb(27, 32, 38),
                accent,
                accent_fill,
                on_accent,
                success: rgb(22, 119, 60),
                busy: rgb(130, 96, 0),
                error: rgb(180, 37, 37),
                caption: Some(rgb(10, 48, 104)),
                glow: false,
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
        ctx.data_mut(|data| data.insert_temp(Id::new(PALETTE), self));
    }
    fn current(ui: &Ui) -> Self {
        ui.data(|data| data.get_temp(Id::new(PALETTE)))
            .unwrap_or_else(|| {
                Self::for_mode(
                    ui.visuals().dark_mode,
                    crate::theme::high_contrast(),
                    Look::default(),
                )
            })
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}
pub fn heading() -> FontFamily {
    FontFamily::Name("heading".into())
}
pub fn hero() -> FontFamily {
    FontFamily::Name("hero".into())
}

#[derive(Clone, Copy)]
pub enum Icon {
    Pause,
    Play,
    Dots,
    Alert,
    Game,
    Apps,
    Ignore,
    Search,
    Add,
    Folder,
    Activity,
    Power,
    Caret,
    Up,
    Down,
}

/// Small icons use the same geometry in navigation, buttons and cards.
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
    let filled = |points: Vec<Pos2>| {
        painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    };
    match kind {
        Icon::Pause => {
            for x in [0.2, 0.58] {
                painter.rect_filled(
                    Rect::from_min_max(p(x, 0.12), p(x + 0.22, 0.88)),
                    rect.width() * 0.05,
                    color,
                );
            }
        }
        Icon::Play => filled(vec![p(0.22, 0.1), p(0.9, 0.5), p(0.22, 0.9)]),
        Icon::Dots => {
            for x in [0.16, 0.5, 0.84] {
                painter.circle_filled(p(x, 0.5), rect.width() * 0.1, color);
            }
        }
        Icon::Alert => {
            painter.rect_filled(
                Rect::from_min_max(p(0.41, 0.08), p(0.59, 0.64)),
                rect.width() * 0.05,
                color,
            );
            painter.circle_filled(p(0.5, 0.84), rect.width() * 0.1, color);
        }
        Icon::Caret => filled(vec![p(0.3, 0.2), p(0.75, 0.5), p(0.3, 0.8)]),
        Icon::Up => filled(vec![p(0.15, 0.72), p(0.5, 0.25), p(0.85, 0.72)]),
        Icon::Down => filled(vec![p(0.15, 0.28), p(0.85, 0.28), p(0.5, 0.75)]),
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
            line(p(0.5, 0.0), p(0.5, 0.5));
        }
    }
}

/// GamePause's own launcher marks. They are deliberately not vendor logos.
pub fn platform(painter: &Painter, rect: Rect, launcher: &str, palette: Palette) {
    painter.circle_filled(rect.center(), rect.width() / 2.0, palette.elevated);
    painter.circle_stroke(
        rect.center(),
        rect.width() / 2.0,
        Stroke::new(1.0_f32, palette.border),
    );
    let rect = rect.shrink(rect.width() * 0.27);
    let p = |x: f32, y: f32| {
        pos2(
            rect.left() + x * rect.width(),
            rect.top() + y * rect.height(),
        )
    };
    let stroke = Stroke::new((rect.width() / 8.0).clamp(1.3, 3.0), palette.text);
    let line = |points: Vec<Pos2>| {
        painter.add(Shape::line(points, stroke));
    };
    let closed = |points: Vec<Pos2>| {
        painter.add(Shape::closed_line(points, stroke));
    };
    match launcher {
        // Rising vapor.
        "Steam" => {
            for x in [0.15, 0.5, 0.85] {
                line(vec![
                    p(x, 1.0),
                    p(x - 0.13, 0.67),
                    p(x + 0.13, 0.33),
                    p(x, 0.0),
                ]);
            }
        }
        // Twin peaks.
        "Epic" => closed(vec![
            p(0.0, 0.92),
            p(0.34, 0.12),
            p(0.55, 0.58),
            p(0.7, 0.36),
            p(1.0, 0.92),
        ]),
        // A box seen from its corner.
        "Xbox" => {
            closed(vec![
                p(0.5, 0.0),
                p(1.0, 0.26),
                p(1.0, 0.74),
                p(0.5, 1.0),
                p(0.0, 0.74),
                p(0.0, 0.26),
            ]);
            line(vec![p(0.0, 0.26), p(0.5, 0.52), p(1.0, 0.26)]);
            line(vec![p(0.5, 0.52), p(0.5, 1.0)]);
        }
        // Bolt.
        "EA" => closed(vec![
            p(0.62, 0.0),
            p(0.12, 0.56),
            p(0.48, 0.56),
            p(0.36, 1.0),
            p(0.88, 0.42),
            p(0.52, 0.42),
        ]),
        // Linked rings.
        "Ubisoft" => {
            for x in [0.3, 0.7] {
                painter.circle_stroke(p(x, 0.5), rect.width() * 0.3, stroke);
            }
        }
        // Crossed blades.
        "Battle.net" => {
            line(vec![p(0.05, 0.95), p(0.95, 0.05)]);
            line(vec![p(0.05, 0.05), p(0.95, 0.95)]);
            line(vec![p(0.1, 0.6), p(0.4, 0.9)]);
            line(vec![p(0.6, 0.9), p(0.9, 0.6)]);
        }
        // Added by the user.
        "Custom" | "Custom exclusion" => {
            closed(vec![p(0.5, 0.0), p(1.0, 0.5), p(0.5, 1.0), p(0.0, 0.5)]);
            painter.circle_filled(p(0.5, 0.5), rect.width() * 0.1, palette.text);
        }
        _ => {
            painter.circle_stroke(rect.center(), rect.width() * 0.42, stroke);
            painter.circle_filled(rect.center(), rect.width() * 0.1, palette.text);
        }
    }
}

/// The large state ring. Layered strokes approximate the mockup glow.
pub fn ring(painter: &Painter, rect: Rect, kind: Icon, palette: Palette) {
    let radius = rect.width() * 0.435;
    let width = rect.width() * 0.046;
    if palette.glow {
        for step in 1..=7 {
            painter.circle_stroke(
                rect.center(),
                radius,
                Stroke::new(
                    width + step as f32 * 3.0,
                    palette.accent.gamma_multiply(0.055),
                ),
            );
        }
    }
    painter.circle_stroke(rect.center(), radius, Stroke::new(width, palette.accent));
    icon(
        painter,
        rect.shrink(rect.width() * 0.25),
        kind,
        palette.accent,
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    Plain,
    Accent,
    Solid,
    /// Solid colors without interaction, for work in progress.
    Busy,
}

/// A painted push button that keeps egui focus, keyboard activation and AccessKit roles.
pub struct Push<'a> {
    text: &'a str,
    icon: Option<Icon>,
    trailing: bool,
    emphasis: Emphasis,
    min: Vec2,
    size: f32,
}
impl<'a> Push<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            icon: None,
            trailing: false,
            emphasis: Emphasis::Plain,
            min: vec2(0.0, CONTROL),
            size: 17.0,
        }
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn trailing(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self.trailing = true;
        self
    }
    pub fn emphasis(mut self, emphasis: Emphasis) -> Self {
        self.emphasis = emphasis;
        self
    }
    pub fn min(mut self, min: Vec2) -> Self {
        self.min = min;
        self
    }
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
    pub fn show(self, ui: &mut Ui, palette: Palette) -> Response {
        let solid = matches!(self.emphasis, Emphasis::Solid | Emphasis::Busy);
        let family = if solid {
            heading()
        } else {
            FontFamily::Proportional
        };
        let galley = ui.painter().layout_no_wrap(
            self.text.to_owned(),
            FontId::new(self.size, family),
            Color32::PLACEHOLDER,
        );
        let glyph = self.size * if solid { 1.35 } else { 1.15 };
        let gap = self.size * 0.6;
        let content = galley.size().x + self.icon.map_or(0.0, |_| glyph + gap);
        let desired = vec2(
            (content + self.size * 1.7).max(self.min.x),
            self.min.y.max(galley.size().y + 12.0),
        );
        let busy = self.emphasis == Emphasis::Busy;
        let (rect, response) =
            ui.allocate_exact_size(desired, if busy { Sense::hover() } else { Sense::click() });
        let enabled = ui.is_enabled() && !busy;
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, self.text));
        if ui.is_rect_visible(rect) {
            let focus = response.has_focus();
            let hot = enabled && (response.hovered() || focus);
            let down = enabled && response.is_pointer_button_down_on();
            let (fill, edge, ink, mark) = match self.emphasis {
                Emphasis::Solid | Emphasis::Busy => (
                    if down {
                        mix(palette.accent_fill, Color32::BLACK, 0.15)
                    } else if hot {
                        mix(palette.accent_fill, Color32::WHITE, 0.12)
                    } else {
                        palette.accent_fill
                    },
                    palette.accent,
                    palette.on_accent,
                    palette.on_accent,
                ),
                Emphasis::Accent => (
                    if hot {
                        palette.selected
                    } else {
                        palette.surface
                    },
                    palette.accent,
                    palette.text,
                    palette.accent,
                ),
                Emphasis::Plain => (
                    if hot {
                        palette.elevated
                    } else {
                        palette.surface
                    },
                    if hot { palette.accent } else { palette.border },
                    palette.text,
                    palette.muted,
                ),
            };
            let painter = ui.painter();
            if solid && palette.glow && ui.is_enabled() {
                painter.add(
                    Shadow {
                        offset: [0, 0],
                        blur: 20,
                        spread: 1,
                        color: palette.accent.gamma_multiply(0.4),
                    }
                    .as_shape(rect, RADIUS),
                );
            }
            painter.rect(
                rect,
                RADIUS,
                fill,
                Stroke::new(if focus { 2.0_f32 } else { 1.0 }, edge),
                StrokeKind::Inside,
            );
            let mut x = rect.center().x - content / 2.0;
            let middle = rect.center().y;
            let mark_at = |x: f32| Rect::from_center_size(pos2(x, middle), Vec2::splat(glyph));
            if let Some(kind) = self.icon.filter(|_| !self.trailing) {
                icon(painter, mark_at(x + glyph / 2.0), kind, mark);
                x += glyph + gap;
            }
            let width = galley.size().x;
            painter.galley(pos2(x, middle - galley.size().y / 2.0), galley, ink);
            if let Some(kind) = self.icon.filter(|_| self.trailing) {
                icon(painter, mark_at(x + width + gap + glyph / 2.0), kind, ink);
            }
        }
        response
    }
}

pub fn button(ui: &mut Ui, text: &str, kind: Icon, primary: bool, palette: Palette) -> Response {
    Push::new(text)
        .icon(kind)
        .emphasis(if primary {
            Emphasis::Accent
        } else {
            Emphasis::Plain
        })
        .show(ui, palette)
}

/// A page tab joined to the panel below it.
pub fn tab(ui: &mut Ui, text: &str, kind: Icon, selected: bool, palette: Palette) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::new(
            17.0,
            if selected {
                heading()
            } else {
                FontFamily::Proportional
            },
        ),
        Color32::PLACEHOLDER,
    );
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 82.0, CONTROL + 4.0), Sense::click());
    let enabled = ui.is_enabled();
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, enabled, selected, text));
    if ui.is_rect_visible(rect) {
        let hot = response.hovered() || response.has_focus();
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius {
                nw: RADIUS,
                ne: RADIUS,
                sw: 0,
                se: 0,
            },
            if selected {
                palette.selected
            } else if hot {
                palette.elevated
            } else {
                palette.surface
            },
            Stroke::new(
                1.0_f32,
                if selected || hot {
                    palette.accent
                } else {
                    palette.border
                },
            ),
            StrokeKind::Inside,
        );
        // The bottom edge belongs to the panel, so a selected tab reads as attached.
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0_f32, palette.border),
        );
        let ink = if selected {
            palette.accent
        } else {
            palette.text
        };
        icon(
            painter,
            Rect::from_center_size(pos2(rect.left() + 34.0, rect.center().y), vec2(24.0, 24.0)),
            kind,
            if selected {
                palette.accent
            } else {
                palette.muted
            },
        );
        painter.galley(
            pos2(rect.left() + 60.0, rect.center().y - galley.size().y / 2.0),
            galley,
            ink,
        );
    }
    response
}

pub fn fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    // Use the installed Windows UI faces, with bundled fonts as the fallback.
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
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    let mut previous = fallback.clone();
    for (family, name, file) in [
        (heading(), "Segoe UI bold", r"C:\Windows\Fonts\segoeuib.ttf"),
        (hero(), "Segoe UI black", r"C:\Windows\Fonts\seguibl.ttf"),
    ] {
        if let Ok(bytes) = std::fs::read(file) {
            fonts
                .font_data
                .insert(name.into(), FontData::from_owned(bytes).into());
            previous = std::iter::once(name.to_owned())
                .chain(fallback.iter().cloned())
                .collect();
        }
        // A missing black face falls back to bold, then to the regular face.
        fonts.families.insert(family, previous.clone());
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
        let palette = Palette::current(ui);
        ui.scope(|ui| {
            ui.spacing_mut().icon_width = 24.0;
            let style = ui.style_mut();
            for widget in [
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.active,
                &mut style.visuals.widgets.hovered,
            ] {
                widget.corner_radius = 4.into();
                if *self.checked {
                    widget.bg_fill = palette.accent_fill;
                    widget.weak_bg_fill = palette.accent_fill;
                    widget.bg_stroke = Stroke::new(1.0_f32, palette.accent);
                    widget.fg_stroke = Stroke::new(2.5_f32, palette.on_accent);
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

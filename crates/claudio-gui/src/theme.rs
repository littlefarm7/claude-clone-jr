//! Rounded, modern look for the desktop app (light and dark).

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, RichText, Shadow, Stroke, TextStyle,
    Theme, Vec2,
};

/// Colors that depend on the light/dark theme.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub sidebar: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub user_bubble: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
}

pub const ACCENT: Color32 = Color32::from_rgb(124, 92, 255);

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: Color32::from_rgb(24, 24, 31),
            sidebar: Color32::from_rgb(18, 18, 24),
            surface: Color32::from_rgb(36, 36, 46),
            surface_hover: Color32::from_rgb(46, 46, 58),
            border: Color32::from_rgb(62, 62, 80),
            text: Color32::from_rgb(236, 236, 244),
            muted: Color32::from_rgb(150, 150, 168),
            accent: ACCENT,
            accent_soft: Color32::from_rgb(52, 44, 92),
            on_accent: Color32::WHITE,
            user_bubble: Color32::from_rgb(66, 52, 140),
            success: Color32::from_rgb(74, 210, 140),
            warning: Color32::from_rgb(245, 190, 80),
            danger: Color32::from_rgb(250, 110, 110),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(250, 250, 253),
            sidebar: Color32::from_rgb(241, 240, 248),
            surface: Color32::WHITE,
            surface_hover: Color32::from_rgb(236, 234, 248),
            border: Color32::from_rgb(222, 220, 234),
            text: Color32::from_rgb(28, 28, 36),
            muted: Color32::from_rgb(110, 110, 128),
            accent: Color32::from_rgb(108, 76, 240),
            accent_soft: Color32::from_rgb(236, 231, 255),
            on_accent: Color32::WHITE,
            user_bubble: Color32::from_rgb(108, 76, 240),
            success: Color32::from_rgb(22, 150, 90),
            warning: Color32::from_rgb(190, 120, 0),
            danger: Color32::from_rgb(210, 50, 50),
        }
    }
}

pub fn current(ctx: &egui::Context) -> Palette {
    palette(ctx.style().visuals.dark_mode)
}

fn style_for(style: &mut egui::Style, dark: bool) {
    let p = palette(dark);
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let r = CornerRadius::same(10);
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.window_corner_radius = CornerRadius::same(18);
    v.menu_corner_radius = CornerRadius::same(14);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 40 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 90 } else { 30 }),
    };
    v.extreme_bg_color = if dark {
        Color32::from_rgb(30, 30, 39)
    } else {
        Color32::from_rgb(246, 245, 251)
    };
    v.faint_bg_color = p.surface;
    v.code_bg_color = p.surface_hover;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    v.override_text_color = Some(p.text);
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;

    let w = &mut v.widgets;
    for (wv, fill) in [
        (&mut w.noninteractive, p.surface),
        (&mut w.inactive, p.surface_hover),
        (&mut w.hovered, p.border),
        (&mut w.active, p.accent_soft),
        (&mut w.open, p.surface_hover),
    ] {
        wv.corner_radius = r;
        wv.bg_fill = fill;
        wv.weak_bg_fill = fill;
        wv.expansion = 0.0;
    }
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.hovered.fg_stroke = Stroke::new(1.5_f32, p.text);
    w.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.active.fg_stroke = Stroke::new(1.5_f32, p.text);
    style.visuals = v;

    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(10.0, 10.0);
    s.button_padding = Vec2::new(14.0, 7.0);
    s.window_margin = Margin::same(20);
    s.menu_margin = Margin::same(10);
    s.interact_size = Vec2::new(40.0, 30.0);
    s.combo_width = 160.0;
    s.text_edit_width = 320.0;
    s.slider_width = 200.0;

    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(24.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(15.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(12.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();
}

/// Install the theme. `pref` is "light", "dark" or anything else (= follow the system).
pub fn apply(ctx: &egui::Context, pref: &str) {
    ctx.style_mut_of(Theme::Dark, |s| style_for(s, true));
    ctx.style_mut_of(Theme::Light, |s| style_for(s, false));
    ctx.set_theme(match pref {
        "light" => egui::ThemePreference::Light,
        "dark" => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    });
}

/// A rounded card.
pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let p = current(ui.ctx());
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(16)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| add(ui))
        .response
}

/// The main call-to-action button (filled with the accent color).
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = current(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).color(p.on_accent).strong())
            .fill(p.accent)
            .corner_radius(CornerRadius::same(22))
            .min_size(Vec2::new(0.0, 40.0)),
    )
}

/// A soft, pill-shaped secondary button.
pub fn pill_button(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(
        egui::Button::new(text)
            .corner_radius(CornerRadius::same(22))
            .min_size(Vec2::new(0.0, 34.0)),
    )
}

/// A big selectable option card (used by the setup assistant).
pub fn choice_card(
    ui: &mut egui::Ui,
    width: f32,
    icon: &str,
    title: &str,
    subtitle: &str,
    selected: bool,
) -> egui::Response {
    let p = current(ui.ctx());
    let frame = egui::Frame::new()
        .fill(if selected { p.accent_soft } else { p.surface })
        .stroke(Stroke::new(
            1.5_f32,
            if selected { p.accent } else { p.border },
        ))
        .corner_radius(18)
        .inner_margin(Margin::same(if icon.is_empty() { 12 } else { 18 }));
    let pad = if icon.is_empty() { 24.0 } else { 36.0 };
    let r = frame
        .show(ui, |ui| {
            ui.set_width(width - pad);
            ui.horizontal_top(|ui| {
                if !icon.is_empty() {
                    ui.label(RichText::new(icon).size(30.0));
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).size(17.0).strong());
                    ui.add(egui::Label::new(RichText::new(subtitle).color(p.muted)).wrap());
                });
            });
        })
        .response;
    let r = ui.interact(r.rect, r.id.with("choice"), egui::Sense::click());
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    r
}

/// Frame for modal dialogs (setup assistant, permissions).
pub fn modal_frame(ctx: &egui::Context) -> egui::Frame {
    let p = current(ctx);
    let dark = ctx.style().visuals.dark_mode;
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(20)
        .inner_margin(Margin::same(24))
        .shadow(Shadow {
            offset: [0, 12],
            blur: 36,
            spread: 0,
            color: Color32::from_black_alpha(if dark { 120 } else { 50 }),
        })
}

/// An on/off switch with a label (easier to read than a checkbox).
pub fn toggle(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let p = current(ui.ctx());
    let r = ui
        .horizontal(|ui| {
            let size = Vec2::new(40.0, 22.0);
            let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
            if resp.clicked() {
                *on = !*on;
                resp.mark_changed();
            }
            let t = ui.ctx().animate_bool_responsive(resp.id, *on);
            let fill = if *on { p.accent } else { p.border };
            ui.painter().rect_filled(rect, CornerRadius::same(11), fill);
            let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
            ui.painter()
                .circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
            let l = ui.add(egui::Label::new(label).sense(egui::Sense::click()));
            if l.clicked() {
                *on = !*on;
                resp.mark_changed();
            }
            resp
        })
        .inner;
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    r
}

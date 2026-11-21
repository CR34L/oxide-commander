//! Look of the app: Light, Dark and a retro Norton Commander style.

use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, Visuals};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Theme { Light, Dark, Norton }

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::Light, Theme::Dark, Theme::Norton];
    pub fn label(self) -> &'static str {
        match self { Theme::Light => "Light", Theme::Dark => "Dark", Theme::Norton => "Norton (retro)" }
    }
    pub fn key(self) -> &'static str {
        match self { Theme::Light => "light", Theme::Dark => "dark", Theme::Norton => "norton" }
    }
    pub fn from_key(s: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.key() == s)
    }
}

/// Colours used by the custom-drawn parts (lists, sidebar, bars).
pub struct Palette {
    pub app_bg: Color32,
    pub sidebar_bg: Color32,
    pub sidebar_text: Color32,
    pub sidebar_current: Color32,
    pub sidebar_hover: Color32,
    pub list_bg: Color32,
    pub stripe: Color32,
    pub hover: Color32,
    pub header_bg: Color32,
    pub header_active: Color32,
    pub text: Color32,
    pub folder_text: Color32,
    pub text_dim: Color32,
    pub text_hidden: Color32,
    pub line: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub accent_soft: Color32,
    pub selected_inactive: Color32,
    /// Norton style: marked files get yellow text instead of a filled row.
    pub marks_as_text: bool,
    pub marked_text: Color32,
    pub error: Color32,
    pub danger: Color32,
    pub fkey_number: Color32,
    pub monospace: bool,
}

impl Palette {
    pub fn font(&self, size: f32) -> FontId {
        if self.monospace { FontId::monospace(size) } else { FontId::proportional(size) }
    }
}

const LIGHT: Palette = Palette {
    app_bg: Color32::from_rgb(228, 231, 236),
    sidebar_bg: Color32::from_rgb(236, 238, 242),
    sidebar_text: Color32::from_gray(30),
    sidebar_current: Color32::from_rgb(214, 218, 226),
    sidebar_hover: Color32::from_rgb(226, 229, 235),
    list_bg: Color32::WHITE,
    stripe: Color32::from_rgb(246, 248, 250),
    hover: Color32::from_rgb(236, 241, 250),
    header_bg: Color32::from_rgb(250, 250, 251),
    header_active: Color32::from_gray(30),
    text: Color32::from_gray(25),
    folder_text: Color32::from_gray(25),
    text_dim: Color32::from_rgb(110, 116, 126),
    text_hidden: Color32::from_gray(150),
    line: Color32::from_gray(225),
    border: Color32::from_gray(205),
    accent: Color32::from_rgb(38, 110, 230),
    accent_text: Color32::WHITE,
    accent_soft: Color32::from_rgb(208, 222, 245),
    selected_inactive: Color32::from_rgb(220, 224, 232),
    marks_as_text: false,
    marked_text: Color32::WHITE,
    error: Color32::from_rgb(200, 40, 40),
    danger: Color32::from_rgb(215, 58, 48),
    fkey_number: Color32::from_rgb(38, 110, 230),
    monospace: false,
};

const DARK: Palette = Palette {
    app_bg: Color32::from_rgb(24, 25, 27),
    sidebar_bg: Color32::from_rgb(34, 35, 38),
    sidebar_text: Color32::from_gray(220),
    sidebar_current: Color32::from_rgb(58, 61, 68),
    sidebar_hover: Color32::from_rgb(46, 48, 54),
    list_bg: Color32::from_rgb(30, 31, 34),
    stripe: Color32::from_rgb(35, 36, 40),
    hover: Color32::from_rgb(44, 48, 57),
    header_bg: Color32::from_rgb(36, 37, 41),
    header_active: Color32::from_gray(230),
    text: Color32::from_gray(226),
    folder_text: Color32::from_gray(226),
    text_dim: Color32::from_rgb(150, 155, 163),
    text_hidden: Color32::from_gray(110),
    line: Color32::from_rgb(52, 54, 59),
    border: Color32::from_rgb(62, 64, 70),
    accent: Color32::from_rgb(10, 100, 220),
    accent_text: Color32::WHITE,
    accent_soft: Color32::from_rgb(36, 64, 110),
    selected_inactive: Color32::from_rgb(64, 67, 75),
    marks_as_text: false,
    marked_text: Color32::WHITE,
    error: Color32::from_rgb(255, 110, 100),
    danger: Color32::from_rgb(205, 60, 50),
    fkey_number: Color32::from_rgb(90, 160, 255),
    monospace: false,
};

// The classic 16-colour text-mode palette.
const NC_BLUE: Color32 = Color32::from_rgb(0, 0, 168);
const NC_DARK_BLUE: Color32 = Color32::from_rgb(0, 0, 120);
const NC_CYAN: Color32 = Color32::from_rgb(0, 168, 168);
const NC_LIGHT_CYAN: Color32 = Color32::from_rgb(84, 252, 252);
const NC_YELLOW: Color32 = Color32::from_rgb(252, 252, 84);
const NC_WHITE: Color32 = Color32::from_rgb(252, 252, 252);

const NORTON: Palette = Palette {
    app_bg: Color32::BLACK,
    sidebar_bg: NC_DARK_BLUE,
    sidebar_text: NC_LIGHT_CYAN,
    sidebar_current: NC_CYAN,
    sidebar_hover: Color32::from_rgb(0, 0, 200),
    list_bg: NC_BLUE,
    stripe: NC_BLUE,
    hover: Color32::from_rgb(0, 0, 200),
    header_bg: NC_BLUE,
    header_active: NC_YELLOW,
    text: NC_LIGHT_CYAN,
    folder_text: NC_WHITE,
    text_dim: NC_CYAN,
    text_hidden: Color32::from_rgb(0, 130, 130),
    line: NC_CYAN,
    border: NC_CYAN,
    accent: NC_CYAN,
    accent_text: Color32::BLACK,
    accent_soft: Color32::from_rgb(0, 100, 140),
    selected_inactive: NC_BLUE,
    marks_as_text: true,
    marked_text: NC_YELLOW,
    error: NC_YELLOW,
    danger: Color32::from_rgb(168, 0, 0),
    fkey_number: NC_WHITE,
    monospace: true,
};

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn pal() -> &'static Palette {
    match CURRENT.load(Ordering::Relaxed) { 1 => &DARK, 2 => &NORTON, _ => &LIGHT }
}

/// Switch the palette and egui's own widget style (buttons, text fields, dialogs).
pub fn apply(ctx: &egui::Context, theme: Theme) {
    CURRENT.store(match theme { Theme::Light => 0, Theme::Dark => 1, Theme::Norton => 2 }, Ordering::Relaxed);
    let p = pal();
    let mut v = match theme { Theme::Light => Visuals::light(), _ => Visuals::dark() };
    v.selection.bg_fill = p.accent;
    v.panel_fill = p.sidebar_bg;
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(if p.monospace { 0 } else { 5 });
    }
    match theme {
        Theme::Light => {
            v.widgets.inactive.weak_bg_fill = Color32::WHITE;
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
        }
        Theme::Dark => {
            v.widgets.inactive.weak_bg_fill = Color32::from_rgb(48, 50, 55);
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
            v.window_fill = Color32::from_rgb(38, 39, 43);
        }
        Theme::Norton => {
            // Dialogs: blue boxes with cyan text; buttons and input lines: black on cyan.
            v.window_fill = NC_BLUE;
            v.window_stroke = Stroke::new(2.0, NC_LIGHT_CYAN);
            v.extreme_bg_color = NC_CYAN;
            v.faint_bg_color = NC_BLUE;
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, NC_LIGHT_CYAN);
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, NC_CYAN);
            for (w, fill) in [(&mut v.widgets.inactive, NC_CYAN), (&mut v.widgets.hovered, NC_LIGHT_CYAN), (&mut v.widgets.active, NC_WHITE)] {
                w.weak_bg_fill = fill;
                w.bg_fill = fill;
                w.fg_stroke = Stroke::new(1.0, Color32::BLACK);
                w.bg_stroke = Stroke::NONE;
            }
            v.selection.stroke = Stroke::new(1.0, Color32::BLACK);
            v.text_cursor.stroke = Stroke::new(2.0, Color32::BLACK);
        }
    }
    ctx.set_visuals_of(egui::Theme::Light, v.clone());
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.set_theme(if theme == Theme::Light { egui::Theme::Light } else { egui::Theme::Dark });
    ctx.all_styles_mut(|s| {
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        for (style, font) in s.text_styles.iter_mut() {
            let base = match style {
                egui::TextStyle::Small => 10.0,
                egui::TextStyle::Heading => 19.0,
                egui::TextStyle::Monospace => 13.0,
                _ => 13.5,
            };
            font.size = base;
            font.family = if p.monospace || *style == egui::TextStyle::Monospace { FontFamily::Monospace } else { FontFamily::Proportional };
        }
    });
}

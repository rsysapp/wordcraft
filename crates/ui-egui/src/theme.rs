//! Design tokens (light and dark), UI fonts and the egui style.

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, Visuals};

/// WordCraft's app colour (and its darker ink for text on light backgrounds).
pub const APP_COLOR: Color32 = Color32::from_rgb(0x3B, 0x5B, 0xDB);
pub const APP_INK: Color32 = Color32::from_rgb(0x2B, 0x47, 0xB5);

/// Every colour the UI uses; widgets never hard-code colours.
#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub dark: bool,
    pub title_bar: Color32,
    pub ribbon: Color32,
    pub tab_strip: Color32,
    pub canvas: Color32,
    pub panel: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_disabled: Color32,
    pub icon: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub checked: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub input: Color32,
    pub input_border: Color32,
    pub group_label: Color32,
    pub ruler: Color32,
    pub ruler_margin: Color32,
    pub ruler_tick: Color32,
    pub status_bar: Color32,
    pub selection: Color32,
    pub caret: Color32,
    pub page_shadow: Color32,
    pub menu: Color32,
    pub blue: Color32,
    pub red: Color32,
    pub green: Color32,
    pub orange: Color32,
}

impl Tokens {
    pub fn light() -> Self {
        Tokens {
            dark: false,
            title_bar: Color32::from_rgb(0xF0, 0xF0, 0xF0),
            ribbon: Color32::from_rgb(0xF9, 0xF9, 0xF9),
            tab_strip: Color32::from_rgb(0xF0, 0xF0, 0xF0),
            canvas: Color32::from_rgb(0xE6, 0xE6, 0xE6),
            panel: Color32::from_rgb(0xFA, 0xFA, 0xFA),
            border: Color32::from_rgb(0xE1, 0xE1, 0xE1),
            border_strong: Color32::from_rgb(0xC8, 0xC8, 0xC8),
            text: Color32::from_rgb(0x24, 0x24, 0x24),
            text_dim: Color32::from_rgb(0x61, 0x61, 0x61),
            text_disabled: Color32::from_rgb(0xA8, 0xA8, 0xA8),
            icon: Color32::from_rgb(0x42, 0x42, 0x42),
            hover: Color32::from_rgb(0xE6, 0xE6, 0xE6),
            pressed: Color32::from_rgb(0xD6, 0xD6, 0xD6),
            checked: Color32::from_rgb(0xDD, 0xE3, 0xF8),
            accent: APP_COLOR,
            accent_text: APP_INK,
            input: Color32::WHITE,
            input_border: Color32::from_rgb(0xC7, 0xC7, 0xC7),
            group_label: Color32::from_rgb(0x70, 0x70, 0x70),
            ruler: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            ruler_margin: Color32::from_rgb(0xD4, 0xD4, 0xD4),
            ruler_tick: Color32::from_rgb(0x70, 0x70, 0x70),
            status_bar: Color32::from_rgb(0xF0, 0xF0, 0xF0),
            selection: Color32::from_rgba_unmultiplied(0x3B, 0x5B, 0xDB, 0x48),
            caret: Color32::BLACK,
            page_shadow: Color32::from_rgba_unmultiplied(0, 0, 0, 0x22),
            menu: Color32::WHITE,
            blue: Color32::from_rgb(0x2F, 0x6F, 0xD0),
            red: Color32::from_rgb(0xD1, 0x3B, 0x3B),
            green: Color32::from_rgb(0x2E, 0x8B, 0x57),
            orange: Color32::from_rgb(0xE0, 0x7B, 0x1F),
        }
    }
    pub fn dark() -> Self {
        Tokens {
            dark: true,
            title_bar: Color32::from_rgb(0x1F, 0x1F, 0x1F),
            ribbon: Color32::from_rgb(0x29, 0x29, 0x29),
            tab_strip: Color32::from_rgb(0x1F, 0x1F, 0x1F),
            canvas: Color32::from_rgb(0x14, 0x14, 0x14),
            panel: Color32::from_rgb(0x26, 0x26, 0x26),
            border: Color32::from_rgb(0x3A, 0x3A, 0x3A),
            border_strong: Color32::from_rgb(0x55, 0x55, 0x55),
            text: Color32::from_rgb(0xEE, 0xEE, 0xEE),
            text_dim: Color32::from_rgb(0xB0, 0xB0, 0xB0),
            text_disabled: Color32::from_rgb(0x6A, 0x6A, 0x6A),
            icon: Color32::from_rgb(0xDD, 0xDD, 0xDD),
            hover: Color32::from_rgb(0x3D, 0x3D, 0x3D),
            pressed: Color32::from_rgb(0x4A, 0x4A, 0x4A),
            checked: Color32::from_rgb(0x2E, 0x3A, 0x66),
            accent: Color32::from_rgb(0x7A, 0x93, 0xF0),
            accent_text: Color32::from_rgb(0x9C, 0xB0, 0xF5),
            input: Color32::from_rgb(0x1E, 0x1E, 0x1E),
            input_border: Color32::from_rgb(0x58, 0x58, 0x58),
            group_label: Color32::from_rgb(0xA0, 0xA0, 0xA0),
            ruler: Color32::from_rgb(0x2E, 0x2E, 0x2E),
            ruler_margin: Color32::from_rgb(0x45, 0x45, 0x45),
            ruler_tick: Color32::from_rgb(0xA0, 0xA0, 0xA0),
            status_bar: Color32::from_rgb(0x1F, 0x1F, 0x1F),
            selection: Color32::from_rgba_unmultiplied(0x7A, 0x93, 0xF0, 0x55),
            caret: Color32::BLACK,
            page_shadow: Color32::from_rgba_unmultiplied(0, 0, 0, 0x60),
            menu: Color32::from_rgb(0x2B, 0x2B, 0x2B),
            blue: Color32::from_rgb(0x6C, 0x9B, 0xEA),
            red: Color32::from_rgb(0xEE, 0x6A, 0x6A),
            green: Color32::from_rgb(0x5B, 0xC0, 0x8A),
            orange: Color32::from_rgb(0xF0, 0xA0, 0x50),
        }
    }
    pub fn get(ctx: &egui::Context) -> Tokens {
        // embed: a host page's accent replaces the app colour.
        Tokens::resolve(ctx.global_style().visuals.dark_mode, accent_override(ctx))
    }

    /// Light or dark tokens, with a host page's accent (embed build) in place of the app colour.
    pub fn resolve(dark: bool, accent: Option<AccentOverride>) -> Tokens {
        let mut tokens = if dark { Tokens::dark() } else { Tokens::light() };
        if let Some(over) = accent {
            tokens.accent = over.accent;
            tokens.accent_text = over.accent;
            // Pages are white in both themes, so the selection and the wash are judged against white.
            let tint = over.on_white();
            tokens.selection = Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), if dark { 0x55 } else { 0x48 });
            tokens.checked = Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), CHECKED_ALPHA);
        }
        tokens
    }
}

/// Install the UI fonts (Inter, JetBrains Mono) and the craft-fonts Japanese UI face if built in.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add(&mut fonts, "Inter", include_bytes!("../../../assets/fonts/Inter-Regular.ttf"));
    add(&mut fonts, "InterMedium", include_bytes!("../../../assets/fonts/Inter-Medium.ttf"));
    add(&mut fonts, "InterSemiBold", include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"));
    add(&mut fonts, "SourceSans", include_bytes!("../../../assets/fonts/SourceSans3-Regular.ttf"));
    add(&mut fonts, "Mono", include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"));
    if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
        prop.insert(0, "Inter".into());
        prop.push("SourceSans".into());
    }
    if let Some(m) = fonts.families.get_mut(&FontFamily::Monospace) {
        m.insert(0, "Mono".into());
    }
    fonts.families.insert(FontFamily::Name("medium".into()), vec!["InterMedium".into(), "Inter".into(), "SourceSans".into()]);
    fonts.families.insert(FontFamily::Name("semibold".into()), vec!["InterSemiBold".into(), "Inter".into(), "SourceSans".into()]);
    for f in wordcraft_fonts::japanese_ui_fonts(false).into_iter().take(1) {
        fonts.font_data.insert("JpUi".into(), Arc::new(FontData::from_static(f.bytes)));
        for fam in [FontFamily::Proportional, FontFamily::Name("medium".into()), FontFamily::Name("semibold".into())] {
            if let Some(v) = fonts.families.get_mut(&fam) {
                v.push("JpUi".into());
            }
        }
    }
    // Symbols and emoji fall back to egui's defaults (kept in the families).
    ctx.set_fonts(fonts);
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}
pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

/// Apply tokens to egui's style.
pub fn apply(ctx: &egui::Context, t: &Tokens) {
    let mut v = if t.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = t.ribbon;
    v.window_fill = t.menu;
    v.extreme_bg_color = t.input;
    v.faint_bg_color = t.panel;
    v.window_stroke = Stroke::new(1.0, t.border_strong);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.selection.bg_fill = t.selection;
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.hyperlink_color = t.accent_text;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, t.text);
    v.widgets.inactive.bg_fill = t.input;
    v.widgets.inactive.weak_bg_fill = t.input;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, t.input_border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, t.text);
    v.widgets.inactive.corner_radius = CornerRadius::same(4);
    v.widgets.hovered.bg_fill = t.hover;
    v.widgets.hovered.weak_bg_fill = t.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, t.border_strong);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, t.text);
    v.widgets.hovered.corner_radius = CornerRadius::same(4);
    v.widgets.active.bg_fill = t.pressed;
    v.widgets.active.weak_bg_fill = t.pressed;
    v.widgets.active.fg_stroke = Stroke::new(1.0, t.text);
    v.widgets.active.corner_radius = CornerRadius::same(4);
    v.widgets.open.bg_fill = t.pressed;
    v.widgets.open.weak_bg_fill = t.pressed;
    v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if t.dark { 120 } else { 40 }) };
    v.window_shadow = egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(if t.dark { 140 } else { 50 }) };
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.interact_size = egui::vec2(24.0, 22.0);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.text_styles.insert(egui::TextStyle::Body, regular(12.5));
        s.text_styles.insert(egui::TextStyle::Button, regular(12.5));
        s.text_styles.insert(egui::TextStyle::Small, regular(10.5));
        s.text_styles.insert(egui::TextStyle::Heading, semibold(17.0));
        s.interaction.tooltip_delay = 0.45;
    });
}

pub fn c32(c: wordcraft_doc::Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// embed: a host page's accent colour and the text colour on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccentOverride {
    pub accent: Color32,
    pub content: Color32,
}

// embed: the one rule for an accent drawn on the white page.
impl AccentOverride {
    /// The accent as it is drawn on a white page: the accent itself, unless it is so pale that a
    /// line or a translucent wash of it would vanish on white (under 3:1); then it is mixed toward
    /// black, which is what contrasts most with white. Not toward the host's text colour: a
    /// mid-tone accent can come with white text, which would lighten it further.
    pub fn on_white(self) -> Color32 {
        darkened_until(self.accent, |colour| contrast(colour, Color32::WHITE) >= 3.0)
    }

    /// The same colour for small text on [`Self::wash`]: darkened by the same rule until it reads
    /// as normal text (4.5:1).
    pub fn on_white_text(self) -> Color32 {
        let wash = self.wash();
        darkened_until(self.on_white(), |colour| contrast(colour, wash) >= 4.5)
    }

    /// The accent's wash (`Tokens::checked`) over the white page, opaque: the fill of a selected
    /// comment balloon and the ground of the header/footer label.
    pub fn wash(self) -> Color32 {
        let tint = self.on_white();
        let over = |channel: u8| (u16::from(channel) * u16::from(CHECKED_ALPHA) + 255 * u16::from(255 - CHECKED_ALPHA) + 127) / 255;
        Color32::from_rgb(over(tint.r()) as u8, over(tint.g()) as u8, over(tint.b()) as u8)
    }
}

/// embed: the alpha of the accent wash behind checked controls and on the page.
const CHECKED_ALPHA: u8 = 0x30;

// embed: halve the channels (toward black) until `enough` holds, at most eight times.
fn darkened_until(mut colour: Color32, enough: impl Fn(Color32) -> bool) -> Color32 {
    for _ in 0..8 {
        if enough(colour) {
            break;
        }
        colour = Color32::from_rgb(colour.r() / 2, colour.g() / 2, colour.b() / 2);
    }
    colour
}

// embed: WCAG contrast ratio of two opaque colours.
fn contrast(first: Color32, second: Color32) -> f32 {
    let linear = |channel: u8| {
        let value = f32::from(channel) / 255.0;
        if value <= 0.03928 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    };
    let luminance = |colour: Color32| 0.2126 * linear(colour.r()) + 0.7152 * linear(colour.g()) + 0.0722 * linear(colour.b());
    let (high, low) = (luminance(first).max(luminance(second)), luminance(first).min(luminance(second)));
    (high + 0.05) / (low + 0.05)
}

fn accent_id() -> egui::Id {
    egui::Id::new("wordcraft.embed.accent")
}

pub fn set_accent_override(ctx: &egui::Context, accent: Option<AccentOverride>) {
    ctx.data_mut(|data| match accent {
        Some(over) => {
            data.insert_temp(accent_id(), over);
        }
        None => data.remove::<AccentOverride>(accent_id()),
    });
}

pub fn accent_override(ctx: &egui::Context) -> Option<AccentOverride> {
    ctx.data(|data| data.get_temp::<AccentOverride>(accent_id()))
}

/// Text colour on the accent (white unless a host page chose another).
pub fn accent_content(ctx: &egui::Context) -> Color32 {
    accent_override(ctx).map_or(Color32::WHITE, |over| over.content)
}

/// embed: the fill of a selected comment balloon. Balloons belong to the page, so it is a page
/// colour in every mode: the host's accent wash over white, or the light theme's checked colour.
pub fn balloon_selected_fill(ctx: &egui::Context) -> Color32 {
    accent_override(ctx).map_or(Color32::from_rgb(0xDD, 0xE3, 0xF8), AccentOverride::wash)
}

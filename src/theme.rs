use eframe::egui;
use eframe::egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Dark,
    Light,
    #[default]
    System,
}

impl ThemeMode {
    pub const ALL: [Self; 3] = [Self::Dark, Self::Light, Self::System];

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Light => "浅色",
            Self::System => "跟随系统",
        }
    }

    pub fn apply(self, ctx: &egui::Context) {
        ctx.set_theme(match self {
            Self::Dark => egui::ThemePreference::Dark,
            Self::Light => egui::ThemePreference::Light,
            Self::System => egui::ThemePreference::System,
        });
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub bg_soft: Color32,
    pub sidebar: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub card_hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_dark: Color32,
    pub orange: Color32,
    pub red: Color32,
    pub green: Color32,
    pub blue: Color32,
}

pub fn palette(ctx: &egui::Context) -> Palette {
    palette_for(ctx.theme())
}

fn palette_for(theme: egui::Theme) -> Palette {
    if theme == egui::Theme::Dark {
        Palette {
            bg: BG,
            bg_soft: BG_SOFT,
            sidebar: SIDEBAR,
            panel: PANEL,
            card: CARD,
            card_hover: CARD_HOVER,
            border: BORDER,
            text: TEXT,
            muted: MUTED,
            accent: ACCENT,
            accent_dark: ACCENT_DARK,
            orange: ORANGE,
            red: RED,
            green: GREEN,
            blue: BLUE,
        }
    } else {
        Palette {
            bg: Color32::from_rgb(242, 247, 248),
            bg_soft: Color32::from_rgb(235, 242, 244),
            sidebar: Color32::from_rgb(250, 253, 253),
            panel: Color32::WHITE,
            card: Color32::from_rgb(232, 241, 243),
            card_hover: Color32::from_rgb(218, 235, 236),
            border: Color32::from_rgb(188, 207, 213),
            text: Color32::from_rgb(24, 44, 53),
            muted: Color32::from_rgb(79, 104, 116),
            accent: Color32::from_rgb(0, 112, 100),
            accent_dark: Color32::from_rgb(167, 216, 209),
            orange: Color32::from_rgb(153, 82, 0),
            red: Color32::from_rgb(187, 40, 62),
            green: Color32::from_rgb(20, 119, 70),
            blue: Color32::from_rgb(29, 101, 173),
        }
    }
}

pub const BG: Color32 = Color32::from_rgb(8, 12, 17);
pub const BG_SOFT: Color32 = Color32::from_rgb(11, 17, 24);
pub const SIDEBAR: Color32 = Color32::from_rgb(10, 15, 21);
pub const PANEL: Color32 = Color32::from_rgb(15, 23, 31);
pub const CARD: Color32 = Color32::from_rgb(20, 30, 40);
pub const CARD_HOVER: Color32 = Color32::from_rgb(24, 37, 49);
pub const BORDER: Color32 = Color32::from_rgb(39, 56, 70);
pub const TEXT: Color32 = Color32::from_rgb(230, 239, 246);
pub const MUTED: Color32 = Color32::from_rgb(132, 153, 169);
pub const ACCENT: Color32 = Color32::from_rgb(43, 218, 195);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(21, 111, 104);
pub const ORANGE: Color32 = Color32::from_rgb(255, 179, 88);
pub const RED: Color32 = Color32::from_rgb(255, 105, 120);
pub const GREEN: Color32 = Color32::from_rgb(83, 221, 146);
pub const BLUE: Color32 = Color32::from_rgb(97, 178, 255);

pub fn setup(ctx: &egui::Context) {
    install_fonts(ctx);

    // Both styles must exist before the first OS theme event arrives.
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.set_style_of(theme, make_style(theme));
    }
}

fn make_style(theme: egui::Theme) -> egui::Style {
    let colors = palette_for(theme);
    let mut style = egui::Style::default();
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size.y = 36.0;
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.bar_inner_margin = 3.0;
    style.spacing.scroll.bar_outer_margin = 2.0;

    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(26.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.5, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );

    let mut visuals = match theme {
        egui::Theme::Dark => Visuals::dark(),
        egui::Theme::Light => Visuals::light(),
    };
    visuals.panel_fill = colors.bg;
    visuals.window_fill = colors.panel;
    visuals.extreme_bg_color = colors.bg_soft;
    visuals.faint_bg_color = colors.card;
    visuals.override_text_color = Some(colors.text);
    visuals.selection.bg_fill = if theme == egui::Theme::Dark {
        colors.accent_dark
    } else {
        Color32::from_rgb(184, 225, 219)
    };
    visuals.selection.stroke = Stroke::new(1.0, colors.accent);
    visuals.hyperlink_color = colors.accent;
    visuals.warn_fg_color = colors.orange;
    visuals.error_fg_color = colors.red;
    visuals.window_stroke = Stroke::new(1.0, colors.border);
    visuals.window_corner_radius = CornerRadius::same(16);
    visuals.menu_corner_radius = CornerRadius::same(10);
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 12],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(160),
    };

    visuals.widgets.noninteractive.bg_fill = colors.panel;
    visuals.widgets.noninteractive.weak_bg_fill = colors.panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, colors.text);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(10);

    visuals.widgets.inactive.bg_fill = colors.card;
    visuals.widgets.inactive.weak_bg_fill = colors.card;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, colors.border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, colors.text);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(10);

    visuals.widgets.hovered.bg_fill = colors.card_hover;
    visuals.widgets.hovered.weak_bg_fill = colors.card_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors.accent_dark);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, colors.text);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(10);

    visuals.widgets.active.bg_fill = colors.card_hover;
    visuals.widgets.active.weak_bg_fill = colors.card_hover;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, colors.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5, colors.text);
    visuals.widgets.active.corner_radius = CornerRadius::same(10);

    visuals.widgets.open.bg_fill = colors.card_hover;
    visuals.widgets.open.weak_bg_fill = colors.card_hover;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, colors.border);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, colors.text);
    visuals.widgets.open.corner_radius = CornerRadius::same(10);
    style.visuals = visuals;

    style
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut ui_fonts = Vec::new();

    let ui_candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\msyhbd.ttc",
            r"C:\Windows\Fonts\segoeui.ttf",
            r"C:\Windows\Fonts\bahnschrift.ttf",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/SFNS.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]
    };

    for (index, candidate) in ui_candidates.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(PathBuf::from(candidate)) {
            let name = format!("ui-{index}");
            fonts
                .font_data
                .insert(name.clone(), FontData::from_owned(bytes).into());
            ui_fonts.push(name);
        }
    }

    if !ui_fonts.is_empty() {
        let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
        for name in ui_fonts.iter().rev() {
            proportional.insert(0, name.clone());
        }
    }

    let mono_candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            r"C:\Windows\Fonts\CascadiaMono.ttf",
            r"C:\Windows\Fonts\consola.ttf",
            r"C:\Windows\Fonts\msyh.ttc",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/SFNSMono.ttf",
            "/System/Library/Fonts/Menlo.ttc",
            "/System/Library/Fonts/PingFang.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ]
    };

    let mut mono_fonts = Vec::new();
    for (index, candidate) in mono_candidates.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(PathBuf::from(candidate)) {
            let name = format!("mono-{index}");
            fonts
                .font_data
                .insert(name.clone(), FontData::from_owned(bytes).into());
            mono_fonts.push(name);
        }
    }
    if !mono_fonts.is_empty() {
        let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
        for name in mono_fonts.iter().rev() {
            monospace.insert(0, name.clone());
        }
    }

    ctx.set_fonts(fonts);
}

pub fn alpha(color: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), (alpha * 255.0) as u8)
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: Color32, b: Color32) -> f32 {
        let luminance = |color: Color32| {
            let channel = |value: u8| {
                let v = value as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        };
        let x = luminance(a);
        let y = luminance(b);
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn both_styles_keep_custom_geometry_and_readable_text() {
        let ctx = egui::Context::default();
        setup(&ctx);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let colors = palette_for(theme);
            let style = ctx.style_of(theme);
            assert_eq!(style.visuals.dark_mode, theme == egui::Theme::Dark);
            assert_eq!(style.spacing.interact_size.y, 36.0);
            assert_eq!(style.spacing.button_padding, egui::vec2(14.0, 8.0));
            assert_eq!(style.visuals.widgets.inactive.weak_bg_fill, colors.card);
            for background in [
                colors.bg,
                colors.sidebar,
                colors.panel,
                colors.card,
                colors.card_hover,
            ] {
                for foreground in [
                    colors.text,
                    colors.muted,
                    colors.accent,
                    colors.red,
                    colors.green,
                    colors.blue,
                    colors.orange,
                ] {
                    assert!(
                        contrast(foreground, background) >= 4.5,
                        "{theme:?}: {foreground:?} on {background:?}"
                    );
                }
            }
            assert!(contrast(colors.bg, colors.accent) >= 4.5);
            assert!(contrast(colors.text, style.visuals.selection.bg_fill) >= 4.5);
        }
    }

    #[test]
    fn explicit_modes_ignore_system_and_system_mode_tracks_changes() {
        let ctx = egui::Context::default();
        setup(&ctx);
        for mode in ThemeMode::ALL {
            mode.apply(&ctx);
            for system in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
                let input = egui::RawInput {
                    system_theme: Some(system),
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| {
                    let expected = match mode {
                        ThemeMode::Dark => egui::Theme::Dark,
                        ThemeMode::Light => egui::Theme::Light,
                        ThemeMode::System => system,
                    };
                    assert_eq!(ctx.theme(), expected);
                    assert_eq!(ctx.style().visuals.panel_fill, palette(ctx).bg);
                    assert_eq!(ctx.style().spacing.interact_size.y, 36.0);
                });
            }
        }
        ThemeMode::System.apply(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            assert_eq!(ctx.theme(), egui::Theme::Dark);
        });
    }
}

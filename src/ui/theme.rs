//! Colours, fonts and egui styling for the terminal look.

use std::sync::Arc;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Visuals};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeName {
    #[default]
    Green,
    Amber,
    Ice,
}

impl ThemeName {
    pub const ALL: [ThemeName; 3] = [ThemeName::Green, ThemeName::Amber, ThemeName::Ice];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "green" | "phosphor" => Some(ThemeName::Green),
            "amber" | "crt" => Some(ThemeName::Amber),
            "ice" | "blue" | "ice-blue" => Some(ThemeName::Ice),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeName::Green => "green",
            ThemeName::Amber => "amber",
            ThemeName::Ice => "ice",
        }
    }

    pub fn palette(self) -> Palette {
        let (fg, dim, bright) = match self {
            ThemeName::Green => (
                Color32::from_rgb(0x33, 0xff, 0x66),
                Color32::from_rgb(0x1a, 0x80, 0x33),
                Color32::from_rgb(0xb3, 0xff, 0xc6),
            ),
            ThemeName::Amber => (
                Color32::from_rgb(0xff, 0xb0, 0x00),
                Color32::from_rgb(0x80, 0x58, 0x00),
                Color32::from_rgb(0xff, 0xdd, 0x88),
            ),
            ThemeName::Ice => (
                Color32::from_rgb(0x66, 0xcc, 0xff),
                Color32::from_rgb(0x2a, 0x5c, 0x80),
                Color32::from_rgb(0xcc, 0xf0, 0xff),
            ),
        };
        Palette {
            bg: Color32::from_rgb(0x05, 0x08, 0x06),
            panel: Color32::from_rgb(0x0a, 0x0f, 0x0c),
            fg,
            dim,
            bright,
            danger: Color32::from_rgb(0xff, 0x33, 0x33),
            warn: Color32::from_rgb(0xff, 0xd0, 0x33),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub fg: Color32,
    pub dim: Color32,
    pub bright: Color32,
    pub danger: Color32,
    pub warn: Color32,
}

pub const FONT_SIZE: f32 = 15.0;

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn apply(ctx: &egui::Context, theme: ThemeName) {
    let p = theme.palette();
    let mut v = Visuals::dark();
    v.override_text_color = Some(p.fg);
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.panel;
    v.faint_bg_color = p.panel;
    v.selection.bg_fill = p.dim.gamma_multiply(0.55);
    v.selection.stroke.color = p.bright;
    v.hyperlink_color = p.bright;
    v.text_cursor.stroke.color = p.fg;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.bg_fill = p.panel;
        w.weak_bg_fill = p.panel;
        w.fg_stroke.color = p.fg;
        w.bg_stroke.color = p.dim;
    }
    v.widgets.hovered.bg_stroke.color = p.fg;
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);

    ctx.all_styles_mut(|s| {
        for font in s.text_styles.values_mut() {
            font.family = FontFamily::Monospace;
        }
        s.text_styles.insert(egui::TextStyle::Body, mono(FONT_SIZE));
        s.text_styles
            .insert(egui::TextStyle::Button, mono(FONT_SIZE));
        s.text_styles
            .insert(egui::TextStyle::Monospace, mono(FONT_SIZE));
    });
}

/// Installs fonts: egui's bundled Hack for everything, plus the first system
/// CJK font found as a fallback so Chinese text renders (ARCHITECTURE §3).
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Some((bytes, index)) = find_cjk_font() {
        let mut data = FontData::from_owned(bytes);
        data.index = index;
        fonts.font_data.insert("cjk".into(), Arc::new(data));
        for fam in [FontFamily::Monospace, FontFamily::Proportional] {
            fonts.families.entry(fam).or_default().push("cjk".into());
        }
    }
    // Terminal look: proportional text uses the monospace font first.
    let mono_fonts = fonts.families[&FontFamily::Monospace].clone();
    fonts.families.insert(FontFamily::Proportional, mono_fonts);
    ctx.set_fonts(fonts);
}

fn find_cjk_font() -> Option<(Vec<u8>, u32)> {
    // Traditional Chinese first: the family is in Hong Kong.
    let candidates: &[&str] = if cfg!(windows) {
        &[
            "C:\\Windows\\Fonts\\msjh.ttc",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\mingliu.ttc",
            "C:\\Windows\\Fonts\\simsun.ttc",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/STHeiti Medium.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansMonoCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ]
    };
    candidates
        .iter()
        .find_map(|p| std::fs::read(p).ok().map(|b| (b, 0)))
}

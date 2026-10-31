//! Colours, type and widget looks. An editing bay: graphite, paper-coloured text, and tungsten
//! amber kept for one meaning only, "in the cut" (selected takes, the open tab and shot).

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};

pub const BAY: Color32 = Color32::from_rgb(0x23, 0x24, 0x27);
pub const RACK: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x1f);
pub const RAISED: Color32 = Color32::from_rgb(0x2c, 0x2d, 0x31);
pub const LINE: Color32 = Color32::from_rgb(0x36, 0x37, 0x3c);
pub const PAPER: Color32 = Color32::from_rgb(0xec, 0xe8, 0xdf);
pub const GRAPHITE: Color32 = Color32::from_rgb(0xc4, 0xc1, 0xb9);
pub const DUST: Color32 = Color32::from_rgb(0x9a, 0x97, 0x8f);
pub const TUNGSTEN: Color32 = Color32::from_rgb(0xe0, 0xa8, 0x4a);
pub const FLAG: Color32 = Color32::from_rgb(0xe0, 0x6a, 0x5a);

pub const SEMIBOLD: &str = "semibold";

/// The shot number at the top of a shot, like the one chalked on a slate.
pub fn slate() -> FontId {
    FontId::new(28.0, FontFamily::Name(SEMIBOLD.into()))
}

pub fn apply(ctx: &egui::Context) {
    fonts(ctx);
    ctx.global_style_mut(|style| {
        let semibold = FontFamily::Name(SEMIBOLD.into());
        style.text_styles = [
            (TextStyle::Heading, FontId::new(17.0, semibold)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);

        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = BAY;
        v.window_fill = BAY;
        v.extreme_bg_color = RACK;
        v.faint_bg_color = Color32::from_rgb(0x28, 0x29, 0x2d);
        v.code_bg_color = RACK;
        v.weak_text_color = Some(DUST);
        v.error_fg_color = FLAG;
        v.warn_fg_color = TUNGSTEN;
        v.hyperlink_color = TUNGSTEN;
        v.selection.bg_fill = Color32::from_rgb(0x4a, 0x3c, 0x22);
        v.selection.stroke = Stroke::new(1.0, PAPER);
        let radius = CornerRadius::same(4);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BAY;
        w.noninteractive.weak_bg_fill = BAY;
        w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, GRAPHITE);
        for (state, fill, text) in [
            (&mut w.inactive, RAISED, GRAPHITE),
            (&mut w.hovered, Color32::from_rgb(0x38, 0x39, 0x3e), PAPER),
            (&mut w.active, Color32::from_rgb(0x44, 0x45, 0x4a), PAPER),
            (&mut w.open, RAISED, PAPER),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke = Stroke::new(1.0, text);
            state.corner_radius = radius;
        }
        w.noninteractive.corner_radius = radius;
    });
}

/// Noto Sans for Latin and Noto Sans Thai behind it, since egui ships no Thai and the system
/// Thai fonts vary: Ayuthaya, for one, leaves a gap after every leading vowel.
fn fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    for (name, bytes) in [
        (
            "noto",
            &include_bytes!("../../../assets/fonts/NotoSans-Regular.ttf")[..],
        ),
        (
            "noto-semibold",
            &include_bytes!("../../../assets/fonts/NotoSans-SemiBold.ttf")[..],
        ),
        (
            "noto-thai",
            &include_bytes!("../../../assets/fonts/NotoSansThai-Regular.ttf")[..],
        ),
    ] {
        defs.font_data
            .insert(name.into(), FontData::from_static(bytes).into());
    }
    let prop = defs.families.entry(FontFamily::Proportional).or_default();
    // egui's own fonts stay behind ours for symbols and emoji.
    prop.splice(0..0, ["noto".to_string(), "noto-thai".to_string()]);
    let rest = prop[2..].to_vec();
    let mut semibold = vec!["noto-semibold".to_string(), "noto-thai".to_string()];
    semibold.extend(rest);
    defs.families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold);
    defs.families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("noto-thai".into());
    ctx.set_fonts(defs);
}

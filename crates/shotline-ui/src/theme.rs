//! Colours, type and widget looks. An editing bay: warm graphite, paper-coloured text, and
//! each colour kept to one meaning. Tungsten amber is "in the cut" and buttons that spend;
//! paper is the main button that doesn't; steel blue is something running; green is done; red
//! is failed or removes.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Stroke,
    TextStyle,
};

/// The page.
pub const BAY: Color32 = Color32::from_rgb(0x1d, 0x1e, 0x21);
/// Bars and the page rail, a step darker than the page.
pub const RACK: Color32 = Color32::from_rgb(0x16, 0x17, 0x1a);
/// Cards and popups, a step lighter.
pub const CARD: Color32 = Color32::from_rgb(0x23, 0x24, 0x27);
/// Buttons, picked rows, hover.
pub const RAISED: Color32 = Color32::from_rgb(0x2c, 0x2d, 0x31);
pub const HOVER: Color32 = Color32::from_rgb(0x35, 0x36, 0x3b);
/// Behind a picture shown on its own.
pub const STAGE: Color32 = Color32::from_rgb(0x0e, 0x0e, 0x10);
/// Between parts of a page.
pub const LINE: Color32 = Color32::from_rgb(0x2b, 0x2c, 0x30);
/// Round cards, fields and buttons.
pub const EDGE: Color32 = Color32::from_rgb(0x36, 0x37, 0x3c);
pub const PAPER: Color32 = Color32::from_rgb(0xec, 0xe8, 0xdf);
pub const GRAPHITE: Color32 = Color32::from_rgb(0xc4, 0xc1, 0xb9);
pub const DUST: Color32 = Color32::from_rgb(0x9a, 0x97, 0x8f);
/// Counts and numbers beside a title, quieter than DUST.
pub const FAINT: Color32 = Color32::from_rgb(0x75, 0x72, 0x6b);
pub const TUNGSTEN: Color32 = Color32::from_rgb(0xe0, 0xa8, 0x4a);
pub const TUNGSTEN_LIT: Color32 = Color32::from_rgb(0xf0, 0xbd, 0x66);
pub const TUNGSTEN_DEEP: Color32 = Color32::from_rgb(0xc4, 0x8f, 0x36);
/// Something running.
pub const STEEL: Color32 = Color32::from_rgb(0x8a, 0xb4, 0xd6);
/// Done.
pub const GO: Color32 = Color32::from_rgb(0x8f, 0xcf, 0xa3);
/// Worth a look before going on, but not wrong.
pub const WARN: Color32 = Color32::from_rgb(0xe0, 0x91, 0x5a);
pub const FLAG: Color32 = Color32::from_rgb(0xe0, 0x6a, 0x5a);
/// Clips on the Cut page's tracks, in the blue and green editors use.
pub const VIDEO_CLIP: Color32 = Color32::from_rgb(0x3b, 0x4f, 0x6b);
pub const AUDIO_CLIP: Color32 = Color32::from_rgb(0x34, 0x5c, 0x48);

pub const SEMIBOLD: &str = "semibold";
/// Phosphor's icons, for `icon::*`.
pub const ICONS: &str = "icons";

/// Corners: controls, rows, cards, popups.
pub const R_CONTROL: u8 = 8;
pub const R_ROW: u8 = 10;
pub const R_CARD: u8 = 14;
pub const R_POPUP: u8 = 18;

/// The gap between cards, and the padding inside one.
pub const GAP: f32 = 16.0;
pub const PAD: f32 = 16.0;

/// A page's title, like a shot's name.
pub fn title() -> FontId {
    FontId::new(24.0, FontFamily::Name(SEMIBOLD.into()))
}

/// The heading of a card or a part of a page.
pub fn section() -> FontId {
    FontId::new(14.0, FontFamily::Name(SEMIBOLD.into()))
}

pub fn icon(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICONS.into()))
}

pub fn apply(ctx: &egui::Context) {
    fonts(ctx);
    ctx.global_style_mut(|style| {
        let semibold = FontFamily::Name(SEMIBOLD.into());
        style.text_styles = [
            (TextStyle::Heading, FontId::new(17.0, semibold)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.menu_margin = egui::Margin::same(6);
        // A row centres what's in it on a band this tall, but a taller widget hangs down from
        // the band's top instead, so a label beside it sits high. Fields and buttons are a
        // 13.5px line plus padding, about 29px; the band has to fit them.
        style.spacing.interact_size.y = 32.0;

        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = BAY;
        v.window_fill = CARD;
        v.window_stroke = Stroke::new(1.0, EDGE);
        v.window_corner_radius = CornerRadius::same(R_CARD);
        v.menu_corner_radius = CornerRadius::same(R_ROW);
        v.window_shadow = egui::Shadow {
            offset: [0, 12],
            blur: 32,
            spread: 0,
            color: Color32::from_black_alpha(110),
        };
        v.popup_shadow = v.window_shadow;
        v.extreme_bg_color = RACK;
        v.faint_bg_color = CARD;
        v.code_bg_color = RACK;
        v.weak_text_color = Some(DUST);
        v.error_fg_color = FLAG;
        v.warn_fg_color = WARN;
        v.hyperlink_color = STEEL;
        v.selection.bg_fill = Color32::from_rgb(0x3a, 0x3e, 0x46);
        v.selection.stroke = Stroke::new(1.0, PAPER);
        v.text_cursor.stroke = Stroke::new(2.0, PAPER);
        let radius = CornerRadius::same(R_CONTROL);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BAY;
        w.noninteractive.weak_bg_fill = BAY;
        w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, GRAPHITE);
        w.noninteractive.corner_radius = radius;
        for (state, fill, edge, text) in [
            (&mut w.inactive, RAISED, EDGE, GRAPHITE),
            (
                &mut w.hovered,
                HOVER,
                Color32::from_rgb(0x44, 0x45, 0x4a),
                PAPER,
            ),
            (
                &mut w.active,
                Color32::from_rgb(0x3f, 0x40, 0x45),
                EDGE,
                PAPER,
            ),
            (&mut w.open, HOVER, EDGE, PAPER),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, edge);
            state.fg_stroke = Stroke::new(1.0, text);
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
    });
}

/// Inter for Latin, Noto Sans Thai behind it, since egui ships no Thai and the system Thai
/// fonts vary (Ayuthaya, for one, leaves a gap after every leading vowel), and Phosphor's icons
/// behind both so they can sit in a label.
fn fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    for (name, bytes, tweak) in [
        (
            "inter",
            &include_bytes!("../../../assets/fonts/Inter-Regular.ttf")[..],
            0.0,
        ),
        (
            "inter-semibold",
            &include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf")[..],
            0.0,
        ),
        // egui centres a fallback font's line in the first font's, which puts the Thai
        // baseline above Inter's by (0.969 - 1.061 + (1.511 - 1.21) / 2) em.
        (
            "noto-thai",
            &include_bytes!("../../../assets/fonts/NotoSansThai-Regular.ttf")[..],
            0.058,
        ),
        (
            "phosphor",
            &include_bytes!("../../../assets/fonts/Phosphor.ttf")[..],
            0.0,
        ),
    ] {
        let data = FontData::from_static(bytes).tweak(FontTweak {
            y_offset_factor: tweak,
            ..FontTweak::default()
        });
        defs.font_data.insert(name.into(), data.into());
    }
    let ours = |first: &str| [first, "noto-thai", "phosphor"].map(String::from);
    let prop = defs.families.entry(FontFamily::Proportional).or_default();
    // egui's own fonts stay behind ours for symbols and emoji.
    let rest = prop.clone();
    prop.splice(0..0, ours("inter"));
    let mut semibold = ours("inter-semibold").to_vec();
    semibold.extend(rest);
    defs.families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold);
    defs.families
        .insert(FontFamily::Name(ICONS.into()), vec!["phosphor".into()]);
    defs.families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("noto-thai".into());
    ctx.set_fonts(defs);
}

//! Neutral dark theme, bundled fonts and CJK font fallback.
use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, Id, Stroke};
use std::path::PathBuf;
use std::sync::Arc;

pub const WINDOW: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
pub const BAR: Color32 = Color32::from_rgb(0x0E, 0x0E, 0x0E);
pub const SURFACE: Color32 = Color32::from_rgb(0x1D, 0x1D, 0x1D);
pub const RAISED: Color32 = Color32::from_rgb(0x23, 0x23, 0x23);
pub const HOVER: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
pub const BORDER: Color32 = Color32::from_rgb(0x2A, 0x2A, 0x2A);
pub const TEXT: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF2);
pub const TEXT_2: Color32 = Color32::from_rgb(0xA8, 0xA8, 0xA8);
pub const TEXT_3: Color32 = Color32::from_rgb(0x8A, 0x8A, 0x8A);
pub const DISABLED: Color32 = Color32::from_rgb(0x5A, 0x5A, 0x5A);

const SELECTION_GREY: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x3A);

pub const CJK_CANDIDATES: [&str; 3] = [
    r"C:\Windows\Fonts\YuGothM.ttc",
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\malgun.ttf",
];

const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
const INTER_BOLD: &[u8] = include_bytes!("../assets/fonts/Inter-Bold.ttf");
const MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// Inter SemiBold, for headings and emphasis.
pub fn semibold() -> FontFamily {
    FontFamily::Name("inter-semibold".into())
}

/// Inter Bold, for page and album titles.
pub fn bold() -> FontFamily {
    FontFamily::Name("inter-bold".into())
}

/// The candidates that exist as files, in the given order.
pub fn fallback_font_paths(candidates: &[PathBuf]) -> Vec<PathBuf> {
    candidates.iter().filter(|p| p.is_file()).cloned().collect()
}

/// The bundled families, each followed by the CJK fallbacks that could be read.
pub fn font_definitions(cjk: &[PathBuf]) -> FontDefinitions {
    let mut fonts = FontDefinitions::empty();
    let families = [
        ("inter", INTER, FontFamily::Proportional),
        ("jetbrains-mono", MONO, FontFamily::Monospace),
        ("inter-semibold", INTER_SEMIBOLD, semibold()),
        ("inter-bold", INTER_BOLD, bold()),
    ];
    for (name, bytes, family) in &families {
        fonts
            .font_data
            .insert((*name).into(), Arc::new(FontData::from_static(bytes)));
        fonts.families.insert(family.clone(), vec![(*name).into()]);
    }
    for (i, path) in cjk.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let name = format!("cjk-fallback-{i}");
        fonts
            .font_data
            .insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
        for (_, _, family) in &families {
            fonts.families.get_mut(family).unwrap().push(name.clone());
        }
    }
    fonts
}

/// WCAG contrast ratio between two opaque colours.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    fn lum(c: Color32) -> f32 {
        let ch = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }
    let (la, lb) = (lum(a), lum(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `target`, eased over `secs` from whatever this `id` showed before.
pub fn animated_color(ctx: &Context, id: Id, target: Color32, secs: f32) -> Color32 {
    let ch = |i: usize, v: u8| {
        ctx.animate_value_with_time(id.with(i), f32::from(v), secs)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(ch(0, target.r()), ch(1, target.g()), ch(2, target.b()))
}

/// Install the neutral dark visuals and the fonts.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.dark_mode = true;
    v.override_text_color = Some(TEXT);
    v.panel_fill = WINDOW;
    v.window_fill = SURFACE;
    v.extreme_bg_color = RAISED;
    v.faint_bg_color = SURFACE;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.selection.bg_fill = SELECTION_GREY;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = TEXT;
    v.text_cursor.stroke.color = Color32::WHITE;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_2);
    // Buttons and combo boxes: raised grey, lighter on hover, 6 px corners.
    let rounded = egui::CornerRadius::same(6);
    for (w, fill) in [
        (&mut v.widgets.inactive, RAISED),
        (&mut v.widgets.hovered, HOVER),
        (&mut v.widgets.active, HOVER),
        (&mut v.widgets.open, HOVER),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = rounded;
        w.bg_stroke = Stroke::new(1.0, BORDER);
        w.fg_stroke = Stroke::new(1.0, TEXT);
    }
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| s.spacing.button_padding = egui::vec2(14.0, 7.0));

    let candidates: Vec<PathBuf> = CJK_CANDIDATES.iter().map(PathBuf::from).collect();
    ctx.set_fonts(font_definitions(&fallback_font_paths(&candidates)));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_fonts_skip_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("a.ttf");
        std::fs::write(&present, b"x").unwrap();
        let missing = dir.path().join("b.ttf");
        assert_eq!(
            fallback_font_paths(&[missing, present.clone()]),
            vec![present]
        );
    }
    #[test]
    fn visuals_use_the_neutral_tokens() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let v = ctx.global_style().visuals.clone();
        assert_eq!(v.panel_fill, WINDOW);
        assert_eq!(v.override_text_color, Some(TEXT));
    }

    #[test]
    fn inter_and_mono_are_first_in_their_families() {
        let defs = font_definitions(&[]);
        assert_eq!(defs.families[&FontFamily::Proportional][0], "inter");
        assert_eq!(defs.families[&FontFamily::Monospace][0], "jetbrains-mono");
        assert_eq!(defs.families[&semibold()][0], "inter-semibold");
        assert_eq!(defs.families[&bold()][0], "inter-bold");
    }

    #[test]
    fn contrast_matches_wcag() {
        assert!((contrast(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 0.01);
        assert!(contrast(TEXT_3, WINDOW) >= 4.5);
        assert!(contrast(TEXT_2, BAR) >= 4.5);
    }
}

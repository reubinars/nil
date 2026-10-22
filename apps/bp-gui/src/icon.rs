//! The app icon for the window and the taskbar: the crossed-loops clover.

/// The 256 px icon drawn by `assets/icon/make_icon.py`.
const PNG: &[u8] = include_bytes!("../assets/icon/nil-256.png");

pub fn app_icon() -> egui::IconData {
    let image = image::load_from_memory(PNG)
        .expect("the bundled icon is a valid PNG")
        .into_rgba8();
    egui::IconData {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_256_px_rgba_with_an_opaque_tile() {
        let icon = app_icon();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        // The middle of the tile's left edge is the dark tile, fully opaque.
        let i = (128 * 256 + 40) * 4;
        assert_eq!(icon.rgba[i + 3], 255);
    }
}

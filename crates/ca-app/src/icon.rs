//! The application icon files that `cargo xtask icon` generates.

/// The window icon, 256 pixels square.
pub const WINDOW_PNG: &[u8] = include_bytes!("../../../assets/icon/compare-all-256.png");

/// The icon the executable carries, with every size up to 256 pixels.
pub const ICO: &[u8] = include_bytes!("../../../assets/icon/compare-all.ico");

/// The window icon decoded to straight red green blue alpha bytes.
///
/// A PNG that does not decode falls back to the icon drawn in code, so a bad
/// asset never stops the window from opening.
#[must_use]
pub fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(WINDOW_PNG)
        .unwrap_or_else(|_| ca_ui::icon::application_icon())
}

/// The pixel size of each image an ICO file lists, in file order.
///
/// A width byte of zero stands for 256 or more; the size is then read from the
/// PNG header of that image.
#[must_use]
pub fn ico_sizes(ico: &[u8]) -> Option<Vec<u32>> {
    let u16_at = |at: usize| Some(u16::from_le_bytes([*ico.get(at)?, *ico.get(at + 1)?]));
    let u32_at = |at: usize| {
        Some(u32::from_le_bytes([
            *ico.get(at)?,
            *ico.get(at + 1)?,
            *ico.get(at + 2)?,
            *ico.get(at + 3)?,
        ]))
    };
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return None;
    }
    let count = usize::from(u16_at(4)?);
    let mut sizes = Vec::with_capacity(count);
    for index in 0..count {
        let entry = 6 + index * 16;
        let width = u32::from(*ico.get(entry)?);
        let offset = usize::try_from(u32_at(entry + 12)?).ok()?;
        let size = if width == 0 {
            // A PNG image stores its width big endian at byte 16.
            let bytes = ico.get(offset + 16..offset + 20)?;
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            width
        };
        sizes.push(size);
    }
    Some(sizes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{ico_sizes, ICO, WINDOW_PNG};

    const PNGS: [(u32, &[u8]); 8] = [
        (
            16,
            include_bytes!("../../../assets/icon/compare-all-16.png"),
        ),
        (
            24,
            include_bytes!("../../../assets/icon/compare-all-24.png"),
        ),
        (
            32,
            include_bytes!("../../../assets/icon/compare-all-32.png"),
        ),
        (
            48,
            include_bytes!("../../../assets/icon/compare-all-48.png"),
        ),
        (
            64,
            include_bytes!("../../../assets/icon/compare-all-64.png"),
        ),
        (
            128,
            include_bytes!("../../../assets/icon/compare-all-128.png"),
        ),
        (
            256,
            include_bytes!("../../../assets/icon/compare-all-256.png"),
        ),
        (
            512,
            include_bytes!("../../../assets/icon/compare-all-512.png"),
        ),
    ];

    #[test]
    fn every_png_decodes_at_its_size() {
        for (size, bytes) in PNGS {
            let icon = eframe::icon_data::from_png_bytes(bytes).unwrap();
            assert_eq!((icon.width, icon.height), (size, size));
            assert_eq!(icon.rgba.len(), (size * size * 4) as usize);
            assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0xFF));
        }
        assert_eq!(super::window_icon().width, 256);
        assert_eq!(WINDOW_PNG, PNGS[6].1);
    }

    #[test]
    fn the_ico_holds_the_seven_sizes_up_to_256() {
        assert_eq!(ico_sizes(ICO).unwrap(), vec![16, 24, 32, 48, 64, 128, 256]);
    }

    #[test]
    fn the_svg_uses_only_the_icon_palette() {
        let svg = include_str!("../../../assets/icon/compare-all.svg");
        let palette: Vec<String> = [
            ca_ui::theme::icon::BACK_FILL,
            ca_ui::theme::icon::BACK_EDGE,
            ca_ui::theme::icon::FRONT_FILL,
            ca_ui::theme::icon::FRONT_EDGE,
            ca_ui::theme::icon::TEXT_LINE,
            ca_ui::theme::icon::LENS_RIM,
            ca_ui::theme::icon::LENS_TINT,
            ca_ui::theme::icon::HANDLE,
        ]
        .iter()
        .map(|[r, g, b, _]| format!("#{r:02X}{g:02X}{b:02X}"))
        .collect();
        let mut rest = svg;
        let mut found = 0;
        while let Some(at) = rest.find('#') {
            let color: String = rest[at..].chars().take(7).collect();
            rest = &rest[at + 1..];
            if color.len() == 7 && color[1..].chars().all(|c| c.is_ascii_hexdigit()) {
                assert!(palette.contains(&color), "{color} is not an icon color");
                found += 1;
            }
        }
        assert!(found > 0);
    }
}

//! Colors of the rasterised window icon.
//!
//! Kept as raw red green blue alpha bytes, not [`egui::Color32`], because the
//! icon is built as a byte buffer for [`egui::IconData`] rather than painted.

/// Back page fill.
pub const BACK_FILL: [u8; 4] = [0x9A, 0xA6, 0xB8, 0xFF];
/// Back page edge.
pub const BACK_EDGE: [u8; 4] = [0x4A, 0x55, 0x66, 0xFF];
/// Front page fill.
pub const FRONT_FILL: [u8; 4] = [0xF2, 0xF4, 0xF8, 0xFF];
/// Front page edge.
pub const FRONT_EDGE: [u8; 4] = [0x2A, 0x33, 0x40, 0xFF];
/// Fake text lines on both pages.
pub const TEXT_LINE: [u8; 4] = [0x5C, 0x68, 0x7A, 0xFF];
/// Rim of the magnifying glass.
pub const LENS_RIM: [u8; 4] = [0x1F, 0x4E, 0x8C, 0xFF];
/// Tint laid over what the glass enlarges.
pub const LENS_TINT: [u8; 4] = [0x3B, 0x82, 0xD6, 0x2E];
/// Handle of the magnifying glass.
pub const HANDLE: [u8; 4] = [0x2A, 0x33, 0x40, 0xFF];

//! MPEG audio frame headers and the facts a listing takes from them.

use crate::bytes::slice;
use crate::error::Result;

/// Largest number of frames scanned while looking for the first valid header.
const MAX_SYNC_SCAN: usize = 1 << 16;

/// Which MPEG version a frame header states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpegVersion {
    /// MPEG 1.
    One,
    /// MPEG 2.
    Two,
    /// MPEG 2.5.
    TwoFive,
}

impl MpegVersion {
    /// The name shown in a listing.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::One => "MPEG 1",
            Self::Two => "MPEG 2",
            Self::TwoFive => "MPEG 2.5",
        }
    }
}

/// Channel arrangement of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    /// Two independent channels mixed as one stereo pair.
    Stereo,
    /// Two channels sharing information.
    JointStereo,
    /// Two independent channels.
    DualChannel,
    /// One channel.
    Mono,
}

impl ChannelMode {
    /// The name shown in a listing.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Stereo => "Stereo",
            Self::JointStereo => "Joint stereo",
            Self::DualChannel => "Dual channel",
            Self::Mono => "Mono",
        }
    }

    /// Number of channels the mode carries.
    #[must_use]
    pub const fn count(self) -> u8 {
        match self {
            Self::Mono => 1,
            _ => 2,
        }
    }
}

/// One MPEG audio frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// MPEG version.
    pub version: MpegVersion,
    /// Layer number, 1 to 3.
    pub layer: u8,
    /// Bit rate in bits per second. Zero means the free format.
    pub bit_rate: u32,
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Channel arrangement.
    pub channel_mode: ChannelMode,
    /// True when the header claims a cyclic redundancy check follows.
    pub protected: bool,
    /// Length of the whole frame in bytes.
    pub frame_len: u32,
    /// Samples one frame carries.
    pub samples_per_frame: u32,
}

const V1_L1: [u32; 15] = [
    0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
];
const V1_L2: [u32; 15] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
];
const V1_L3: [u32; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const V2_L1: [u32; 15] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
];
const V2_L23: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// Read a frame header from four bytes.
///
/// Returns `None` when the bytes are not a valid header, so a scan can step
/// forward one byte and try again.
#[must_use]
pub fn parse_header(raw: &[u8]) -> Option<FrameHeader> {
    if raw.len() < 4 || raw[0] != 0xFF || raw[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = match (raw[1] >> 3) & 0x03 {
        0 => MpegVersion::TwoFive,
        2 => MpegVersion::Two,
        3 => MpegVersion::One,
        _ => return None,
    };
    let layer = match (raw[1] >> 1) & 0x03 {
        1 => 3u8,
        2 => 2,
        3 => 1,
        _ => return None,
    };
    let protected = raw[1] & 0x01 == 0;
    let bitrate_index = usize::from(raw[2] >> 4);
    if bitrate_index == 0 || bitrate_index >= 15 {
        return None;
    }
    let table = match (version, layer) {
        (MpegVersion::One, 1) => V1_L1,
        (MpegVersion::One, 2) => V1_L2,
        (MpegVersion::One, _) => V1_L3,
        (_, 1) => V2_L1,
        (_, _) => V2_L23,
    };
    let bit_rate = table.get(bitrate_index).copied().unwrap_or(0) * 1000;
    let rate_index = usize::from((raw[2] >> 2) & 0x03);
    let rates: [u32; 3] = match version {
        MpegVersion::One => [44100, 48000, 32000],
        MpegVersion::Two => [22050, 24000, 16000],
        MpegVersion::TwoFive => [11025, 12000, 8000],
    };
    let sample_rate = *rates.get(rate_index)?;
    let padding = u32::from((raw[2] >> 1) & 0x01);
    let channel_mode = match (raw[3] >> 6) & 0x03 {
        0 => ChannelMode::Stereo,
        1 => ChannelMode::JointStereo,
        2 => ChannelMode::DualChannel,
        _ => ChannelMode::Mono,
    };
    let samples_per_frame = match (version, layer) {
        (_, 1) => 384,
        (_, 2) | (MpegVersion::One, _) => 1152,
        (_, _) => 576,
    };
    let frame_len = if layer == 1 {
        (12 * bit_rate / sample_rate + padding) * 4
    } else {
        samples_per_frame / 8 * bit_rate / sample_rate + padding
    };
    if frame_len < 4 {
        return None;
    }
    Some(FrameHeader {
        version,
        layer,
        bit_rate,
        sample_rate,
        channel_mode,
        protected,
        frame_len,
        samples_per_frame,
    })
}

/// Facts a listing shows about an MPEG audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFacts {
    /// Header of the first valid frame.
    pub first: FrameHeader,
    /// File offset of the first valid frame.
    pub first_offset: u64,
    /// Number of frames, from a header that states one or from a count.
    pub frame_count: u64,
    /// True when the bit rate changes between frames.
    pub variable_bit_rate: bool,
    /// Average bit rate in bits per second.
    pub average_bit_rate: u32,
    /// Duration in milliseconds.
    pub duration_ms: u64,
}

/// Scan an audio range for frame headers and summarise the stream.
///
/// The scan stops at the first frame when no further frame follows, so a file
/// that is not MPEG audio costs a bounded walk rather than a whole pass.
///
/// # Errors
///
/// Returns an error only when a slice of the input is out of range, which the
/// caller's own range check already prevents.
pub fn scan(data: &[u8], start: u64, max_frames: u64) -> Result<Option<StreamFacts>> {
    let Some((offset, first)) = find_first(data) else {
        return Ok(None);
    };
    let mut position = offset;
    let mut frames = 0u64;
    let mut total_bits = 0u64;
    let mut variable = false;
    while frames < max_frames {
        let Some(raw) = data.get(position..position.saturating_add(4)) else {
            break;
        };
        let Some(header) = parse_header(raw) else {
            break;
        };
        if header.bit_rate != first.bit_rate {
            variable = true;
        }
        frames = frames.saturating_add(1);
        total_bits = total_bits.saturating_add(u64::from(header.bit_rate));
        let step = usize::try_from(header.frame_len).unwrap_or(4).max(4);
        position = position.saturating_add(step);
    }
    let xing = read_xing(data, offset, &first);
    let frame_count = xing.map_or(frames, |(count, _)| count);
    let average_bit_rate = if frames == 0 {
        first.bit_rate
    } else {
        u32::try_from(total_bits / frames).unwrap_or(first.bit_rate)
    };
    let duration_ms = if first.sample_rate == 0 {
        0
    } else {
        frame_count
            .saturating_mul(u64::from(first.samples_per_frame))
            .saturating_mul(1000)
            / u64::from(first.sample_rate)
    };
    Ok(Some(StreamFacts {
        first,
        first_offset: start.saturating_add(u64::try_from(offset).unwrap_or(0)),
        frame_count,
        variable_bit_rate: variable || xing.is_some_and(|(_, is_xing)| is_xing),
        average_bit_rate,
        duration_ms,
    }))
}

fn find_first(data: &[u8]) -> Option<(usize, FrameHeader)> {
    let limit = data.len().min(MAX_SYNC_SCAN);
    let mut index = 0usize;
    while index + 4 <= limit {
        if let Some(header) = parse_header(data.get(index..index + 4)?) {
            return Some((index, header));
        }
        index += 1;
    }
    None
}

/// Read the frame count from a `Xing` or `Info` header.
fn read_xing(data: &[u8], frame_offset: usize, header: &FrameHeader) -> Option<(u64, bool)> {
    if header.layer != 3 {
        return None;
    }
    let side_info = match (header.version, header.channel_mode) {
        (crate::media::mpeg::MpegVersion::One, ChannelMode::Mono) => 17,
        (crate::media::mpeg::MpegVersion::One, _) => 32,
        (_, ChannelMode::Mono) => 9,
        (_, _) => 17,
    };
    let crc_len = if header.protected { 2 } else { 0 };
    let tag_at = frame_offset
        .saturating_add(4)
        .saturating_add(crc_len)
        .saturating_add(side_info);
    let Ok(tag) = slice(data, tag_at, 4, "mpeg vbr header") else {
        return None;
    };
    if tag != b"Xing" && tag != b"Info" {
        return None;
    }
    let Ok(flags_raw) = slice(data, tag_at + 4, 4, "mpeg vbr header") else {
        return None;
    };
    let flags = u32::from_be_bytes([flags_raw[0], flags_raw[1], flags_raw[2], flags_raw[3]]);
    let mut cursor = tag_at + 8;
    let mut frames = 0u64;
    if flags & 0x01 != 0 {
        let Ok(raw) = slice(data, cursor, 4, "mpeg vbr header") else {
            return None;
        };
        frames = u64::from(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]));
        cursor += 4;
    }
    if flags & 0x02 != 0 && slice(data, cursor, 4, "mpeg vbr header").is_err() {
        return None;
    }
    if frames == 0 {
        return None;
    }
    Some((frames, tag == b"Xing"))
}

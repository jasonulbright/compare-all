//! Container formats other than a bare MPEG audio stream.
//!
//! Only as much of each container is read as a tag listing needs: the stream
//! facts and the metadata block. Nothing here decodes audio.

use crate::bytes::{slice, to_u64, Cursor};
use crate::error::{RecordError, Result};
use crate::limits::{Limits, RecordBudget};
use crate::media::id3::TagField;
use crate::record::ByteRange;

/// Largest number of container boxes walked at one level.
const MAX_BOXES: u64 = 65_536;
/// Largest cumulative Ogg header packet scanned for stream and tag metadata.
const MAX_OGG_HEADER_BYTES: usize = 1 << 20;

/// Facts a listing shows about an audio stream in a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContainerStream {
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u8,
    /// Bits per sample, where the container states one.
    pub bits_per_sample: u16,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Average bit rate in bits per second, where the container states one.
    pub bit_rate: u32,
}

/// What one container read produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContainerTags {
    /// Stream facts, absent when the container states none.
    pub stream: Option<ContainerStream>,
    /// Metadata fields in file order.
    pub fields: Vec<TagField>,
}

/// Read a FLAC file's stream information and comments.
///
/// # Errors
///
/// Returns [`RecordError::Malformed`] when the signature or metadata block
/// layout is invalid and [`RecordError::Truncated`] when a block runs past the
/// end of the file.
pub fn read_flac(data: &[u8], limits: &Limits) -> Result<ContainerTags> {
    if slice(data, 0, 4, "flac signature")? != b"fLaC" {
        return Err(RecordError::malformed("flac signature", 0));
    }
    let mut budget = RecordBudget::new(limits);
    let mut out = ContainerTags::default();
    let mut at = 4usize;
    let mut blocks = 0u64;
    let mut seen_vorbis_comment = false;
    let mut seen_seektable = false;
    loop {
        blocks += 1;
        if blocks > MAX_BOXES {
            return Err(RecordError::LimitExceeded {
                limit: "containerBlocks",
                allowed: MAX_BOXES,
                requested: blocks,
            });
        }
        let header = slice(data, at, 4, "flac block header")?;
        let last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7F;
        let size =
            (usize::from(header[1]) << 16) | (usize::from(header[2]) << 8) | usize::from(header[3]);
        if kind == 127 {
            return Err(RecordError::malformed(
                "flac metadata block type",
                to_u64(at),
            ));
        }
        if blocks == 1 && kind != 0 {
            return Err(RecordError::malformed(
                "flac streaminfo position",
                to_u64(at),
            ));
        }
        if kind == 0 && (blocks != 1 || size != 34) {
            return Err(RecordError::malformed("flac streaminfo block", to_u64(at)));
        }
        if kind == 4 {
            if seen_vorbis_comment {
                return Err(RecordError::malformed(
                    "flac vorbis comment block",
                    to_u64(at),
                ));
            }
            seen_vorbis_comment = true;
        }
        if kind == 3 {
            if seen_seektable {
                return Err(RecordError::malformed("flac seektable block", to_u64(at)));
            }
            seen_seektable = true;
        }
        limits.check_value(to_u64(size))?;
        let body = slice(data, at + 4, size, "flac block")?;
        match kind {
            0 => out.stream = Some(parse_flac_stream_info(body)),
            4 => parse_vorbis_comment(body, at + 4, &mut out.fields, limits, &mut budget)?,
            _ => {}
        }
        at = at.saturating_add(4).saturating_add(size);
        if last {
            break;
        }
    }
    Ok(out)
}

fn parse_flac_stream_info(body: &[u8]) -> ContainerStream {
    let sample_rate =
        (u32::from(body[10]) << 12) | (u32::from(body[11]) << 4) | (u32::from(body[12]) >> 4);
    let channels = ((body[12] >> 1) & 0x07) + 1;
    let bits = (((u16::from(body[12]) & 0x01) << 4) | (u16::from(body[13]) >> 4)) + 1;
    let total_samples = (u64::from(body[13] & 0x0F) << 32)
        | (u64::from(body[14]) << 24)
        | (u64::from(body[15]) << 16)
        | (u64::from(body[16]) << 8)
        | u64::from(body[17]);
    let duration_ms = if sample_rate == 0 {
        0
    } else {
        total_samples.saturating_mul(1000) / u64::from(sample_rate)
    };
    ContainerStream {
        sample_rate,
        channels,
        bits_per_sample: bits,
        duration_ms,
        bit_rate: 0,
    }
}

fn parse_vorbis_comment(
    body: &[u8],
    at: usize,
    fields: &mut Vec<TagField>,
    limits: &Limits,
    budget: &mut RecordBudget,
) -> Result<()> {
    let mut cursor = Cursor::new(body, "vorbis comment");
    let vendor_len = usize::try_from(cursor.u32_le()?).unwrap_or(0);
    limits.check_value(to_u64(vendor_len))?;
    cursor.skip(vendor_len)?;
    let count = u64::from(cursor.u32_le()?);
    limits.check_records(count)?;
    for _ in 0..count {
        let len = usize::try_from(cursor.u32_le()?).unwrap_or(0);
        limits.check_value(to_u64(len))?;
        let start = cursor.position();
        let raw = cursor.take(len)?;
        budget.spend()?;
        let text = String::from_utf8_lossy(raw);
        let (name, value) = match text.split_once('=') {
            Some((name, value)) => (name.to_owned(), value.to_owned()),
            None => (text.into_owned(), String::new()),
        };
        fields.push(TagField {
            id: name.to_ascii_uppercase(),
            name: readable(&name),
            text: Some(value),
            binary: None,
            source: ByteRange::new(to_u64(at + start), to_u64(len)),
        });
    }
    Ok(())
}

/// Read an Ogg file's identification and comment headers.
///
/// # Errors
///
/// Returns [`RecordError::Malformed`] when a page has an invalid capture
/// pattern, [`RecordError::Truncated`] when a page runs past the source, and
/// [`RecordError::LimitExceeded`] when an Ogg or comment limit is exceeded.
pub fn read_ogg(data: &[u8], limits: &Limits) -> Result<ContainerTags> {
    if slice(data, 0, 4, "ogg signature")? != b"OggS" {
        return Err(RecordError::malformed("ogg signature", 0));
    }
    if data.len() < 27 {
        return Err(RecordError::truncated(
            "ogg page header",
            0,
            27,
            to_u64(data.len()),
        ));
    }
    let mut budget = RecordBudget::new(limits);
    let mut out = ContainerTags::default();
    let mut at = 0usize;
    let mut pages = 0u64;
    let mut packet = Vec::new();
    let mut packet_index = 0usize;
    let mut header_bytes = 0usize;
    let mut headers_complete = false;
    'pages: while at + 27 <= data.len() && pages < MAX_BOXES {
        let header = slice(data, at, 27, "ogg page header")?;
        if header.get(..4) != Some(b"OggS".as_slice()) {
            return Err(RecordError::malformed("ogg page header", to_u64(at)));
        }
        let segments = usize::from(header[26]);
        let table = slice(data, at + 27, segments, "ogg segment table")?;
        let body_len: usize = table.iter().map(|value| usize::from(*value)).sum();
        limits.check_value(to_u64(body_len))?;
        let body = slice(data, at + 27 + segments, body_len, "ogg page body")?;
        at = at
            .saturating_add(27)
            .saturating_add(segments)
            .saturating_add(body_len);
        pages += 1;
        let mut body_at = 0usize;
        for segment in table {
            let segment_len = usize::from(*segment);
            let Some(segment_bytes) = body.get(body_at..body_at.saturating_add(segment_len)) else {
                break 'pages;
            };
            body_at += segment_len;
            if packet_index < 2 {
                header_bytes = header_bytes.saturating_add(segment_len);
                if header_bytes > MAX_OGG_HEADER_BYTES {
                    return Err(RecordError::LimitExceeded {
                        limit: "oggHeaderBytes",
                        allowed: to_u64(MAX_OGG_HEADER_BYTES),
                        requested: to_u64(header_bytes),
                    });
                }
                packet.extend_from_slice(segment_bytes);
            }
            if *segment < 255 {
                if packet_index < 2 {
                    let comment_packet_read = read_ogg_header_packet(
                        &packet,
                        packet_index,
                        &mut out,
                        limits,
                        &mut budget,
                    )?;
                    if out.stream.is_some() && comment_packet_read {
                        headers_complete = true;
                        break 'pages;
                    }
                }
                packet_index = packet_index.saturating_add(1);
                packet.clear();
            }
        }
    }
    if pages == MAX_BOXES && !headers_complete {
        return Err(RecordError::LimitExceeded {
            limit: "oggPages",
            allowed: MAX_BOXES,
            requested: MAX_BOXES.saturating_add(1),
        });
    }
    if out.stream.is_some() && !headers_complete {
        return Err(RecordError::truncated(
            "ogg vorbis comment header",
            to_u64(at),
            1,
            0,
        ));
    }
    if !headers_complete && at < data.len() && data.len().saturating_sub(at) < 27 {
        return Err(RecordError::truncated(
            "ogg page header",
            to_u64(at),
            27,
            to_u64(data.len().saturating_sub(at)),
        ));
    }
    Ok(out)
}

fn read_ogg_header_packet(
    packet: &[u8],
    packet_index: usize,
    out: &mut ContainerTags,
    limits: &Limits,
    budget: &mut RecordBudget,
) -> Result<bool> {
    #[cfg(test)]
    OGG_SCANNED_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(packet.len())));
    if packet_index == 0
        && packet.len() >= 30
        && packet.get(1..7) == Some(b"vorbis".as_slice())
        && packet[0] == 1
    {
        let channels = packet.get(11).copied().unwrap_or(0);
        let sample_rate = u32::from_le_bytes([
            packet.get(12).copied().unwrap_or(0),
            packet.get(13).copied().unwrap_or(0),
            packet.get(14).copied().unwrap_or(0),
            packet.get(15).copied().unwrap_or(0),
        ]);
        let nominal = u32::from_le_bytes([
            packet.get(20).copied().unwrap_or(0),
            packet.get(21).copied().unwrap_or(0),
            packet.get(22).copied().unwrap_or(0),
            packet.get(23).copied().unwrap_or(0),
        ]);
        out.stream = Some(ContainerStream {
            sample_rate,
            channels,
            bits_per_sample: 0,
            duration_ms: 0,
            bit_rate: nominal,
        });
    }
    if packet_index == 1 && packet.get(..7) == Some(b"\x03vorbis".as_slice()) {
        let body = packet.get(7..).unwrap_or_default();
        let mut fields = Vec::new();
        parse_vorbis_comment(body, 7, &mut fields, limits, budget)?;
        out.fields = fields;
        return Ok(true);
    }
    Ok(false)
}

/// Read a WAV file's format chunk and its information list.
///
/// # Errors
///
/// Returns [`RecordError::Malformed`] when the file is not a RIFF WAVE file.
pub fn read_wav(data: &[u8], limits: &Limits) -> Result<ContainerTags> {
    if slice(data, 0, 4, "riff signature")? != b"RIFF" || slice(data, 8, 4, "riff form")? != b"WAVE"
    {
        return Err(RecordError::malformed("riff signature", 0));
    }
    let riff_size = usize::try_from(u32::from_le_bytes(
        slice(data, 4, 4, "riff size")?
            .try_into()
            .map_err(|_| RecordError::malformed("riff size", 4))?,
    ))
    .map_err(|_| RecordError::malformed("riff size", 4))?;
    if riff_size < 4 {
        return Err(RecordError::malformed("riff size", 4));
    }
    let riff_end = 8usize
        .checked_add(riff_size)
        .ok_or_else(|| RecordError::malformed("riff size", 4))?;
    slice(data, 0, riff_end, "riff file")?;
    let riff_data = &data[..riff_end];

    let mut budget = RecordBudget::new(limits);
    let mut out = ContainerTags::default();
    let mut at = 12usize;
    let mut chunks = 0u64;
    let mut byte_rate = 0u32;
    let mut data_len = 0u64;
    while at < riff_end {
        if chunks == MAX_BOXES {
            return Err(RecordError::LimitExceeded {
                limit: "containerChunks",
                allowed: MAX_BOXES,
                requested: MAX_BOXES.saturating_add(1),
            });
        }
        chunks += 1;
        let header = slice(riff_data, at, 8, "riff chunk header")?;
        let id = [header[0], header[1], header[2], header[3]];
        let size = usize::try_from(u32::from_le_bytes([
            header[4], header[5], header[6], header[7],
        ]))
        .map_err(|_| RecordError::malformed("riff chunk size", to_u64(at + 4)))?;
        let padded_size = size
            .checked_add(size % 2)
            .ok_or_else(|| RecordError::malformed("riff chunk size", to_u64(at + 4)))?;
        let step = 8usize
            .checked_add(padded_size)
            .ok_or_else(|| RecordError::malformed("riff chunk size", to_u64(at + 4)))?;
        let next_at = at
            .checked_add(step)
            .ok_or_else(|| RecordError::malformed("riff chunk size", to_u64(at)))?;
        if next_at > riff_end {
            return Err(RecordError::truncated(
                "riff chunk",
                to_u64(at),
                to_u64(step),
                to_u64(riff_end.saturating_sub(at)),
            ));
        }
        let body = slice(riff_data, at + 8, size, "riff chunk")?;
        if &id == b"fmt " && body.len() >= 16 {
            let channels = body[2];
            let sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            byte_rate = u32::from_le_bytes([body[8], body[9], body[10], body[11]]);
            let bits = u16::from_le_bytes([body[14], body[15]]);
            out.stream = Some(ContainerStream {
                sample_rate,
                channels,
                bits_per_sample: bits,
                duration_ms: 0,
                bit_rate: byte_rate.saturating_mul(8),
            });
        } else if &id == b"data" {
            data_len = to_u64(size);
        } else if &id == b"LIST" && body.get(..4) == Some(b"INFO".as_slice()) {
            read_info_list(body, at + 8, &mut out.fields, limits, &mut budget)?;
        }
        at = next_at;
    }
    if let Some(stream) = out.stream.as_mut() {
        if byte_rate > 0 {
            stream.duration_ms = data_len.saturating_mul(1000) / u64::from(byte_rate);
        }
    }
    Ok(out)
}

fn read_info_list(
    body: &[u8],
    at: usize,
    fields: &mut Vec<TagField>,
    limits: &Limits,
    budget: &mut RecordBudget,
) -> Result<()> {
    let mut cursor = 4usize;
    let mut chunks = 0u64;
    while cursor < body.len() {
        if body.len().saturating_sub(cursor) < 8 {
            return Err(RecordError::truncated(
                "riff INFO chunk header",
                to_u64(at.saturating_add(cursor)),
                8,
                to_u64(body.len().saturating_sub(cursor)),
            ));
        }
        if chunks == MAX_BOXES {
            return Err(RecordError::LimitExceeded {
                limit: "riffInfoChunks",
                allowed: MAX_BOXES,
                requested: MAX_BOXES.saturating_add(1),
            });
        }
        chunks += 1;
        let header = slice(body, cursor, 8, "riff INFO chunk header")?;
        let id = String::from_utf8_lossy(&header[..4]).into_owned();
        let size = usize::try_from(u32::from_le_bytes([
            header[4], header[5], header[6], header[7],
        ]))
        .map_err(|_| RecordError::malformed("riff INFO chunk size", to_u64(at + cursor + 4)))?;
        limits.check_value(to_u64(size))?;
        let padded_size = size
            .checked_add(size % 2)
            .ok_or_else(|| RecordError::malformed("riff INFO chunk size", to_u64(at + cursor)))?;
        let step = 8usize
            .checked_add(padded_size)
            .ok_or_else(|| RecordError::malformed("riff INFO chunk size", to_u64(at + cursor)))?;
        let next_cursor = cursor.checked_add(step).ok_or_else(|| {
            RecordError::malformed("riff INFO chunk size", to_u64(at.saturating_add(cursor)))
        })?;
        if next_cursor > body.len() {
            return Err(RecordError::truncated(
                "riff INFO chunk",
                to_u64(at.saturating_add(cursor)),
                to_u64(step),
                to_u64(body.len().saturating_sub(cursor)),
            ));
        }
        let value = slice(body, cursor + 8, size, "riff INFO chunk value")?;
        budget.spend()?;
        fields.push(TagField {
            id: id.clone(),
            name: riff_info_name(&id),
            text: Some(
                String::from_utf8_lossy(value)
                    .trim_end_matches('\0')
                    .to_owned(),
            ),
            binary: None,
            source: ByteRange::new(to_u64(at + cursor), to_u64(size + 8)),
        });
        cursor = next_cursor;
    }
    Ok(())
}

fn riff_info_name(id: &str) -> String {
    match id {
        "INAM" => "Title",
        "IART" => "Artist",
        "IPRD" => "Album",
        "ICRD" => "Year",
        "IGNR" => "Genre",
        "ICMT" => "Comment",
        "ITRK" => "Track",
        "ISFT" => "Software",
        "ICOP" => "Copyright",
        _ => "Field",
    }
    .to_owned()
}

/// Read an MP4 file's metadata list and its duration.
///
/// # Errors
///
/// Returns [`RecordError::Malformed`] when the file carries no `ftyp` box and
/// [`RecordError::Truncated`] when a box runs past the end of the file.
pub fn read_mp4(data: &[u8], limits: &Limits) -> Result<ContainerTags> {
    if slice(data, 4, 4, "mp4 signature")? != b"ftyp" {
        return Err(RecordError::malformed("mp4 signature", 0));
    }
    let mut budget = RecordBudget::new(limits);
    let mut out = ContainerTags::default();
    walk_boxes(data, 0, data.len(), &mut out, limits, &mut budget, 0)?;
    Ok(out)
}

/// Walk one level of boxes. Depth is checked against the limit on entry, so
/// the recursion cannot run deeper than the configured ceiling.
fn walk_boxes(
    data: &[u8],
    start: usize,
    end: usize,
    out: &mut ContainerTags,
    limits: &Limits,
    budget: &mut RecordBudget,
    depth: u32,
) -> Result<()> {
    walk_boxes_inner(data, (start, end), out, limits, budget, depth, false)
}

fn walk_boxes_inner(
    data: &[u8],
    span: (usize, usize),
    out: &mut ContainerTags,
    limits: &Limits,
    budget: &mut RecordBudget,
    depth: u32,
    allow_zero_terminator: bool,
) -> Result<()> {
    let (start, end) = span;
    limits.check_depth(depth)?;
    let mut at = start;
    let mut seen = 0u64;
    while end.saturating_sub(at) >= 8 && seen < MAX_BOXES {
        seen += 1;
        let header = slice(data, at, 8, "mp4 box header")?;
        let mut size = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let kind = [header[4], header[5], header[6], header[7]];
        let mut body_at = at + 8;
        let mut header_size = 8usize;
        if size == 1 {
            if end.saturating_sub(at) < 16 {
                return Err(RecordError::truncated(
                    "mp4 extended box header",
                    to_u64(at),
                    16,
                    to_u64(end.saturating_sub(at)),
                ));
            }
            let extended = slice(data, at + 8, 8, "mp4 box size")?;
            size = u64::from_be_bytes([
                extended[0],
                extended[1],
                extended[2],
                extended[3],
                extended[4],
                extended[5],
                extended[6],
                extended[7],
            ]);
            body_at += 8;
            header_size = 16;
        } else if size == 0 {
            size = to_u64(end.saturating_sub(at));
        }
        let box_end = checked_mp4_box_end(at, end, size, header_size, "mp4 box")?;
        match &kind {
            b"udta" => {
                walk_boxes_inner(
                    data,
                    (body_at, box_end),
                    out,
                    limits,
                    budget,
                    depth + 1,
                    true,
                )?;
            }
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                walk_boxes(data, body_at, box_end, out, limits, budget, depth + 1)?;
            }
            b"meta" => {
                // The metadata box carries a four byte version and flags field
                // before its children.
                if box_end.saturating_sub(body_at) < 4 {
                    return Err(RecordError::truncated(
                        "mp4 metadata header",
                        to_u64(body_at),
                        4,
                        to_u64(box_end.saturating_sub(body_at)),
                    ));
                }
                walk_boxes(data, body_at + 4, box_end, out, limits, budget, depth + 1)?;
            }
            b"ilst" => read_ilst(data, body_at, box_end, out, limits, budget)?,
            b"mvhd" => read_mvhd(data, body_at, box_end, out),
            _ => {}
        }
        at = box_end.max(at + 8);
    }
    // Check at the next child boundary; four zero bytes can also be a valid
    // data-box locale field and must not be stripped from the child payload.
    if allow_zero_terminator
        && end.saturating_sub(at) == 4
        && data.get(at..end) == Some([0, 0, 0, 0].as_slice())
    {
        return Ok(());
    }
    finish_mp4_box_walk(at, end, seen, "mp4Boxes", "mp4 box header")?;
    Ok(())
}

fn finish_mp4_box_walk(
    at: usize,
    end: usize,
    seen: u64,
    limit: &'static str,
    context: &'static str,
) -> Result<()> {
    if at == end {
        return Ok(());
    }
    if end.saturating_sub(at) < 8 {
        return Err(RecordError::truncated(
            context,
            to_u64(at),
            8,
            to_u64(end.saturating_sub(at)),
        ));
    }
    if seen == MAX_BOXES {
        return Err(RecordError::LimitExceeded {
            limit,
            allowed: MAX_BOXES,
            requested: MAX_BOXES.saturating_add(1),
        });
    }
    Err(RecordError::truncated(
        context,
        to_u64(at),
        8,
        to_u64(end.saturating_sub(at)),
    ))
}

fn checked_mp4_box_end(
    at: usize,
    parent_end: usize,
    size: u64,
    header_size: usize,
    context: &'static str,
) -> Result<usize> {
    let available = parent_end.saturating_sub(at);
    if size < to_u64(header_size) {
        return Err(RecordError::malformed(context, to_u64(at)));
    }
    if size > to_u64(available) {
        return Err(RecordError::truncated(
            context,
            to_u64(at),
            size,
            to_u64(available),
        ));
    }
    let declared_size = size;
    let size = usize::try_from(declared_size).map_err(|_| {
        RecordError::truncated(context, to_u64(at), declared_size, to_u64(available))
    })?;
    at.checked_add(size).ok_or_else(|| {
        RecordError::truncated(context, to_u64(at), declared_size, to_u64(available))
    })
}

fn read_mvhd(data: &[u8], body_at: usize, end: usize, out: &mut ContainerTags) {
    let Some(body) = data.get(body_at..end) else {
        return;
    };
    if body.len() < 20 {
        return;
    }
    let version = body[0];
    let (timescale, duration) = if version == 1 && body.len() >= 32 {
        (
            u32::from_be_bytes([body[20], body[21], body[22], body[23]]),
            u64::from_be_bytes([
                body[24], body[25], body[26], body[27], body[28], body[29], body[30], body[31],
            ]),
        )
    } else {
        (
            u32::from_be_bytes([body[12], body[13], body[14], body[15]]),
            u64::from(u32::from_be_bytes([body[16], body[17], body[18], body[19]])),
        )
    };
    let duration_ms = if timescale == 0 {
        0
    } else {
        duration.saturating_mul(1000) / u64::from(timescale)
    };
    let stream = out.stream.get_or_insert(ContainerStream::default());
    stream.duration_ms = duration_ms;
}

fn read_ilst(
    data: &[u8],
    start: usize,
    end: usize,
    out: &mut ContainerTags,
    limits: &Limits,
    budget: &mut RecordBudget,
) -> Result<()> {
    let mut at = start;
    let mut seen = 0u64;
    while end.saturating_sub(at) >= 8 && seen < MAX_BOXES {
        seen += 1;
        let header = slice(data, at, 8, "mp4 item header")?;
        let declared = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let mut header_size = 8usize;
        let mut size = declared;
        if declared == 1 {
            if end.saturating_sub(at) < 16 {
                return Err(RecordError::truncated(
                    "mp4 extended item header",
                    to_u64(at),
                    16,
                    to_u64(end.saturating_sub(at)),
                ));
            }
            let extended = slice(data, at + 8, 8, "mp4 item size")?;
            size = u64::from_be_bytes([
                extended[0],
                extended[1],
                extended[2],
                extended[3],
                extended[4],
                extended[5],
                extended[6],
                extended[7],
            ]);
            header_size = 16;
        } else if declared == 0 {
            size = to_u64(end.saturating_sub(at));
        }
        let item_end = checked_mp4_box_end(at, end, size, header_size, "mp4 item")?;
        // An item name may start with the byte 0xA9, which is not valid UTF-8,
        // so the four bytes decode one byte per character.
        let id: String = header
            .get(4..8)
            .unwrap_or_default()
            .iter()
            .map(|byte| char::from(*byte))
            .collect();
        read_data_boxes(
            data,
            at + header_size,
            item_end,
            &id,
            limits,
            budget,
            &mut out.fields,
        )?;
        at = item_end.max(at + 8);
    }
    finish_mp4_box_walk(at, end, seen, "mp4Items", "mp4 item header")?;
    Ok(())
}

fn read_data_boxes(
    data: &[u8],
    start: usize,
    end: usize,
    id: &str,
    limits: &Limits,
    budget: &mut RecordBudget,
    fields: &mut Vec<TagField>,
) -> Result<()> {
    let mut at = start;
    let mut seen = 0u64;
    while end.saturating_sub(at) >= 8 && seen < MAX_BOXES {
        seen += 1;
        let header = slice(data, at, 8, "mp4 child box header")?;
        let declared = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let mut header_size = 8usize;
        let mut size = declared;
        if declared == 1 {
            if end.saturating_sub(at) < 16 {
                return Err(RecordError::truncated(
                    "mp4 extended child box header",
                    to_u64(at),
                    16,
                    to_u64(end.saturating_sub(at)),
                ));
            }
            let extended = slice(data, at + 8, 8, "mp4 child box size")?;
            size = u64::from_be_bytes([
                extended[0],
                extended[1],
                extended[2],
                extended[3],
                extended[4],
                extended[5],
                extended[6],
                extended[7],
            ]);
            header_size = 16;
        } else if declared == 0 {
            size = to_u64(end.saturating_sub(at));
        }
        let box_end = checked_mp4_box_end(at, end, size, header_size, "mp4 child box")?;
        if header.get(4..8) != Some(b"data".as_slice()) {
            at = box_end;
            continue;
        }
        if size < to_u64(header_size.saturating_add(8)) {
            return Err(RecordError::malformed("mp4 data box", to_u64(at)));
        }
        let metadata_at = at + header_size;
        let metadata = slice(data, metadata_at, 8, "mp4 data metadata")?;
        let kind = u32::from_be_bytes([metadata[0], metadata[1], metadata[2], metadata[3]]);
        let payload_len_u64 = size - to_u64(header_size.saturating_add(8));
        let payload_len = usize::try_from(payload_len_u64).map_err(|_| {
            RecordError::truncated(
                "mp4 data payload",
                to_u64(metadata_at + 8),
                payload_len_u64,
                to_u64(end.saturating_sub(metadata_at + 8)),
            )
        })?;
        limits.check_value(to_u64(payload_len))?;
        let payload = slice(data, metadata_at + 8, payload_len, "mp4 data payload")?;
        let source = ByteRange::new(to_u64(at), size);
        let name = mp4_name(id);
        budget.spend()?;
        let parsed = match kind {
            1 => TagField {
                id: id.to_owned(),
                name,
                text: Some(String::from_utf8_lossy(payload).into_owned()),
                binary: None,
                source,
            },
            13 | 14 => TagField {
                id: id.to_owned(),
                name,
                text: None,
                binary: Some(payload.to_vec()),
                source,
            },
            _ => TagField {
                id: id.to_owned(),
                name,
                text: Some(integer_text(payload)),
                binary: None,
                source,
            },
        };
        fields.push(parsed);
        at = box_end;
    }
    finish_mp4_box_walk(at, end, seen, "mp4DataBoxes", "mp4 child box header")?;
    Ok(())
}

fn integer_text(payload: &[u8]) -> String {
    let mut value = 0u64;
    for byte in payload.iter().take(8) {
        value = (value << 8) | u64::from(*byte);
    }
    value.to_string()
}

fn mp4_name(id: &str) -> String {
    match id {
        "\u{a9}nam" => "Title",
        "\u{a9}ART" => "Artist",
        "aART" => "Album artist",
        "\u{a9}alb" => "Album",
        "\u{a9}day" => "Year",
        "trkn" => "Track",
        "disk" => "Disc",
        "\u{a9}gen" | "gnre" => "Genre",
        "\u{a9}wrt" => "Composer",
        "\u{a9}cmt" => "Comment",
        "\u{a9}too" => "Encoder",
        "cprt" => "Copyright",
        "covr" => "Cover art",
        _ => "Field",
    }
    .to_owned()
}

fn readable(name: &str) -> String {
    match name.to_ascii_uppercase().as_str() {
        "TITLE" => "Title",
        "ARTIST" => "Artist",
        "ALBUM" => "Album",
        "ALBUMARTIST" => "Album artist",
        "DATE" => "Year",
        "TRACKNUMBER" => "Track",
        "DISCNUMBER" => "Disc",
        "GENRE" => "Genre",
        "COMPOSER" => "Composer",
        "COMMENT" => "Comment",
        "COPYRIGHT" => "Copyright",
        _ => "Field",
    }
    .to_owned()
}

#[cfg(test)]
thread_local! {
    static OGG_SCANNED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod ogg_tests {
    use super::{read_ogg, OGG_SCANNED_BYTES};
    use crate::limits::Limits;

    fn page(sequence: u32, packet: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(28 + packet.len());
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push(0);
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&7u32.to_le_bytes());
        out.extend_from_slice(&sequence.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.push(1);
        out.push(u8::try_from(packet.len()).expect("short packet"));
        out.extend_from_slice(packet);
        out
    }

    #[test]
    fn empty_pages_do_not_rescan_the_header_payload() {
        let mut identification = vec![0; 30];
        identification[0] = 1;
        identification[1..7].copy_from_slice(b"vorbis");
        identification[11] = 2;
        identification[12..16].copy_from_slice(&48_000u32.to_le_bytes());

        let mut comment = b"\x03vorbis".to_vec();
        comment.extend_from_slice(&0u32.to_le_bytes());
        comment.extend_from_slice(&1u32.to_le_bytes());
        let value = "x".repeat(400);
        let field = format!("TITLE={value}");
        comment.extend_from_slice(&u32::try_from(field.len()).unwrap().to_le_bytes());
        comment.extend_from_slice(field.as_bytes());
        let split_at = 255;

        let mut bytes = page(0, &identification);
        bytes.extend(page(1, &comment[..split_at]));
        bytes.reserve(27 * 65_000);
        for _ in 0..65_000 {
            bytes.extend_from_slice(b"OggS");
            bytes.extend_from_slice(&[0; 22]);
            bytes.push(0);
        }
        bytes.extend(page(65_002, &comment[split_at..]));
        OGG_SCANNED_BYTES.with(|count| count.set(0));
        let tags = read_ogg(&bytes, &Limits::default()).expect("Ogg pages");
        assert_eq!(tags.stream.map(|stream| stream.sample_rate), Some(48_000));
        assert_eq!(
            tags.fields.first().and_then(|field| field.text.as_deref()),
            Some(value.as_str())
        );
        assert_eq!(
            OGG_SCANNED_BYTES.with(std::cell::Cell::get),
            identification.len() + comment.len()
        );
    }

    #[test]
    fn packet_headers_split_across_pages_still_read_stream_and_comments() {
        let mut identification = vec![0; 30];
        identification[0] = 1;
        identification[1..7].copy_from_slice(b"vorbis");
        identification[11] = 2;
        identification[12..16].copy_from_slice(&48_000u32.to_le_bytes());
        let mut comment = b"\x03vorbis".to_vec();
        comment.extend_from_slice(&0u32.to_le_bytes());
        comment.extend_from_slice(&1u32.to_le_bytes());
        comment.extend_from_slice(&10u32.to_le_bytes());
        comment.extend_from_slice(b"TITLE=Song");
        let mut bytes = page(0, &identification);
        bytes.extend(page(1, &comment));
        OGG_SCANNED_BYTES.with(|count| count.set(0));
        let tags = read_ogg(&bytes, &Limits::default()).expect("Vorbis headers");
        assert_eq!(
            tags.stream
                .map(|stream| (stream.sample_rate, stream.channels)),
            Some((48_000, 2))
        );
        assert_eq!(
            tags.fields.first().map(|field| field.name.as_str()),
            Some("Title")
        );
        assert_eq!(
            tags.fields.first().and_then(|field| field.text.as_deref()),
            Some("Song")
        );
        assert_eq!(
            OGG_SCANNED_BYTES.with(std::cell::Cell::get),
            identification.len() + comment.len()
        );
    }

    #[test]
    fn vorbis_comment_record_limit_is_not_silently_dropped() {
        let mut identification = vec![0; 30];
        identification[0] = 1;
        identification[1..7].copy_from_slice(b"vorbis");
        identification[11] = 2;
        identification[12..16].copy_from_slice(&48_000u32.to_le_bytes());

        let mut comment = b"\x03vorbis".to_vec();
        comment.extend_from_slice(&0u32.to_le_bytes());
        comment.extend_from_slice(&1u32.to_le_bytes());
        comment.extend_from_slice(&10u32.to_le_bytes());
        comment.extend_from_slice(b"TITLE=Song");
        let mut bytes = page(0, &identification);
        bytes.extend(page(1, &comment));
        let limits = Limits {
            max_records: 0,
            ..Limits::default()
        };

        assert!(matches!(
            read_ogg(&bytes, &limits),
            Err(crate::error::RecordError::LimitExceeded {
                limit: "maxRecords",
                ..
            })
        ));
    }
}
